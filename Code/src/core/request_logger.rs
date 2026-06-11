use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Local};
use futures_util::StreamExt;
use serde::Serialize;
use tracing::{error, warn};

use super::config::Config;

const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "openai-api-key",
    "proxy-authorization",
];
const REDACTED: &str = "***REDACTED***";

#[derive(Serialize)]
struct RequestResponseLog {
    timestamp: String,
    method: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    status: u16,
    duration_ms: u128,
    request: Side,
    response: Side,
}

#[derive(Serialize)]
struct Side {
    headers: BTreeMap<String, String>,
    body: serde_yaml::Value,
}

pub async fn log_request_response(
    State(config): State<Arc<Config>>,
    req: Request,
    next: Next,
) -> Response {
    let start = Instant::now();
    let started_at = Local::now();

    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| q.to_string());
    let request_headers = headers_to_map(req.headers());

    let (parts, body) = req.into_parts();
    let req_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("request_logger: failed to read request body: {}", e);
            Bytes::new()
        }
    };
    let request_body = body_to_value(&req_bytes);
    let req = Request::from_parts(parts, Body::from(req_bytes));

    let response = next.run(req).await;
    let status = response.status().as_u16();
    let response_headers = headers_to_map(response.headers());
    let (parts, body) = response.into_parts();
    let mut data_stream = body.into_data_stream();
    let log_dir = config.request_log_dir.clone();

    let teed = async_stream::stream! {
        let mut accumulated: Vec<u8> = Vec::new();
        while let Some(chunk) = data_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    accumulated.extend_from_slice(&bytes);
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(e) => {
                    error!("request_logger: error reading response body: {}", e);
                    yield Err(std::io::Error::other(e.to_string()));
                    break;
                }
            }
        }

        let record = RequestResponseLog {
            timestamp: started_at.to_rfc3339(),
            method,
            path,
            query,
            status,
            duration_ms: start.elapsed().as_millis(),
            request: Side { headers: request_headers, body: request_body },
            response: Side { headers: response_headers, body: body_to_value(&accumulated) },
        };
        write_log_file(&log_dir, started_at, &record);
    };

    Response::from_parts(parts, Body::from_stream(teed))
}

fn headers_to_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_string();
        let masked = SENSITIVE_HEADERS.contains(&key.to_ascii_lowercase().as_str());
        let val = if masked {
            REDACTED.to_string()
        } else {
            value.to_str().unwrap_or("<non-utf8>").to_string()
        };
        map.entry(key)
            .and_modify(|existing: &mut String| {
                existing.push_str(", ");
                existing.push_str(&val);
            })
            .or_insert(val);
    }
    map
}

fn body_to_value(bytes: &[u8]) -> serde_yaml::Value {
    if bytes.is_empty() {
        return serde_yaml::Value::Null;
    }

    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) {
        if let Ok(yaml) = serde_yaml::to_value(&json) {
            return yaml;
        }
    }

    match std::str::from_utf8(bytes) {
        Ok(text) => serde_yaml::Value::String(text.to_string()),
        Err(_) => serde_yaml::Value::String(format!("<{} bytes of binary data>", bytes.len())),
    }
}

fn write_log_file(base_dir: &Path, when: DateTime<Local>, record: &RequestResponseLog) {
    let day_dir: PathBuf = base_dir.join(when.format("%Y-%m-%d").to_string());
    if let Err(e) = std::fs::create_dir_all(&day_dir) {
        error!(
            "request_logger: failed to create log dir {:?}: {}",
            day_dir, e
        );
        return;
    }

    let stem = when.format("%H%M%S-%3f").to_string();
    let mut path = day_dir.join(format!("{}.yml", stem));
    if path.exists() {
        let suffix = &uuid::Uuid::new_v4().to_string()[..8];
        path = day_dir.join(format!("{}-{}.yml", stem, suffix));
    }

    match serde_yaml::to_string(record) {
        Ok(yaml) => {
            if let Err(e) = std::fs::write(&path, yaml) {
                error!("request_logger: failed to write {:?}: {}", path, e);
            }
        }
        Err(e) => warn!("request_logger: failed to serialize log record: {}", e),
    }
}
