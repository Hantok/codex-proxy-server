# Codex Proxy Server

Local OpenAI-compatible proxy for ChatGPT/Codex-backed `gpt-5*` models. It exposes Chat Completions and Responses-style endpoints on `127.0.0.1:5011`, uses local Codex/OpenCode authentication, and can pass other `/v1/*` OpenAI API calls through when an OpenAI API key is available.

## Features

- `POST /v1/chat/completions` and legacy `POST /chat/completions`
- `POST /v1/responses` and `POST /responses`
- Local `store=true` support for Responses API requests
- `previous_response_id` replay from the local response store
- `GET` and `DELETE` for stored responses
- `GET /v1/models` and `GET /health`
- Wildcard `/v1/*` passthrough to `https://api.openai.com/v1/*`
- Redacted YAML request/response logs
- Size-based log pruning and response-store retention

## Requirements

- Rust stable
- A valid `auth.json` from Codex/OpenCode login, or an OpenAI API key for passthrough-only endpoints

## Build And Run

```sh
cd Code
cargo build --release
cargo run --release -- --server
```

With no arguments, the binary opens an interactive menu for starting the server, login, token refresh, listing ports, and closing running servers.

## Authentication

The server searches for ChatGPT/Codex auth in this order:

1. `./local_auth/auth.json`
2. `./auth.json`
3. `~/.codex/auth.json`
4. `~/.opencode/auth.json`

Use the interactive `Login` option if you need to create an auth file. Passthrough OpenAI endpoints also accept an incoming `Authorization` header or `OPENAI_API_KEY`.

## Endpoints

- `POST /v1/chat/completions`: OpenAI Chat Completions-compatible local endpoint
- `POST /chat/completions`: legacy Chat Completions-compatible local endpoint
- `POST /v1/responses`: Responses API-compatible local endpoint
- `POST /responses`: legacy Responses API-compatible local endpoint
- `GET /v1/responses/{id}` and `GET /responses/{id}`: retrieve a locally stored response
- `DELETE /v1/responses/{id}` and `DELETE /responses/{id}`: delete a locally stored response
- `GET /v1/models`: list supported local model ids
- `/v1/*`: passthrough to OpenAI API for endpoints documented in `Docs/openapi.with-code-samples.yml`
- `GET /health`: health check

## Responses Store

The ChatGPT/Codex backend rejects upstream `store=true`, so this proxy forces upstream `store=false` and stores completed responses locally when the client requests `store=true`.

Stored responses are used to expand `previous_response_id` into full local conversation history before sending the next upstream request.

## Configuration

Environment variables:

- `CODEX_PROXY_REQUEST_LOG_DIR`: request/response YAML log directory, default `~/.codex-proxy/logs`
- `CODEX_PROXY_STORE_DIR`: local response store directory, default `~/.codex-proxy/store`
- `CODEX_PROXY_STORE_MAX_AGE_DAYS`: response retention, default `30`, `0` disables expiry
- `CODEX_PROXY_REQUEST_LOG_MAX_MB`: request log size cap, default `500`, `0` disables pruning
- `CODEX_PROXY_APP_LOG_MAX_MB`: app log size cap, default `100`, `0` disables pruning
- `OPENAI_API_KEY`: fallback API key for passthrough OpenAI endpoints
- `RUST_LOG`: tracing filter

A `.env` file is loaded automatically when present.

## Client Base URL

Use this base URL for OpenAI-compatible clients:

```text
http://127.0.0.1:5011/v1
```

For clients that do not append `/v1`, use:

```text
http://127.0.0.1:5011
```

## License

MIT
