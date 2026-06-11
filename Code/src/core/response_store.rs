use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::error;

use super::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    pub request: Value,
    pub response: Value,
}

pub fn prepare_responses_payload(config: &Config, original: &Value) -> Result<(Value, bool)> {
    let should_store = original
        .get("store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut upstream = original.clone();

    if let Some(previous_response_id) = original
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
    {
        let mut seen = HashSet::new();
        let chain = load_response_chain(config, previous_response_id, &mut seen)?;
        let mut input = Vec::new();

        for stored in chain {
            input.extend(input_items(stored.request.get("input")));
            input.extend(response_output_items(&stored.response));
        }
        input.extend(input_items(original.get("input")));

        upstream["input"] = Value::Array(input);
        if let Some(obj) = upstream.as_object_mut() {
            obj.remove("previous_response_id");
        }
    }

    // ChatGPT's Codex backend rejects store=true. Store locally instead.
    upstream["store"] = json!(false);

    if let Some(input) = upstream.get_mut("input") {
        strip_reasoning_items(input);
    }

    Ok((upstream, should_store))
}

fn strip_reasoning_items(input: &mut Value) {
    if let Some(items) = input.as_array_mut() {
        items.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
    }
}

pub fn store_response(config: &Config, request: Value, response: Value) -> Result<()> {
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("response has no string id"))?;
    let path = response_path(config, id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create response store dir {:?}", parent))?;
    }

    let stored = StoredResponse { request, response };
    let json = serde_json::to_vec_pretty(&stored)?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {:?}", path))?;
    Ok(())
}

pub fn load_response(config: &Config, id: &str) -> Result<Option<Value>> {
    let path = response_path(config, id)?;
    if !path.exists() {
        return Ok(None);
    }

    let stored = read_stored_response(&path)?;
    Ok(Some(stored.response))
}

pub fn delete_response(config: &Config, id: &str) -> Result<bool> {
    let path = response_path(config, id)?;
    if !path.exists() {
        return Ok(false);
    }

    std::fs::remove_file(&path).with_context(|| format!("failed to delete {:?}", path))?;
    Ok(true)
}

pub fn prune_expired(config: &Config) -> usize {
    let max_age_secs = config.response_store_max_age_secs;
    if max_age_secs == 0 {
        return 0;
    }
    let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(max_age_secs)) else {
        return 0;
    };
    let Ok(read_dir) = std::fs::read_dir(&config.response_store_dir) else {
        return 0;
    };

    let mut removed = 0;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            let expired = entry
                .metadata()
                .and_then(|md| md.modified())
                .map(|modified| modified < cutoff)
                .unwrap_or(false);
            if expired {
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(e) => error!("response_store: failed to prune {:?}: {}", path, e),
                }
            }
        }
    }
    removed
}

pub fn output_item_done_entry(event: &Value) -> Option<(u64, Value)> {
    if event.get("type").and_then(Value::as_str) != Some("response.output_item.done") {
        return None;
    }
    let item = event.get("item")?.clone();
    let index = event
        .get("output_index")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some((index, item))
}

pub fn fill_output_from_items(response: &mut Value, items: &[(u64, Value)]) {
    let already_populated = response
        .get("output")
        .and_then(Value::as_array)
        .map(|out| !out.is_empty())
        .unwrap_or(false);
    if already_populated || items.is_empty() {
        return;
    }
    let mut ordered: Vec<&(u64, Value)> = items.iter().collect();
    ordered.sort_by_key(|(index, _)| *index);
    let output: Vec<Value> = ordered.into_iter().map(|(_, item)| item.clone()).collect();
    if let Some(obj) = response.as_object_mut() {
        obj.insert("output".to_string(), Value::Array(output));
    }
}

pub fn response_from_sse_bytes(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut response = None;
    let mut items: Vec<(u64, Value)> = Vec::new();

    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };

        if let Some(entry) = output_item_done_entry(&event) {
            items.push(entry);
        } else if event.get("type").and_then(Value::as_str) == Some("response.completed") {
            if let Some(completed) = event.get("response") {
                response = Some(completed.clone());
            }
        }
    }

    let mut response = response?;
    fill_output_from_items(&mut response, &items);
    Some(response)
}

fn load_response_chain(
    config: &Config,
    id: &str,
    seen: &mut HashSet<String>,
) -> Result<Vec<StoredResponse>> {
    if !seen.insert(id.to_string()) {
        return Err(anyhow!("cycle in previous_response_id chain at {id}"));
    }

    let path = response_path(config, id)?;
    if !path.exists() {
        return Err(anyhow!("stored response not found: {id}"));
    }

    let stored = read_stored_response(&path)?;
    let mut chain = if let Some(previous_response_id) = stored
        .request
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
    {
        load_response_chain(config, previous_response_id, seen)?
    } else {
        Vec::new()
    };

    chain.push(stored);
    Ok(chain)
}

fn read_stored_response(path: &PathBuf) -> Result<StoredResponse> {
    let bytes = std::fs::read(path).with_context(|| format!("failed to read {:?}", path))?;
    let stored = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse stored response {:?}", path))?;
    Ok(stored)
}

fn input_items(input: Option<&Value>) -> Vec<Value> {
    match input {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(_)) => input.cloned().into_iter().collect(),
        Some(Value::String(text)) => vec![json!({
            "role": "user",
            "content": text,
        })],
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![json!({
            "role": "user",
            "content": other.to_string(),
        })],
    }
}

fn response_output_items(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn response_path(config: &Config, id: &str) -> Result<PathBuf> {
    validate_response_id(id)?;
    Ok(config.response_store_dir.join(format!("{id}.json")))
}

fn validate_response_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(anyhow!("invalid response id"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(dir: PathBuf) -> Config {
        Config {
            codex_home: dir.clone(),
            chatgpt_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            model: "gpt-5.5".to_string(),
            user_instructions: None,
            request_log_dir: dir.clone(),
            response_store_dir: dir,
            response_store_max_age_secs: 0,
            request_log_max_bytes: 0,
            app_log_max_bytes: 0,
        }
    }

    #[test]
    fn prepare_forces_upstream_store_false() {
        let config = test_config(tempfile::tempdir().unwrap().path().to_path_buf());
        let payload = json!({ "store": true, "input": "hello" });

        let (upstream, should_store) = prepare_responses_payload(&config, &payload).unwrap();

        assert!(should_store);
        assert_eq!(upstream["store"], false);
    }

    #[test]
    fn previous_response_id_expands_local_history() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path().to_path_buf());
        store_response(
            &config,
            json!({"input": [{"role": "user", "content": "first"}], "store": true}),
            json!({"id": "resp_1", "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "second"}]}]}),
        )
        .unwrap();

        let (upstream, _) = prepare_responses_payload(
            &config,
            &json!({"previous_response_id": "resp_1", "input": [{"role": "user", "content": "third"}], "store": true}),
        )
        .unwrap();

        let input = upstream["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert!(upstream.get("previous_response_id").is_none());
        assert_eq!(upstream["store"], false);
    }

    #[test]
    fn delete_removes_stored_response() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path().to_path_buf());
        store_response(
            &config,
            json!({ "input": "hi", "store": true }),
            json!({ "id": "resp_del", "output": [] }),
        )
        .unwrap();

        assert!(delete_response(&config, "resp_del").unwrap());
        assert!(load_response(&config, "resp_del").unwrap().is_none());
        assert!(!delete_response(&config, "resp_del").unwrap());
        assert!(delete_response(&config, "../escape").is_err());
    }

    #[test]
    fn sse_reconstructs_output_from_output_item_done() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,",
            "\"item\":{\"type\":\"message\",\"role\":\"assistant\",",
            "\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,",
            "\"item\":{\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\",\"output\":[]}}\n\n",
            "data: [DONE]\n\n",
        );

        let response = response_from_sse_bytes(sse.as_bytes()).expect("should parse");
        let output = response["output"].as_array().expect("output array");
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["type"], "message");
    }
}
