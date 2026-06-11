use dirs::home_dir;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub codex_home: PathBuf,
    pub chatgpt_base_url: String,
    pub model: String,
    pub user_instructions: Option<String>,
    pub request_log_dir: PathBuf,
    pub response_store_dir: PathBuf,
    pub response_store_max_age_secs: u64,
    pub request_log_max_bytes: u64,
    pub app_log_max_bytes: u64,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let codex_home = find_codex_home();
        let request_log_dir = default_request_log_dir();
        let response_store_dir = default_response_store_dir();
        let response_store_max_age_secs = env_days_to_secs("CODEX_PROXY_STORE_MAX_AGE_DAYS", 30);
        let request_log_max_bytes = env_mb_to_bytes("CODEX_PROXY_REQUEST_LOG_MAX_MB", 500);
        let app_log_max_bytes = env_mb_to_bytes("CODEX_PROXY_APP_LOG_MAX_MB", 100);

        // Load user instructions from AGENTS.md
        let user_instructions = Self::load_instructions(Some(&codex_home));

        // Check if auth.json exists in ./local_auth directory (fallback)
        let local_auth_dir = std::env::current_dir()?.join("local_auth");
        let local_auth_file = local_auth_dir.join("auth.json");
        if local_auth_file.exists() {
            return Ok(Config {
                codex_home: local_auth_dir,
                chatgpt_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                model: "gpt-5".to_string(),
                user_instructions,
                request_log_dir,
                response_store_dir,
                response_store_max_age_secs,
                request_log_max_bytes,
                app_log_max_bytes,
            });
        }
        // Check if auth.json exists in the current directory (legacy fallback)
        let current_dir_auth = std::env::current_dir()?.join("auth.json");
        if current_dir_auth.exists() {
            return Ok(Config {
                codex_home: std::env::current_dir()?,
                chatgpt_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                model: "gpt-5".to_string(),
                user_instructions,
                request_log_dir,
                response_store_dir,
                response_store_max_age_secs,
                request_log_max_bytes,
                app_log_max_bytes,
            });
        }

        Ok(Config {
            codex_home,
            chatgpt_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            model: "gpt-5".to_string(), // Default, but can be changed to any gpt-5* variant
            user_instructions,
            request_log_dir,
            response_store_dir,
            response_store_max_age_secs,
            request_log_max_bytes,
            app_log_max_bytes,
        })
    }

    fn load_instructions(codex_dir: Option<&std::path::Path>) -> Option<String> {
        let mut p = match codex_dir {
            Some(p) => p.to_path_buf(),
            None => return None,
        };

        p.push("AGENTS.md");
        std::fs::read_to_string(&p).ok().and_then(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        })
    }
}

fn env_mb_to_bytes(var: &str, default_mb: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_mb)
        .saturating_mul(1024 * 1024)
}

fn env_days_to_secs(var: &str, default_days: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_days)
        .saturating_mul(24 * 60 * 60)
}

pub fn default_log_base_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".codex-proxy")
        .join("logs")
}

fn default_request_log_dir() -> PathBuf {
    std::env::var("CODEX_PROXY_REQUEST_LOG_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| PathBuf::from(value.trim()))
        .unwrap_or_else(default_log_base_dir)
}

fn default_response_store_dir() -> PathBuf {
    std::env::var("CODEX_PROXY_STORE_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| PathBuf::from(value.trim()))
        .unwrap_or_else(|| {
            home_dir()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
                .join(".codex-proxy")
                .join("store")
        })
}

fn find_codex_home() -> PathBuf {
    // Try to find codex home directory for Codex Proxy Server and Opencode integration
    if let Some(home) = home_dir() {
        // First check for .codex directory (Codex Proxy Server CLI compatibility)
        let codex_home = home.join(".codex");
        if codex_home.exists() {
            return codex_home;
        }

        // Then check for .opencode directory (Opencode integration default)
        let opencode_home = home.join(".opencode");
        if opencode_home.exists() {
            return opencode_home;
        }
    }

    // Fallback to current directory
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
