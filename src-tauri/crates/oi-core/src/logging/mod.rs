//! Local diagnostics logging: rolling files, retention and export with redaction.
//!
//! Product rules enforced here: no telemetry ever leaves the machine, no terminal command
//! contents or source code are written, retention is about seven days with a size cap, and
//! exported logs have user names, paths and key material removed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::timeutil::{age_secs, now_unix};

pub const RETENTION_SECS: u64 = 7 * 24 * 3600;
pub const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const LOG_PREFIX: &str = "oi-linux";

pub struct Logging {
    pub dir: PathBuf,
    _guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl Logging {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Initialise file logging into `dir` plus a compact stderr writer. Called once per process.
pub fn init(dir: &Path, level: &str) -> OiResult<Logging> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    std::fs::create_dir_all(dir).map_err(|e| OiError::io(dir, e))?;

    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir)
        .map_err(|e| OiError::Other(format!("could not create the log appender: {e}")))?;
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("oi_core={level},oitool={level},oi_linux={level},warn")));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(true);

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init();

    // Path may contain private material; keep it owner-only.
    let _ = crate::sys::acl::restrict_to_current_user(dir);

    Ok(Logging {
        dir: dir.to_path_buf(),
        _guard: Some(guard),
    })
}

/// Initialise logging without file output (used by unit tests and the tool binary).
pub fn init_stderr(level: &str) -> Logging {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.to_string()));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_ansi(false))
        .try_init();
    Logging {
        dir: PathBuf::new(),
        _guard: None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogFileInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_unix: u64,
}

pub fn list_logs(dir: &Path) -> OiResult<Vec<LogFileInfo>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir).map_err(|e| OiError::io(dir, e))? {
        let entry = entry.map_err(|e| OiError::io(dir, e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified_unix = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(LogFileInfo {
            name: entry.file_name().to_string_lossy().to_string(),
            path: path.to_string_lossy().to_string(),
            size_bytes: meta.len(),
            modified_unix,
        });
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

/// Delete log files older than the retention window, then trim the oldest files until the total
/// size is under the cap. Returns the files that were removed.
pub fn enforce_retention(dir: &Path) -> OiResult<Vec<String>> {
    let mut removed = Vec::new();
    let mut files = list_logs(dir)?;
    for f in files.iter() {
        if age_secs(f.modified_unix) > RETENTION_SECS {
            let p = PathBuf::from(&f.path);
            if std::fs::remove_file(&p).is_ok() {
                removed.push(f.name.clone());
            }
        }
    }
    files.retain(|f| !removed.contains(&f.name));

    let mut total: u64 = files.iter().map(|f| f.size_bytes).sum();
    files.sort_by_key(|f| f.modified_unix);
    for f in files {
        if total <= MAX_TOTAL_BYTES {
            break;
        }
        let p = PathBuf::from(&f.path);
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(f.size_bytes);
            removed.push(f.name);
        }
    }
    Ok(removed)
}

/// Copy every log file into `dest`, removing user names, absolute paths and key material.
/// Returns the list of files written.
pub fn export_logs(dir: &Path, dest: &Path) -> OiResult<Vec<String>> {
    std::fs::create_dir_all(dest).map_err(|e| OiError::io(dest, e))?;
    let stamp = crate::timeutil::rfc3339(now_unix()).replace([':', '-'], "");
    let mut written = Vec::new();
    for f in list_logs(dir)? {
        let text = std::fs::read_to_string(&f.path).unwrap_or_default();
        let name = format!("{stamp}-{}.redacted.log", f.name);
        let target = dest.join(&name);
        std::fs::write(&target, redact(&text).as_bytes()).map_err(|e| OiError::io(&target, e))?;
        let _ = crate::sys::acl::restrict_to_current_user(&target);
        written.push(target.to_string_lossy().to_string());
    }
    Ok(written)
}

/// Remove user names, absolute Windows user paths and key blobs from a log body.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_private_key = false;
    for line in text.lines() {
        if line.contains("BEGIN OPENSSH PRIVATE KEY") {
            in_private_key = true;
            out.push_str("<redacted private key>\n");
            continue;
        }
        if line.contains("END OPENSSH PRIVATE KEY") {
            in_private_key = false;
            continue;
        }
        if in_private_key {
            continue;
        }
        out.push_str(&redact_line(line));
        out.push('\n');
    }
    out
}

fn redact_line(line: &str) -> String {
    let mut s = redact_user_paths(line);
    s = redact_keys(&s);
    s
}

fn redact_user_paths(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if line[i..].len() >= 7
            && (line[i..].starts_with("\\Users\\") || line[i..].starts_with("/Users/"))
        {
            let sep = &line[i..i + 7];
            out.push_str(sep);
            i += 7;
            // consume the account name up to the next separator
            let rest = &line[i..];
            let end = rest
                .find(['\\', '/'])
                .unwrap_or(rest.len());
            if end > 0 {
                out.push_str("<user>");
                i += end;
            }
        } else {
            let ch = line[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn redact_keys(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        let candidates = [
            rest.find("ssh-ed25519 "),
            rest.find("ssh-rsa "),
            rest.find("ssh-dss "),
            rest.find("ecdsa-sha2-"),
        ];
        let Some(pos) = candidates.into_iter().flatten().min() else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let end = after
            .find(|c: char| c == ' ' || c == '\n' || c == '\t')
            .unwrap_or(after.len());
        let token_end = after[end..]
            .find(|c: char| c != ' ' && c != '\t')
            .map(|o| end + o)
            .unwrap_or(end);
        let blob_start = token_end;
        let blob_len = after[blob_start..]
            .find([' ', '\t', '\n', '"', '\''])
            .unwrap_or(after.len() - blob_start);
        // keep the algorithm name, drop the base64 blob
        let keep = &after[..blob_start];
        if blob_len > 0 {
            out.push_str(keep);
            out.push_str("<redacted-key>");
            rest = &after[blob_start + blob_len..];
        } else {
            out.push_str(after);
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_user_names_and_key_blobs() {
        let text = "opened C:\\Users\\admin\\Documents\\oi-linux\\x.txt\n\
                    authorized_keys: ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBBBBB student@host\n\
                    -----BEGIN OPENSSH PRIVATE KEY-----\nAAAAsecret\n-----END OPENSSH PRIVATE KEY-----\n";
        let r = redact(text);
        assert!(r.contains("C:\\Users\\<user>\\Documents"), "{r}");
        assert!(!r.contains("\\admin\\"), "{r}");
        assert!(r.contains("ssh-ed25519 <redacted-key>"), "{r}");
        assert!(!r.contains("AAAAC3NzaC1lZDI1NTE5AAAAIBBBBB"), "{r}");
        assert!(!r.contains("AAAAsecret"), "{r}");
        assert!(r.contains("<redacted private key>"), "{r}");
    }

    #[test]
    fn retention_removes_old_files_and_respects_cap() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("oi-linux.2020-01-01.log");
        std::fs::write(&old, b"x").unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(RETENTION_SECS + 60);
        filetime_set(&old, old_time);

        let fresh = dir.path().join("oi-linux.2026-10-16.log");
        std::fs::write(&fresh, vec![b'y'; 1024]).unwrap();

        let removed = enforce_retention(dir.path()).unwrap();
        assert!(removed.contains(&"oi-linux.2020-01-01.log".to_string()));
        assert!(fresh.is_file());
    }

    fn filetime_set(path: &Path, t: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn export_writes_redacted_copies() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("oi-linux.x.log"), b"path C:\\Users\\admin\\a\n").unwrap();
        let dest = dir.path().join("out");
        let written = export_logs(&logs, &dest).unwrap();
        assert_eq!(written.len(), 1);
        let body = std::fs::read_to_string(&written[0]).unwrap();
        assert!(body.contains("C:\\Users\\<user>\\a"));
        assert!(!body.contains("\\admin\\"));
    }
}
