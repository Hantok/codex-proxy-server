use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tracing::{error, info};

use super::config::{self, Config};

const PRUNE_INTERVAL: Duration = Duration::from_secs(60);
const APP_LOG_PREFIX: &str = "codex-proxy.log";

struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

pub fn spawn(config: Arc<Config>) {
    let result = std::thread::Builder::new()
        .name("log-rotation".into())
        .spawn(move || {
            let app_log_dir = config::default_log_base_dir();
            loop {
                prune_request_logs(&config.request_log_dir, config.request_log_max_bytes);
                prune_app_logs(&app_log_dir, config.app_log_max_bytes);
                let expired = super::response_store::prune_expired(&config);
                if expired > 0 {
                    info!(
                        "log_rotation: pruned {} stored response(s) past retention",
                        expired
                    );
                }
                std::thread::sleep(PRUNE_INTERVAL);
            }
        });
    if let Err(e) = result {
        error!("log_rotation: failed to start pruning thread: {}", e);
    }
}

fn prune_request_logs(dir: &Path, max_bytes: u64) {
    if max_bytes == 0 || !dir.exists() {
        return;
    }

    let mut files = Vec::new();
    let mut total = 0u64;
    collect_yml_files(dir, &mut files, &mut total);
    if total <= max_bytes {
        return;
    }

    files.sort_by_key(|f| f.modified);

    let mut current = total;
    let mut removed = 0u64;
    for f in &files {
        if current <= max_bytes {
            break;
        }
        match std::fs::remove_file(&f.path) {
            Ok(()) => {
                current = current.saturating_sub(f.size);
                removed += 1;
            }
            Err(e) => error!("log_rotation: failed to remove {:?}: {}", f.path, e),
        }
    }

    if removed > 0 {
        info!(
            "log_rotation: pruned {} request-log file(s), {} -> {} bytes (cap {})",
            removed, total, current, max_bytes
        );
        remove_empty_subdirs(dir);
    }
}

fn prune_app_logs(dir: &Path, max_bytes: u64) {
    if max_bytes == 0 || !dir.exists() {
        return;
    }

    let mut files = Vec::new();
    let mut total = 0u64;
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let is_app_log = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(APP_LOG_PREFIX))
                .unwrap_or(false);
            if !is_app_log {
                continue;
            }
            if let Ok(md) = entry.metadata() {
                if !md.is_file() {
                    continue;
                }
                total += md.len();
                files.push(Entry {
                    path,
                    size: md.len(),
                    modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
    }

    if total <= max_bytes || files.is_empty() {
        return;
    }

    files.sort_by_key(|f| f.modified);
    let keep_index = files.len() - 1;

    let mut current = total;
    let mut removed = 0u64;
    for (i, f) in files.iter().enumerate() {
        if current <= max_bytes {
            break;
        }
        if i == keep_index {
            continue;
        }
        match std::fs::remove_file(&f.path) {
            Ok(()) => {
                current = current.saturating_sub(f.size);
                removed += 1;
            }
            Err(e) => error!("log_rotation: failed to remove {:?}: {}", f.path, e),
        }
    }

    if removed > 0 {
        info!(
            "log_rotation: pruned {} app-log file(s), {} -> {} bytes (cap {})",
            removed, total, current, max_bytes
        );
    }
}

fn collect_yml_files(dir: &Path, out: &mut Vec<Entry>, total: &mut u64) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        match entry.metadata() {
            Ok(md) if md.is_dir() => collect_yml_files(&path, out, total),
            Ok(md) if path.extension().map(|e| e == "yml").unwrap_or(false) => {
                *total += md.len();
                out.push(Entry {
                    path,
                    size: md.len(),
                    modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
            _ => {}
        }
    }
}

fn remove_empty_subdirs(base: &Path) {
    if let Ok(read_dir) = std::fs::read_dir(base) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let is_empty = std::fs::read_dir(&path)
                    .map(|mut it| it.next().is_none())
                    .unwrap_or(false);
                if is_empty {
                    let _ = std::fs::remove_dir(&path);
                }
            }
        }
    }
}
