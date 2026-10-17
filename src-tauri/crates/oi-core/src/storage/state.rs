//! Single instance lock, start failure counter and abnormal exit marker.

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::timeutil::now_unix;

/// Held for as long as the application owns the Linux data directory. On Windows the file is
/// opened with no sharing, so a second instance fails immediately instead of racing on qcow2
/// writes.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(path: &Path) -> OiResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OiError::io(parent, e))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_NONE: u32 = 0;
            opts.share_mode(FILE_SHARE_NONE);
        }
        let file = opts.open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32) {
                OiError::InvalidState(format!(
                    "another OI Linux instance is already using this environment ({})",
                    path.display()
                ))
            } else {
                OiError::io(path, e)
            }
        })?;
        use std::io::Write;
        let mut f = &file;
        let _ = write!(f, "pid={}\nstarted={}\n", std::process::id(), now_unix());
        Ok(InstanceLock {
            _file: file,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Counts consecutive *real* start failures. WHPX failures that were transparently retried with
/// TCG and share-only failures must never be recorded here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailureCounter {
    #[serde(default)]
    pub consecutive: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_attempt_unix: Option<u64>,
    #[serde(default)]
    pub suggested_reset_at: Option<u64>,
}

impl FailureCounter {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> OiResult<()> {
        crate::storage::config::write_json_atomic(path, self)
    }

    pub fn record_failure(&mut self, error: impl Into<String>) {
        self.consecutive = self.consecutive.saturating_add(1);
        self.last_error = Some(error.into());
        self.last_attempt_unix = Some(now_unix());
        if self.consecutive >= 3 && self.suggested_reset_at.is_none() {
            self.suggested_reset_at = Some(now_unix());
        }
    }

    pub fn record_success(&mut self) {
        self.consecutive = 0;
        self.last_error = None;
        self.suggested_reset_at = None;
    }

    /// Product rule: after three genuine start failures the UI *suggests* a reset. It never
    /// performs one.
    pub fn should_suggest_reset(&self) -> bool {
        self.consecutive >= 3
    }
}

/// Marker file used to notice that the previous run ended without a clean shutdown.
#[derive(Debug, Clone)]
pub struct RunMarker {
    path: PathBuf,
}

impl RunMarker {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        RunMarker { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns `true` when the previous run left a marker behind.
    pub fn was_abnormal(&self) -> bool {
        self.path.is_file()
    }

    pub fn mark_running(&self) -> OiResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OiError::io(parent, e))?;
        }
        std::fs::write(&self.path, format!("{}\n", now_unix()))
            .map_err(|e| OiError::io(&self.path, e))
    }

    pub fn clear(&self) -> OiResult<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(OiError::io(&self.path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_attempt_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state").join("instance.lock");
        let first = InstanceLock::acquire(&p).unwrap();
        let second = InstanceLock::acquire(&p);
        assert!(second.is_err(), "second instance must not acquire the lock");
        drop(first);
        let third = InstanceLock::acquire(&p);
        assert!(third.is_ok(), "lock must be reusable after the owner exits");
    }

    #[test]
    fn failure_counter_suggests_reset_after_three_real_failures() {
        let mut c = FailureCounter::default();
        c.record_failure("boom");
        c.record_failure("boom");
        assert!(!c.should_suggest_reset());
        c.record_failure("boom");
        assert!(c.should_suggest_reset());
        assert_eq!(c.consecutive, 3);
        c.record_success();
        assert_eq!(c.consecutive, 0);
        assert!(!c.should_suggest_reset());
    }

    #[test]
    fn failure_counter_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("failures.json");
        let mut c = FailureCounter::default();
        c.record_failure("qemu exited with 1");
        c.save(&p).unwrap();
        let back = FailureCounter::load(&p);
        assert_eq!(back.consecutive, 1);
        assert_eq!(back.last_error.as_deref(), Some("qemu exited with 1"));
    }

    #[test]
    fn run_marker_reports_abnormal_exit() {
        let dir = tempfile::tempdir().unwrap();
        let m = RunMarker::new(dir.path().join("running.marker"));
        assert!(!m.was_abnormal());
        m.mark_running().unwrap();
        assert!(m.was_abnormal());
        m.clear().unwrap();
        assert!(!m.was_abnormal());
        m.clear().unwrap();
    }
}
