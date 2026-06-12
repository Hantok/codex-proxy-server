use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::core::config::Config;
use crate::core::models::{ChatRequest, ResponseChoice, ResponseDelta, ResponseEvent};

pub async fn stream_chat_completions(
    config: &Config,
    request: ChatRequest,
) -> Result<mpsc::Receiver<Result<ResponseEvent>>> {
    let client = Client::new();
    let (tx, rx) = mpsc::channel(100);
    let config = config.clone();

    tokio::spawn(async move {
        // SOLUTION: Convert system messages to user messages with special formatting
        // ChatGPT Responses API has strict validation on instructions field
        // So we put system messages in the input array as user messages

        let mut input_messages = Vec::new();
        let is_raycast_request = request
            .messages
            .iter()
            .any(|msg| message_content_to_text(&msg.content).contains("<user-preferences>"));

        for msg in &request.messages {
            let content_text = message_content_to_text(&msg.content);
            match msg.role.as_str() {
                "system" => {
                    input_messages.push(json!({
                        "role": "user",
                        "content": format!("<system>\n{}\n</system>", content_text)
                    }));
                }
                "tool" => {
                    input_messages.push(json!({
                        "role": "assistant",
                        "content": format!("<tool_response>\n{}\n</tool_response>", content_text)
                    }));
                }
                "assistant" | "user" | "developer" => {
                    input_messages.push(json!({
                        "role": msg.role,
                        "content": content_text
                    }));
                }
                _ => {
                    input_messages.push(json!({
                        "role": "user",
                        "content": format!("<{}>\n{}\n</{}>", msg.role, content_text, msg.role)
                    }));
                }
            }
        }

        // Use the full base instructions from prompt.md
        use crate::core::client_common::BASE_INSTRUCTIONS;

        let mut instructions = if is_raycast_request {
            "You are a concise, helpful assistant. Answer the user's request directly.".to_string()
        } else {
            BASE_INSTRUCTIONS.to_string()
        };

        // Add user instructions from AGENTS.md if available
        if let Some(user_instructions) = &config.user_instructions {
            instructions.push_str("\n\n<user_instructions>\n\n");
            instructions.push_str(user_instructions);
            instructions.push_str("\n\n</user_instructions>");
        }

        println!("🔍 DEBUG - Processing {} messages", request.messages.len());
        println!(
            "🔍 DEBUG - Instructions length: {} characters",
            instructions.len()
        );
        println!(
            "🔍 DEBUG - Instructions preview: {}...",
            &instructions[..200.min(instructions.len())]
        );
        println!("🔍 DEBUG - Input messages: {}", input_messages.len());

        // Extract tools from the original request and map to ChatGPT Responses schema
        // Chat Completions format nests name under `function.name`; Responses expects top-level `name`
        let mapped_tools: Vec<Value> = request
            .tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function").unwrap_or(tool);
                let name = function.get("name")?.as_str()?.trim();
                if name.is_empty() || name == "null" {
                    return None;
                }
                if is_sensitive_client_tool(name) {
                    return None;
                }
                let parameters = function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                Some(json!({
                    "type": "function",
                    "name": name,
                    "parameters": normalize_tool_parameters(parameters),
                    "strict": true  // Add strict mode for better tool calling
                }))
            })
            .collect();

        let has_tools = !mapped_tools.is_empty();

        println!("🔍 DEBUG - Tools in request: {}", request.tools.len());
        println!("🔍 DEBUG - Mapped tools included: {}", mapped_tools.len());
        println!("🔍 DEBUG - Has valid tools: {}", has_tools);

        // Construct payload matching ChatGPT Responses API format
        let model_id = match request.model.as_str() {
            "gpt-5.5" => "gpt-5.5".to_string(),
            "gpt-5.4" => "gpt-5.4".to_string(),
            "gpt-5.4-mini" => "gpt-5.4-mini".to_string(),
            "gpt-5.3-codex-spark" => "gpt-5.3-codex-spark".to_string(),
            m if m.starts_with("gpt-5") => "gpt-5".to_string(),
            m => m.to_string(),
        };

        let mut payload = json!({
            "model": model_id,
            "instructions": instructions,  // Full instructions from prompt.md
            "input": input_messages,        // User/assistant messages only
            "store": false,        // CRITICAL: Must be false for ChatGPT Plus plan
            "stream": true
        });

        // Only add tools-related fields if we have valid tools
        if has_tools {
            payload["tools"] = json!(mapped_tools);
            // Keep auto selection; the Responses API accepts "auto" here
            payload["tool_choice"] = json!("auto");
            payload["parallel_tool_calls"] = json!(false);
            println!("🔍 DEBUG - Added mapped tools to payload");
        } else {
            println!("🔍 DEBUG - No tools added to payload");
        }

        println!(
            "🔍 DEBUG - Request payload keys: model, instructions, input, store, stream{}",
            if has_tools { ", tools" } else { "" }
        );

        // Get access token and account ID
        println!("🔑 Getting access token...");
        let access_token = match get_access_token(&config).await {
            Ok(token) => {
                println!(
                    "✅ Access token retrieved: {}...",
                    &token[..50.min(token.len())]
                );
                token
            }
            Err(e) => {
                println!("❌ Access token retrieval failed: {}", e);
                let _ = tx
                    .send(Err(anyhow::anyhow!("Access token retrieval failed: {}", e)))
                    .await;
                return;
            }
        };

        println!("🆔 Getting account ID...");
        let account_id = match get_account_id(&config).await {
            Ok(id) => {
                println!("✅ Account ID retrieved: {}", id);
                id
            }
            Err(e) => {
                println!("❌ Account ID retrieval failed: {}", e);
                let _ = tx
                    .send(Err(anyhow::anyhow!("Account ID retrieval failed: {}", e)))
                    .await;
                return;
            }
        };

        // Try the exact URL that working codex uses: base + codex + responses
        let url = "https://chatgpt.com/backend-api/codex/responses";
        println!("🌐 Making request to ChatGPT Responses API: {}", url);

        // CRITICAL: Use exact headers for ChatGPT Plus plan
        let session_id = uuid::Uuid::new_v4().to_string();
        println!("🔍 DEBUG - Headers:");
        println!(
            "  Authorization: Bearer {}...",
            &access_token[..50.min(access_token.len())]
        );
        println!("  chatgpt-account-id: {}", account_id);
        println!("  session_id: {}", session_id);

        let response = match client
            .post(url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("chatgpt-account-id", &account_id) // CRITICAL: Required for plan users
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("OpenAI-Beta", "responses=experimental") // CRITICAL: Responses API header
            .header("session_id", session_id) // CRITICAL: Session tracking
            .header("originator", "codex_cli_rs") // CRITICAL: Plan identifier
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                println!("✅ Got response with status: {}", resp.status());

                if resp.status().is_success() {
                    resp
                } else {
                    // CRITICAL: Capture response body for debugging 400 errors
                    let status = resp.status();
                    let response_body = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Failed to read response body".to_string());
                    println!("❌ Failed with status: {}", status);
                    println!("🔍 DEBUG - Response body: {}", response_body);

                    // Send properly formatted error response as SSE
                    // Transform specific error messages for better user experience
                    let user_friendly_message = if status.as_u16() == 429
                        && response_body.contains("usage_limit_reached")
                    {
                        "You've hit your usage limit. Upgrade to Pro (https://openai.com/chatgpt/pricing), or wait for limits to reset (every 5h and every week.).".to_string()
                    } else {
                        format!("Error: {} - {}", status, response_body)
                    };

                    let error_event = ResponseEvent {
                        id: format!("error-{}", uuid::Uuid::new_v4()),
                        object: "chat.completion.chunk".to_string(),
                        created: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64,
                        model: request.model.clone(),
                        choices: vec![ResponseChoice {
                            index: 0,
                            delta: ResponseDelta {
                                role: Some("assistant".to_string()),
                                content: Some(user_friendly_message),
                                tool_calls: None,
                            },
                            finish_reason: Some("error".to_string()),
                        }],
                    };
                    let _ = tx.send(Ok(error_event)).await;
                    return;
                }
            }
            Err(e) => {
                println!("❌ Request failed: {}", e);
                let _ = tx.send(Err(anyhow::anyhow!("Request failed: {}", e))).await;
                return;
            }
        };

        // Handle streaming response with proper SSE buffering
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();

        // Deduplication: Track last sent content
        let mut last_sent_content: Option<String> = None;

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    let error_event = ResponseEvent {
                        id: format!("error-{}", uuid::Uuid::new_v4()),
                        object: "chat.completion.chunk".to_string(),
                        created: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64,
                        model: request.model.clone(),
                        choices: vec![ResponseChoice {
                            index: 0,
                            delta: ResponseDelta {
                                role: Some("assistant".to_string()),
                                content: Some(format!("Stream error: {}", e)),
                                tool_calls: None,
                            },
                            finish_reason: Some("error".to_string()),
                        }],
                    };
                    let _ = tx.send(Ok(error_event)).await;
                    break;
                }
            };

            // Add chunk to buffer
            buffer.extend_from_slice(&chunk);

            // Process complete lines from buffer
            while let Some(line_end) = buffer.iter().position(|byte| *byte == b'\n') {
                let line_bytes: Vec<u8> = buffer.drain(..=line_end).collect();
                let line = String::from_utf8_lossy(&line_bytes)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();

                // Skip empty lines (SSE format requirement)
                if line.is_empty() {
                    continue;
                }

                // Process SSE data lines
                if let Some(data) = line.strip_prefix("data: ") {
                    let json_str = data.trim();

                    // Skip "[DONE]" marker
                    if json_str == "[DONE]" {
                        println!("🏁 Received [DONE] marker, ending stream");
                        return;
                    }

                    // Skip empty data lines
                    if json_str.is_empty() {
                        continue;
                    }

                    println!("🔍 DEBUG - Parsing JSON: {}", json_str);

                    match serde_json::from_str::<Value>(json_str) {
                        Ok(event_json) => {
                            // Convert to our ResponseEvent format
                            if let Some(response_event) = parse_sse_event(&event_json) {
                                // Deduplication logic
                                let mut should_send = true;
                                // Try to extract content from the event
                                let content = response_event
                                    .choices
                                    .first()
                                    .and_then(|choice| choice.delta.content.as_ref())
                                    .map(|s| s.trim().to_string());
                                // Only deduplicate non-empty content messages
                                if let Some(ref new_content) = content {
                                    if let Some(ref last_content) = last_sent_content {
                                        if !new_content.is_empty() && new_content == last_content {
                                            should_send = false;
                                        }
                                    }
                                }
                                if should_send {
                                    // Update last sent content if this is a non-empty message
                                    if let Some(ref new_content) = content {
                                        if !new_content.is_empty() {
                                            last_sent_content = Some(new_content.clone());
                                        }
                                    }
                                    if tx.send(Ok(response_event)).await.is_err() {
                                        // Channel closed, stop processing
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("⚠️  JSON parse error for line '{}': {}", json_str, e);
                            // Send a structured error response for malformed JSON
                            let error_event = ResponseEvent {
                                id: format!("error-{}", uuid::Uuid::new_v4()),
                                object: "chat.completion.chunk".to_string(),
                                created: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs() as i64,
                                model: request.model.clone(),
                                choices: vec![ResponseChoice {
                                    index: 0,
                                    delta: ResponseDelta {
                                        role: Some("assistant".to_string()),
                                        content: Some(format!("JSON parse error: {}", e)),
                                        tool_calls: None,
                                    },
                                    finish_reason: Some("error".to_string()),
                                }],
                            };
                            let _ = tx.send(Ok(error_event)).await;
                            continue;
                        }
                    }
                } else if let Some(event_type) = line.strip_prefix("event: ") {
                    // Handle SSE event types if needed
                    println!("📡 SSE Event type: {}", event_type);
                } else if let Some(event_id) = line.strip_prefix("id: ") {
                    // Handle SSE event IDs if needed
                    println!("🆔 SSE Event ID: {}", event_id);
                }
            }
        }
    });

    Ok(rx)
}

pub async fn send_responses_request(config: &Config, payload: Value) -> Result<reqwest::Response> {
    let client = Client::new();
    let access_token = get_access_token(config).await?;
    let account_id = get_account_id(config).await?;
    let session_id = uuid::Uuid::new_v4().to_string();

    let response = client
        .post("https://chatgpt.com/backend-api/codex/responses")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("chatgpt-account-id", account_id)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("OpenAI-Beta", "responses=experimental")
        .header("session_id", session_id)
        .header("originator", "codex_cli_rs")
        .json(&payload)
        .send()
        .await?;

    Ok(response)
}

fn message_content_to_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }

    if let Some(parts) = content.as_array() {
        let text_parts: Vec<String> = parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|text| text.as_str())
                    .or_else(|| part.get("content").and_then(|text| text.as_str()))
                    .map(|text| text.to_string())
            })
            .collect();

        if !text_parts.is_empty() {
            return text_parts.join("\n");
        }
    }

    content.to_string()
}

fn is_sensitive_client_tool(name: &str) -> bool {
    matches!(name, "location-get-current-location")
}

fn normalize_tool_parameters(mut parameters: Value) -> Value {
    if !parameters.is_object() {
        return json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        });
    }

    if parameters.get("type").is_none() {
        parameters["type"] = json!("object");
    }
    if parameters.get("properties").is_none() {
        parameters["properties"] = json!({});
    }
    add_strict_object_schema_flags(&mut parameters);
    parameters
}

fn add_strict_object_schema_flags(schema: &mut Value) {
    if let Some(schema_object) = schema.as_object_mut() {
        if schema_object.get("type").and_then(|value| value.as_str()) == Some("object") {
            schema_object
                .entry("properties".to_string())
                .or_insert_with(|| json!({}));
            schema_object.insert("additionalProperties".to_string(), json!(false));
        }

        if let Some(properties) = schema_object
            .get_mut("properties")
            .and_then(|value| value.as_object_mut())
        {
            for property_schema in properties.values_mut() {
                add_strict_object_schema_flags(property_schema);
            }
        }

        if let Some(items) = schema_object.get_mut("items") {
            add_strict_object_schema_flags(items);
        }
    }
}

fn parse_sse_event(event: &Value) -> Option<ResponseEvent> {
    println!(
        "🔍 DEBUG - Raw event type: {}",
        event
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    );

    // ChatGPT's Responses API emits function-call arguments as raw `delta`
    // strings. Do not surface those as assistant text; emit the completed tool
    // call from `response.output_item.done` instead.
    let event_type = event.get("type").and_then(|v| v.as_str());
    if matches!(
        event_type,
        Some("response.function_call_arguments.delta")
            | Some("response.function_call_arguments.done")
    ) {
        return None;
    }

    if event_type == Some("response.output_item.done") {
        if let Some(item) = event.get("item") {
            if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                let output_index = event
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("id").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));

                return Some(ResponseEvent {
                    id: event
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8])
                        }),
                    object: "chat.completion.chunk".to_string(),
                    created: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64,
                    model: event
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("gpt-5")
                        .to_string(),
                    choices: vec![ResponseChoice {
                        index: 0,
                        delta: ResponseDelta {
                            role: Some("assistant".to_string()),
                            content: None,
                            tool_calls: Some(serde_json::json!([{
                                "id": call_id,
                                "type": "function",
                                "index": output_index,
                                "function": {
                                    "name": name,
                                    "arguments": arguments
                                }
                            }])),
                        },
                        finish_reason: Some("tool_calls".to_string()),
                    }],
                });
            }
        }
    }

    // ChatGPT's Responses API has a different structure than OpenAI's
    // It might have fields like: response, message, content, etc.

    // Handle tool calls specifically
    if let Some(response) = event.get("response") {
        if let Some(output) = response.get("output").and_then(|o| o.as_array()) {
            // Look for function_call items in the output
            for item in output.iter() {
                if let Some(item_obj) = item.as_object() {
                    if let Some(item_type) = item_obj.get("type").and_then(|t| t.as_str()) {
                        // Handle function_call type items
                        if item_type == "function_call" {
                            // Extract tool call information
                            let name = item_obj
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = item_obj
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("")
                                .to_string();
                            let call_id = item_obj
                                .get("call_id")
                                .and_then(|id| id.as_str())
                                .unwrap_or(&format!("call_{}", uuid::Uuid::new_v4()))
                                .to_string();

                            // Create a tool call response
                            return Some(ResponseEvent {
                                id: event
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| {
                                        format!(
                                            "chatcmpl-{}",
                                            &uuid::Uuid::new_v4().to_string()[..8]
                                        )
                                    }),
                                object: "chat.completion.chunk".to_string(),
                                created: event
                                    .get("created")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or_else(|| {
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs()
                                            as i64
                                    }),
                                model: event
                                    .get("model")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("gpt-4")
                                    .to_string(),
                                choices: vec![ResponseChoice {
                                    index: 0,
                                    delta: ResponseDelta {
                                        role: Some("assistant".to_string()),
                                        content: None,
                                        tool_calls: Some(serde_json::json!([{
                                            "id": call_id,
                                            "type": "function",
                                            "index": 0,
                                            "function": {
                                                "name": name,
                                                "arguments": arguments
                                            }
                                        }])),
                                    },
                                    finish_reason: None,
                                }],
                            });
                        }
                    }
                }
            }
        }
    }

    // Try to extract content from various possible structures
    let content = extract_content_from_chatgpt_response(event);
    let model = event
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-4")
        .to_string();

    // Create OpenAI-compatible response
    Some(ResponseEvent {
        id: event
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8])),
        object: "chat.completion.chunk".to_string(),
        created: event
            .get("created")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            }),
        model,
        choices: if let Some(content) = content {
            vec![ResponseChoice {
                index: 0,
                delta: ResponseDelta {
                    role: Some("assistant".to_string()),
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason: event
                    .get("finish_reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }]
        } else {
            // Check if this is a finish event
            if event.get("finish_reason").is_some() {
                vec![ResponseChoice {
                    index: 0,
                    delta: ResponseDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason: event
                        .get("finish_reason")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                }]
            } else {
                vec![]
            }
        },
    })
}

fn extract_content_from_chatgpt_response(event: &Value) -> Option<String> {
    // Try multiple possible paths for content in ChatGPT's response format

    // Handle tool calls specifically
    if let Some(response) = event.get("response") {
        if let Some(output) = response.get("output").and_then(|o| o.as_array()) {
            // Look for function_call items in the output
            for item in output {
                if let Some(item_obj) = item.as_object() {
                    if let Some(item_type) = item_obj.get("type").and_then(|t| t.as_str()) {
                        // Handle function_call type items
                        if item_type == "function_call" {
                            // This is a tool call, we want to pass it through properly
                            // Return None here so the SSE parser can handle the tool call structure directly
                            return None;
                        }
                        // Handle message type items that might contain tool results
                        else if item_type == "message" {
                            if let Some(content) = item_obj.get("content").and_then(|c| {
                                c.as_array().and_then(|arr| {
                                    arr.first().and_then(|first| {
                                        first.get("text").and_then(|t| t.as_str())
                                    })
                                })
                            }) {
                                return Some(content.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Standard OpenAI format
    if let Some(choices) = event.get("choices").and_then(|c| c.as_array()) {
        if let Some(choice) = choices.first() {
            if let Some(delta) = choice.get("delta") {
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    return Some(content.to_string());
                }
            }
            if let Some(message) = choice.get("message") {
                if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                    return Some(content.to_string());
                }
            }
        }
    }

    // ChatGPT Responses API format - direct content field
    if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
        return Some(content.to_string());
    }

    // ChatGPT Responses API format - message field
    if let Some(message) = event.get("message") {
        if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
            return Some(content.to_string());
        }
        if let Some(content) = message.as_str() {
            return Some(content.to_string());
        }
    }

    // ChatGPT Responses API format - response field
    if let Some(response) = event.get("response") {
        if let Some(content) = response.get("content").and_then(|c| c.as_str()) {
            return Some(content.to_string());
        }
        if let Some(content) = response.as_str() {
            return Some(content.to_string());
        }
    }

    // ChatGPT Responses API format - text field
    if let Some(text) = event.get("text").and_then(|t| t.as_str()) {
        return Some(text.to_string());
    }

    // ChatGPT Responses API format - delta field
    if let Some(delta) = event.get("delta") {
        if let Some(text) = delta.as_str() {
            return Some(text.to_string());
        }
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            return Some(content.to_string());
        }
        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
            return Some(text.to_string());
        }
    }

    None
}

async fn get_access_token(config: &Config) -> Result<String> {
    use crate::login::lib::CodexAuth;

    let auth = CodexAuth::from_codex_home(&config.codex_home)?
        .ok_or_else(|| anyhow::anyhow!("No authentication found"))?;

    let token_data = auth.get_token_data().await?;
    Ok(token_data.access_token)
}

async fn get_account_id(config: &Config) -> Result<String> {
    use crate::login::lib::CodexAuth;

    let auth = CodexAuth::from_codex_home(&config.codex_home)?
        .ok_or_else(|| anyhow::anyhow!("No authentication found"))?;

    let token_data = auth.get_token_data().await?;
    token_data
        .account_id
        .ok_or_else(|| anyhow::anyhow!("No account ID found"))
}
