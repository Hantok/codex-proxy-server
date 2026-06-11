use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::{sse::Event, IntoResponse, Json, Response},
    routing::{any, get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Modules
mod core;
mod login;

use core::chat_completions;
use core::config::Config;
use core::models::{ChatRequest, Model, ModelList};
use login::lib::{AuthMode, CodexAuth, OPENAI_API_KEY_ENV_VAR};

// For CLI menu
use std::io::{self, Write};

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
}

#[tokio::main]
async fn main() {
    let dotenv_path = dotenvy::dotenv().ok();
    let logs_dir = core::config::default_log_base_dir();
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        eprintln!("Failed to create logs directory: {}", e);
    }
    // Initialize tracing with both console and file output
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "codex-proxy.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codex_proxy=info,tower_http=info".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_ansi(true)
                .with_target(false)
                .with_thread_ids(false)
                .with_thread_names(false),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .init();

    info!("=== Starting Codex Proxy Server ==="); // Codex Proxy Server
    if let Some(path) = &dotenv_path {
        info!("Loaded environment from {}", path.display());
    }
    info!("App log: {}/codex-proxy.log", logs_dir.display());
    info!("Timestamp: {}", chrono::Utc::now().to_rfc3339());

    // Check for --server flag to start directly
    let args: Vec<String> = std::env::args().collect();
    if args.contains(&"--server".to_string()) {
        if let Err(e) = run_server().await {
            error!("Failed to start server: {}", e);
        }
        return;
    }

    // Display CLI menu
    loop {
        display_menu();
        let choice = get_user_choice();

        match choice.as_str() {
            "1" => {
                if let Err(e) = run_server().await {
                    error!("Failed to start server: {}", e);
                }
            }
            "2" => {
                // Close all servers functionality
                if let Err(e) = close_all_servers().await {
                    error!("Failed to close servers: {}", e);
                }
            }
            "3" => {
                if let Err(e) = run_login().await {
                    error!("Login failed: {}", e);
                }
            }
            "4" => {
                if let Err(e) = refresh_token().await {
                    error!("Token refresh failed: {}", e);
                }
            }
            "5" => {
                println!("Exiting...");
                break;
            }
            "6" => {
                if let Err(e) = list_running_servers().await {
                    error!("Failed to list running servers: {}", e);
                }
            }
            _ => {
                println!("Invalid choice. Please try again.");
            }
        }
    }
}

async fn run_login() -> anyhow::Result<()> {
    info!("Starting login process");
    let home_dir =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let codex_home = home_dir.join(".codex");
    let opencode_home = home_dir.join(".opencode");
    let codex_auth_path = codex_home.join("auth.json");
    let opencode_auth_path = opencode_home.join("auth.json");

    // Try to read or create in .codex first
    std::fs::create_dir_all(&codex_home)?;
    println!("Codex home directory: {:?}", codex_home);
    println!("Expected auth file path: {:?}", codex_auth_path);

    let mut used_opencode = false;
    let login_result = login::lib::login_with_chatgpt(&codex_home, false).await;
    if login_result.is_err() || !codex_auth_path.exists() {
        // If failed or file not created, try .opencode
        println!("Could not create or find auth.json in .codex, switching to .opencode directory (Opencode integration)...");
        std::fs::create_dir_all(&opencode_home)?;
        let login_result2 = login::lib::login_with_chatgpt(&opencode_home, false).await;
        if login_result2.is_err() || !opencode_auth_path.exists() {
            // Third fallback: create ./local_auth directory in current working directory
            let local_auth_dir = std::env::current_dir()?.join("local_auth");
            std::fs::create_dir_all(&local_auth_dir)?;
            let local_auth_path = local_auth_dir.join("auth.json");
            println!("Could not create or find auth.json in .codex or .opencode, switching to ./local_auth directory (local fallback)...");
            let login_result3 = login::lib::login_with_chatgpt(&local_auth_dir, false).await;
            if login_result3.is_err() || !local_auth_path.exists() {
                return Err(anyhow::anyhow!("Login failed: Could not create auth.json in .codex, .opencode, or ./local_auth directory."));
            }
            println!("Auth file created successfully at: {:?} (local fallback, move to ~/.codex or ~/.opencode for best compatibility)", local_auth_path);
            println!("WARNING: Using local fallback directory for authentication. Move auth.json to ~/.codex or ~/.opencode for best compatibility and Opencode integration.");
            return Ok(());
        }
        used_opencode = true;
    }

    if used_opencode {
        println!(
            "Auth file created successfully at: {:?} (Opencode integration)",
            opencode_auth_path
        );
    } else {
        println!(
            "Auth file created successfully at: {:?} (Codex Proxy Server)",
            codex_auth_path
        );
    }

    info!("Login successful");
    println!("Login completed!");
    Ok(())
}

fn display_menu() {
    println!("\n=== Codex Proxy Server===");
    println!("1. Run server");
    println!("2. Close all servers");
    println!("3. Login");
    println!("4. Refresh token");
    println!("5. Exit");
    println!("6. List running servers");
    print!("Please select an option (1-6): ");
    io::stdout().flush().unwrap();
}

fn get_user_choice() -> String {
    let mut choice = String::new();
    io::stdin()
        .read_line(&mut choice)
        .expect("Failed to read input");
    choice.trim().to_string()
}

async fn run_server() -> anyhow::Result<()> {
    info!("Starting Codex Proxy Server");

    // Load configuration
    let config = match Config::load() {
        Ok(config) => {
            info!("Configuration loaded successfully");
            Arc::new(config)
        }
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            return Err(e);
        }
    };

    // Check authentication
    match check_authentication(&config).await {
        Ok(_) => info!("Authentication check passed"),
        Err(e) => {
            error!("Authentication check failed: {}", e);
            error!("Please use the 'Login' option in the CLI menu. This will enable Opencode and other integrations.");
            return Err(e);
        }
    }

    // Create app state
    let app_state = AppState { config };

    info!(
        "Request/response logs: {:?} (override with CODEX_PROXY_REQUEST_LOG_DIR)",
        app_state.config.request_log_dir
    );
    info!(
        "Response store: {:?}, retention {} days (override with CODEX_PROXY_STORE_DIR / CODEX_PROXY_STORE_MAX_AGE_DAYS)",
        app_state.config.response_store_dir,
        app_state.config.response_store_max_age_secs / (24 * 60 * 60)
    );
    core::log_rotation::spawn(app_state.config.clone());

    // Create router
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/chat/completions", post(chat_completions_handler)) // Keep legacy root for compatibility
        .route("/responses", post(responses_handler))
        .route(
            "/responses/:response_id",
            get(get_response_handler).delete(delete_response_handler),
        )
        .route("/v1/responses", post(responses_handler))
        .route(
            "/v1/responses/:response_id",
            get(get_response_handler).delete(delete_response_handler),
        )
        .route("/v1/models", get(models_handler))
        .route("/v1/*path", any(openai_passthrough_handler))
        .route("/health", get(health_handler))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(axum::middleware::from_fn_with_state(
            app_state.config.clone(),
            core::request_logger::log_request_response,
        ))
        .with_state(app_state);

    let ipv4_addr = SocketAddr::from(([127, 0, 0, 1], 5011));
    let ipv6_addr = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 5011));
    let ipv4_listener = tokio::net::TcpListener::bind(ipv4_addr).await?;
    let ipv6_listener = tokio::net::TcpListener::bind(ipv6_addr).await;

    info!("Server listening on {}", ipv4_addr);
    println!("Server is running. Press Ctrl+C to stop.");

    match ipv6_listener {
        Ok(ipv6_listener) => {
            info!("Server listening on {}", ipv6_addr);
            let ipv4_server = axum::serve(ipv4_listener, app.clone());
            let ipv6_server = axum::serve(ipv6_listener, app);

            tokio::select! {
                result = ipv4_server => result?,
                result = ipv6_server => result?,
            }
        }
        Err(e) => {
            warn!("Could not listen on {}: {}", ipv6_addr, e);
            axum::serve(ipv4_listener, app).await?;
        }
    }
    Ok(())
}

async fn refresh_token() -> anyhow::Result<()> {
    println!("Refreshing token...");

    // Load configuration
    let config = Config::load()?;

    // Get the codex auth
    let codex_auth = match CodexAuth::from_codex_home(&config.codex_home) {
        Ok(Some(auth)) => auth,
        _ => {
            return Err(anyhow::anyhow!("No authentication found. Please use the 'Login' option in the CLI menu. This enables Opencode and other integrations."));
        }
    };

    // Get token data which will automatically refresh if needed
    let token_data = match codex_auth.get_token_data().await {
        Ok(data) => data,
        Err(_) => {
            if codex_auth.mode == AuthMode::ApiKey {
                info!("Authentication successful using OpenAI API key");
                return Ok(());
            }
            return Err(anyhow::anyhow!("No authentication found. Please use the 'Login' option in the CLI menu. This enables Opencode and other integrations."));
        }
    };

    println!("Token refreshed successfully!");
    match &token_data.account_id {
        Some(account_id) => println!("Account ID: {}", account_id),
        None => println!("Account ID: None"),
    }

    Ok(())
}

async fn close_all_servers() -> anyhow::Result<()> {
    println!("Closing all servers (system-wide)...");
    let mut closed = 0;
    for port in 5011..=5020 {
        let pids = get_pids_for_port(port);
        for pid in pids {
            if kill_pid(pid) {
                println!("Killed server on port {} (PID {})", port, pid);
                closed += 1;
            }
        }
    }
    println!("Closed {} running server(s) on ports 5011-5020.", closed);
    Ok(())
}

// Get PIDs listening on a port
fn get_pids_for_port(port: u16) -> Vec<u32> {
    #[cfg(target_family = "unix")]
    {
        use std::process::Command;
        let output = Command::new("lsof")
            .arg("-ti")
            .arg(format!(":{}", port))
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .filter_map(|line| line.trim().parse::<u32>().ok())
                .collect()
        } else {
            vec![]
        }
    }
    #[cfg(target_family = "windows")]
    {
        use std::process::Command;
        let output = Command::new("netstat").arg("-ano").output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .filter_map(|line| {
                    if line.contains(&format!(":{}", port)) {
                        line.split_whitespace().last()?.parse::<u32>().ok()
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![]
        }
    }
}

// Kill a process by PID
fn kill_pid(pid: u32) -> bool {
    #[cfg(target_family = "unix")]
    {
        use std::process::Command;
        let status = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        status.map(|s| s.success()).unwrap_or(false)
    }
    #[cfg(target_family = "windows")]
    {
        use std::process::Command;
        let status = Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/F")
            .status();
        status.map(|s| s.success()).unwrap_or(false)
    }
}

// Utility: Check if a port is in use (cross-platform)
fn is_port_in_use(port: u16) -> bool {
    #[cfg(target_family = "unix")]
    {
        use std::process::Command;
        // Try lsof first
        let lsof_output = Command::new("lsof")
            .arg("-i")
            .arg(format!(":{}", port))
            .output();
        if let Ok(out) = lsof_output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains(&format!(":{}", port)) {
                return true;
            }
        } else {
            eprintln!("lsof failed for port {}", port);
        }
        // Fallback to netstat
        let netstat_output = Command::new("netstat").arg("-an").output();
        if let Ok(out) = netstat_output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains(&format!(":{}", port)) {
                return true;
            }
        } else {
            eprintln!("netstat failed for port {}", port);
        }
        false
    }
    #[cfg(target_family = "windows")]
    {
        use std::process::Command;
        let output = Command::new("netstat").arg("-ano").output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains(&format!(":{}", port)) {
                return true;
            }
        } else {
            eprintln!("netstat failed for port {}", port);
        }
        false
    }
}

async fn list_running_servers() -> anyhow::Result<()> {
    println!("Checking ports 5011-5020 for running servers...");
    let mut found = false;
    for port in 5011..=5020 {
        if is_port_in_use(port) {
            println!("Port {}: RUNNING", port);
            found = true;
        }
    }
    if !found {
        println!("No running servers found on ports 5011-5020.");
    }
    Ok(())
}

async fn check_authentication(config: &Config) -> anyhow::Result<()> {
    info!(
        "Checking authentication in directory: {:?}",
        &config.codex_home
    );
    let auth_file_path = config.codex_home.join("auth.json");
    info!("Looking for auth file at: {:?}", auth_file_path);

    if auth_file_path.exists() {
        info!("Auth file found!");
        // Try to read the file to check if it's valid
        match std::fs::read_to_string(&auth_file_path) {
            Ok(_content) => {
                // Auth file content preview removed for security
            }
            Err(e) => {
                error!("Error reading auth file: {}", e);
                return Err(anyhow::anyhow!("Failed to read auth file: {}", e));
            }
        }
    } else {
        warn!("Auth file not found!");
        // List files in the directory to see what's there
        if let Ok(entries) = std::fs::read_dir(&config.codex_home) {
            info!("Files in codex home directory:");
            for entry in entries.flatten() {
                info!("  - {}", entry.file_name().to_string_lossy());
            }
        }

        // Check if we're in .codex or .opencode and provide specific guidance
        if let Some(home_dir) = dirs::home_dir() {
            let codex_path = home_dir.join(".codex");
            let opencode_path = home_dir.join(".opencode");

            if config.codex_home == codex_path {
                info!("Looking in .codex directory. Checking if auth file exists in .opencode...");
                let opencode_auth = opencode_path.join("auth.json");
                if opencode_auth.exists() {
                    info!("Found auth file in .opencode directory. Consider moving it to .codex for better compatibility.");
                }
            } else if config.codex_home == opencode_path {
                info!("Looking in .opencode directory. Checking if auth file exists in .codex...");
                let codex_auth = codex_path.join("auth.json");
                if codex_auth.exists() {
                    info!("Found auth file in .codex directory. Using that instead.");
                }
            }
        }
    }

    let codex_auth = match CodexAuth::from_codex_home(&config.codex_home) {
        Ok(Some(auth)) => auth,
        _ => {
            return Err(anyhow::anyhow!("No authentication found. Please use the 'Login' option in the CLI menu. This enables Opencode and other integrations."));
        }
    };

    let token_data = match codex_auth.get_token_data().await {
        Ok(data) => data,
        Err(_) => {
            if codex_auth.mode == AuthMode::ApiKey {
                info!("Authentication successful using OpenAI API key");
                return Ok(());
            }
            return Err(anyhow::anyhow!("No authentication found. Please use the 'Login' option in the CLI menu. This enables Opencode and other integrations."));
        }
    };

    if token_data.access_token.is_empty() {
        return Err(anyhow::anyhow!("No authentication found. Please use the 'Login' option in the CLI menu. This enables Opencode and other integrations."));
    }

    if token_data.account_id.is_none() {
        return Err(anyhow::anyhow!("No authentication found. Please use the 'Login' option in the CLI menu. This enables Opencode and other integrations."));
    }

    // Log token information for debugging
    info!("Authentication successful");
    info!(
        "Account ID: {}",
        token_data.account_id.as_deref().unwrap_or("None")
    );
    info!(
        "Plan type: {}",
        codex_auth.get_plan_type().as_deref().unwrap_or("None")
    );

    Ok(())
}

async fn openai_passthrough_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let query = uri
        .query()
        .map(|query| format!("?{}", query))
        .unwrap_or_default();
    let upstream_url = format!("https://api.openai.com/v1/{}{}", path, query);
    info!("↔️ Passthrough request: {} /v1/{}{}", method, path, query);

    let client = reqwest::Client::new();
    let reqwest_method = match reqwest::Method::from_bytes(method.as_str().as_bytes()) {
        Ok(method) => method,
        Err(e) => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Unsupported HTTP method: {}", e),
                        "type": "invalid_request_error",
                        "code": "unsupported_method"
                    }
                })),
            )
                .into_response();
        }
    };

    let mut request_builder = client.request(reqwest_method, upstream_url);

    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        if matches!(
            name_str.to_ascii_lowercase().as_str(),
            "host" | "content-length" | "connection"
        ) {
            continue;
        }

        if let Ok(value_str) = value.to_str() {
            request_builder = request_builder.header(name_str, value_str);
        }
    }

    if !headers.contains_key("authorization") {
        match resolve_openai_api_key(&state.config).await {
            Ok(api_key) => {
                request_builder =
                    request_builder.header("Authorization", format!("Bearer {}", api_key));
            }
            Err(response) => return response,
        }
    }

    let upstream_response = match request_builder.body(body).send().await {
        Ok(response) => response,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to reach OpenAI API: {}", e),
                        "type": "server_error",
                        "code": "upstream_error"
                    }
                })),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(upstream_response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response_builder = Response::builder().status(status);

    for (name, value) in upstream_response.headers().iter() {
        let name_str = name.as_str();
        if matches!(
            name_str.to_ascii_lowercase().as_str(),
            "connection" | "content-length" | "transfer-encoding"
        ) {
            continue;
        }

        if let Ok(value_str) = value.to_str() {
            response_builder = response_builder.header(name_str, value_str);
        }
    }

    response_builder
        .body(axum::body::Body::from_stream(
            upstream_response.bytes_stream(),
        ))
        .unwrap_or_else(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to build proxied response: {}", e),
                        "type": "server_error",
                        "code": "response_build_error"
                    }
                })),
            )
                .into_response()
        })
}

async fn resolve_openai_api_key(config: &Config) -> Result<String, Response> {
    if let Ok(api_key) = std::env::var(OPENAI_API_KEY_ENV_VAR) {
        if !api_key.is_empty() {
            return Ok(api_key);
        }
    }

    let codex_auth = CodexAuth::from_codex_home(&config.codex_home)
        .ok()
        .flatten()
        .filter(|auth| auth.mode == AuthMode::ApiKey);

    if let Some(auth) = codex_auth {
        if let Ok(api_key) = auth.get_token().await {
            if !api_key.is_empty() {
                return Ok(api_key);
            }
        }
    }

    Err((
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": {
                "message": "This OpenAI API endpoint requires an API key. Provide an Authorization header or set OPENAI_API_KEY.",
                "type": "authentication_error",
                "code": "missing_api_key"
            }
        })),
    )
        .into_response())
}

async fn health_handler() -> Json<serde_json::Value> {
    info!("💓 Health check endpoint requested");
    let response = serde_json::json!({
        "status": "healthy",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "service": "codex-proxy-server",
        "version": env!("CARGO_PKG_VERSION")
    });
    info!("✅ Health check response: {}", response);
    Json(response)
}

async fn models_handler(State(_state): State<AppState>) -> Json<ModelList> {
    info!("📋 Models endpoint requested");
    let models = vec![
        Model {
            id: "gpt-5.5".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
        Model {
            id: "gpt-5.4".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
        Model {
            id: "gpt-5.4-mini".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
        Model {
            id: "gpt-5.3-codex-spark".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
        Model {
            id: "gpt-5".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
        Model {
            id: "gpt-5-mini".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
        Model {
            id: "gpt-5-nano".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "chatgpt".to_string(),
        },
    ];

    info!("✅ Returning {} available models", models.len());
    Json(ModelList {
        object: "list".to_string(),
        data: models,
    })
}

async fn responses_handler(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Response, StatusCode> {
    info!("🚀 RESPONSES REQUEST RECEIVED!");
    debug!("Responses request: {:?}", payload);

    let original_payload = payload.clone();
    let (mut payload, should_store) =
        match core::response_store::prepare_responses_payload(&state.config, &payload) {
            Ok(prepared) => prepared,
            Err(e) => {
                warn!("Failed to prepare Responses payload: {}", e);
                return Ok((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": {
                            "message": e.to_string(),
                            "type": "invalid_request_error",
                            "code": "previous_response_not_found"
                        }
                    })),
                )
                    .into_response());
            }
        };

    if payload.get("model").is_none() {
        payload["model"] = serde_json::json!(state.config.model);
    }
    if payload.get("instructions").is_none() {
        let mut instructions = core::client_common::BASE_INSTRUCTIONS.to_string();
        if let Some(user_instructions) = &state.config.user_instructions {
            instructions.push_str("\n\n<user_instructions>\n\n");
            instructions.push_str(user_instructions);
            instructions.push_str("\n\n</user_instructions>");
        }
        payload["instructions"] = serde_json::json!(instructions);
    }
    if payload
        .get("input")
        .and_then(|input| input.as_array())
        .is_none()
    {
        let input = match payload.get("input").cloned() {
            Some(serde_json::Value::String(text)) => vec![serde_json::json!({
                "role": "user",
                "content": text,
            })],
            Some(serde_json::Value::Null) | None => Vec::new(),
            Some(other) => vec![other],
        };
        payload["input"] = serde_json::Value::Array(input);
    }

    let stream_requested = payload
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    payload["stream"] = serde_json::json!(true);

    match chat_completions::send_responses_request(&state.config, payload).await {
        Ok(response) => {
            let status = StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

            if stream_requested {
                let config = state.config.clone();
                let mut byte_stream = response.bytes_stream();
                let stream = async_stream::stream! {
                    let mut line_buf: Vec<u8> = Vec::new();
                    let mut items: Vec<(u64, serde_json::Value)> = Vec::new();
                    let mut accumulated: Vec<u8> = Vec::new();

                    while let Some(chunk) = futures_util::StreamExt::next(&mut byte_stream).await {
                        let bytes = match chunk {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                yield Err(std::io::Error::other(e.to_string()));
                                return;
                            }
                        };
                        if should_store && status.is_success() {
                            accumulated.extend_from_slice(&bytes);
                        }
                        line_buf.extend_from_slice(&bytes);

                        while let Some(nl) = line_buf.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = line_buf.drain(..=nl).collect();
                            yield Ok::<Vec<u8>, std::io::Error>(patch_responses_sse_line(line, &mut items));
                        }
                    }

                    if !line_buf.is_empty() {
                        let line = std::mem::take(&mut line_buf);
                        yield Ok::<Vec<u8>, std::io::Error>(patch_responses_sse_line(line, &mut items));
                    }

                    if should_store && status.is_success() {
                        if let Some(response_json) = core::response_store::response_from_sse_bytes(&accumulated) {
                            if let Err(e) = core::response_store::store_response(&config, original_payload, response_json) {
                                error!("Failed to store streamed response locally: {}", e);
                            }
                        } else {
                            warn!("store=true requested but streamed response had no completed response object");
                        }
                    }
                };

                let mut out = Response::new(Body::from_stream(stream));
                *out.status_mut() = status;
                out.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                Ok(out)
            } else {
                let body = response
                    .bytes()
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                if !status.is_success() {
                    let mut out = Response::new(Body::from(body));
                    *out.status_mut() = status;
                    out.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    return Ok(out);
                }

                let Some(response_json) = core::response_store::response_from_sse_bytes(&body)
                else {
                    return Ok((
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({
                            "error": {
                                "message": "Upstream stream ended without response.completed",
                                "type": "server_error",
                                "code": "missing_response_completed"
                            }
                        })),
                    )
                        .into_response());
                };

                if should_store && status.is_success() {
                    if let Err(e) = core::response_store::store_response(
                        &state.config,
                        original_payload,
                        response_json.clone(),
                    ) {
                        error!("Failed to store response locally: {}", e);
                    }
                }

                let body = serde_json::to_vec(&response_json)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                let mut out = Response::new(Body::from(body));
                *out.status_mut() = status;
                out.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                Ok(out)
            }
        }
        Err(e) => {
            error!("❌ Responses error: {}", e);
            Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to proxy Responses request: {}", e),
                        "type": "server_error",
                        "code": "internal_error"
                    }
                })),
            )
                .into_response())
        }
    }
}

fn patch_responses_sse_line(line: Vec<u8>, items: &mut Vec<(u64, serde_json::Value)>) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(&line) else {
        return line;
    };
    let Some(rest) = text.strip_prefix("data: ") else {
        return line;
    };
    let data = rest.trim();
    if data.is_empty() || data == "[DONE]" {
        return line;
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
        return line;
    };

    if let Some(entry) = core::response_store::output_item_done_entry(&event) {
        items.push(entry);
        return line;
    }

    if event.get("type").and_then(|t| t.as_str()) == Some("response.completed") {
        let mut event = event;
        if let Some(resp) = event.get_mut("response") {
            core::response_store::fill_output_from_items(resp, items);
        }
        if let Ok(json) = serde_json::to_string(&event) {
            return format!("data: {}\n", json).into_bytes();
        }
    }

    line
}

async fn get_response_handler(
    State(state): State<AppState>,
    Path(response_id): Path<String>,
) -> Result<Response, StatusCode> {
    match core::response_store::load_response(&state.config, &response_id) {
        Ok(Some(response)) => Ok(Json(response).into_response()),
        Ok(None) => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "message": format!("Stored response not found: {}", response_id),
                    "type": "invalid_request_error",
                    "code": "response_not_found"
                }
            })),
        )
            .into_response()),
        Err(e) => Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": e.to_string(),
                    "type": "invalid_request_error",
                    "code": "invalid_response_id"
                }
            })),
        )
            .into_response()),
    }
}

async fn delete_response_handler(
    State(state): State<AppState>,
    Path(response_id): Path<String>,
) -> Result<Response, StatusCode> {
    match core::response_store::delete_response(&state.config, &response_id) {
        Ok(true) => Ok(Json(serde_json::json!({
            "id": response_id,
            "object": "response.deleted",
            "deleted": true,
        }))
        .into_response()),
        Ok(false) => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "message": format!("Stored response not found: {}", response_id),
                    "type": "invalid_request_error",
                    "code": "response_not_found"
                }
            })),
        )
            .into_response()),
        Err(e) => Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": e.to_string(),
                    "type": "invalid_request_error",
                    "code": "invalid_response_id"
                }
            })),
        )
            .into_response()),
    }
}

async fn chat_completions_handler(
    State(state): State<AppState>,
    _headers: HeaderMap,
    Json(mut request): Json<ChatRequest>,
) -> Result<Response, StatusCode> {
    if let Some((_, model)) = request.model.rsplit_once('/') {
        if request.model.starts_with("custom-router-codex-proxy/") {
            request.model = model.to_string();
        }
    }

    info!("🚀 CHAT COMPLETIONS REQUEST RECEIVED!");
    info!("Request model: {}", request.model);
    info!("Request messages count: {}", request.messages.len());
    info!("Request tools count: {}", request.tools.len());
    debug!("Full request: {:?}", request);

    // Validate model
    if !request.model.starts_with("gpt-5") {
        warn!("Invalid model requested: {}", request.model);
        return Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "message": format!("Model not found. Verify the slug starts with 'gpt-5' (e.g. 'gpt-5', 'gpt-5-mini', 'gpt-5-nano') and try again. Requested: {}", request.model),
                    "type": "model_not_found",
                    "code": "model_not_found"
                }
            }))
        ).into_response());
    }

    let response_model = request.model.clone();
    let stream_requested = request.stream;

    // Process the chat completion
    match chat_completions::stream_chat_completions(&state.config, request).await {
        Ok(mut response_stream) if !stream_requested => {
            info!("✅ Chat completion non-stream response started successfully");
            let mut content = String::new();
            let mut finish_reason = "stop".to_string();

            while let Some(result) = response_stream.recv().await {
                match result {
                    Ok(event) => {
                        for choice in event.choices {
                            if let Some(delta_content) = choice.delta.content {
                                content.push_str(&delta_content);
                            }
                            if let Some(reason) = choice.finish_reason {
                                finish_reason = reason;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Stream error while building non-stream response: {}", e);
                        return Ok((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": {
                                    "message": format!("Stream error: {}", e),
                                    "type": "stream_error",
                                    "code": "stream_error"
                                }
                            })),
                        )
                            .into_response());
                    }
                }
            }

            Ok(Json(serde_json::json!({
                "id": format!("chatcmpl-{}", &uuid::Uuid::new_v4().to_string()[..8]),
                "object": "chat.completion",
                "created": chrono::Utc::now().timestamp(),
                "model": response_model,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": content
                    },
                    "finish_reason": finish_reason
                }],
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": 0,
                    "total_tokens": 0
                }
            }))
            .into_response())
        }
        Ok(mut response_stream) => {
            info!("✅ Chat completion stream started successfully");
            let sse_stream = async_stream::stream! {
                while let Some(result) = response_stream.recv().await {
                    match result {
                        Ok(event) => {
                            if event.choices.is_empty() {
                                continue;
                            }
                            let json = serde_json::to_string(&event).unwrap_or_else(|e| {
                                error!("Failed to serialize event: {}", e);
                                r#"{"error": "Failed to serialize event"}"#.to_string()
                            });
                            yield Ok::<Event, Box<dyn std::error::Error + Send + Sync>>(Event::default().data(json));
                        }
                        Err(e) => {
                            error!("Stream error: {}", e);
                            let error_json = serde_json::to_string(&serde_json::json!({
                                "error": {
                                    "message": format!("Stream error: {}", e),
                                    "type": "stream_error",
                                    "code": "stream_error"
                                }
                            })).unwrap_or_else(|_| r#"{"error":{"message":"Failed to format error","type":"format_error","code":"format_error"}}"#.to_string());
                            yield Ok::<Event, Box<dyn std::error::Error + Send + Sync>>(Event::default().data(error_json));
                        }
                    }
                }
                yield Ok::<Event, Box<dyn std::error::Error + Send + Sync>>(Event::default().data("[DONE]"));
            };

            Ok(axum::response::Sse::new(sse_stream).into_response())
        }
        Err(e) => {
            error!("❌ Chat completions error: {}", e);
            error!("Error details: {:?}", e);
            Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to process chat completion: {}", e),
                        "type": "server_error",
                        "code": "internal_error"
                    }
                })),
            )
                .into_response())
        }
    }
}
