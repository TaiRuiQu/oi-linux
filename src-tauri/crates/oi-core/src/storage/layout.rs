//! On-disk layout of the Linux data directory (`OILinux_PRD.md` §4.2).
//!
//! ```text
//! D:\OI-Linux-Data\
//! ├── environment.json          # schema, distribution, image identity, hash, creation info
//! ├── base.qcow2               # copied at initialisation; immutable for this environment's life
//! ├── user.qcow2               # COW overlay, persists Linux changes
//! ├── config.json              # resources and workspace mapping
//! ├── ssh\                     # both directions' identities and pinned host keys (strict ACL)
//! ├── metadata.db              # file execution mode metadata
//! ├── logs\                    # 7 day rotation with a size cap
//! └── state\                   # single instance lock, abnormal exits, start failure counter
//! ```

use std::path::{Path, PathBuf};

use crate::error::{OiError, OiResult};

pub const BASE_IMAGE: &str = "base.qcow2";
pub const USER_IMAGE: &str = "user.qcow2";

#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DataDir { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn environment_json(&self) -> PathBuf {
        self.root.join("environment.json")
    }

    pub fn config_json(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn base_qcow2(&self) -> PathBuf {
        self.root.join(BASE_IMAGE)
    }

    pub fn user_qcow2(&self) -> PathBuf {
        self.root.join(USER_IMAGE)
    }

    pub fn ssh_dir(&self) -> PathBuf {
        self.root.join("ssh")
    }

    pub fn metadata_db(&self) -> PathBuf {
        self.root.join("metadata.db")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    pub fn seed_iso(&self) -> PathBuf {
        self.state_dir().join("seed.iso")
    }

    pub fn instance_lock(&self) -> PathBuf {
        self.state_dir().join("instance.lock")
    }

    pub fn failure_counter(&self) -> PathBuf {
        self.state_dir().join("startup-failures.json")
    }

    pub fn qmp_socket(&self) -> PathBuf {
        self.state_dir().join("qmp.sock")
    }

    /// Create the directory skeleton. Existing user data is never removed.
    ///
    /// A fresh directory can be briefly held by an antivirus scanner or the indexer, so a denied
    /// creation is retried a few times before it is reported.
    pub fn create_skeleton(&self) -> OiResult<()> {
        for dir in [
            self.root.clone(),
            self.ssh_dir(),
            self.logs_dir(),
            self.state_dir(),
        ] {
            let mut last_error = None;
            for attempt in 0..4 {
                match std::fs::create_dir_all(&dir) {
                    Ok(()) => {
                        last_error = None;
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e);
                        if attempt < 3 {
                            std::thread::sleep(std::time::Duration::from_millis(150));
                        }
                    }
                }
            }
            if let Some(e) = last_error {
                return Err(OiError::io(&dir, e));
            }
        }
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.environment_json().is_file() && self.base_qcow2().is_file() && self.user_qcow2().is_file()
    }

    /// Refuse to operate on a directory that exists and is non-empty unless it already looks
    /// like an OI Linux environment. This protects a user who picks the wrong folder.
    pub fn assert_usable_for_init(&self) -> OiResult<()> {
        if !self.root.exists() {
            return Ok(());
        }
        if !self.root.is_dir() {
            return Err(OiError::config(format!(
                "{} exists and is not a directory",
                self.root.display()
            )));
        }
        if self.environment_json().is_file() {
            return Err(OiError::config(format!(
                "{} already contains an OI Linux environment; use \"relink\" instead of \"create\"",
                self.root.display()
            )));
        }
        let mut entries = std::fs::read_dir(&self.root).map_err(|e| OiError::io(&self.root, e))?;
        if entries.next().is_some() {
            return Err(OiError::config(format!(
                "{} is not empty; choose an empty directory for a new Linux environment",
                self.root.display()
            )));
        }
        Ok(())
    }
}
