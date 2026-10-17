//! `environment.json` and `config.json` – the two files that describe an environment.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::timeutil::now_rfc3339;

pub const ENV_SCHEMA: u32 = 1;
pub const CONFIG_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentJson {
    pub schema: u32,
    pub environment_id: String,
    /// Product version that created the environment.
    pub product_version: String,
    pub alpine_version: String,
    pub base_sha256: String,
    pub base_size_bytes: u64,
    pub created_at: String,
    /// Where the shipped base image came from, for diagnostics.
    pub image_source: String,
    #[serde(default)]
    pub last_relinked_at: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl EnvironmentJson {
    pub fn load(path: &Path) -> OiResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| OiError::io(path, e))?;
        let env: EnvironmentJson = serde_json::from_str(&text)?;
        env.validate()?;
        Ok(env)
    }

    pub fn save(&self, path: &Path) -> OiResult<()> {
        self.validate()?;
        write_json_atomic(path, self)
    }

    pub fn validate(&self) -> OiResult<()> {
        if self.schema > ENV_SCHEMA {
            return Err(OiError::ImageVerification(format!(
                "environment.json schema {} is newer than this build understands ({ENV_SCHEMA}); \
                 this environment was created by a newer OI Linux and is opened read-only",
                self.schema
            )));
        }
        if self.base_sha256.len() != 64 || !self.base_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(OiError::ImageVerification(
                "environment.json has no usable base image hash".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema: u32,
    pub vcpu: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
    pub workspace_dir: String,
    pub language: String,
    pub update_channel: String,
    /// Share transport that last passed the health check: `direct` or `reverse_tunnel`.
    #[serde(default)]
    pub share_transport: Option<String>,
    /// `auto` | `whpx` | `tcg`
    #[serde(default = "default_accel")]
    pub accelerator_preference: String,
    /// Incremented whenever the seed configuration disk is regenerated.
    #[serde(default)]
    pub seed_generation: u64,
    #[serde(default)]
    pub disk_cap_gib: Option<u32>,
}

fn default_accel() -> String {
    "auto".to_string()
}

impl AppConfig {
    pub fn new(workspace_dir: impl Into<String>, data_dir: impl Into<String>) -> Self {
        let _ = data_dir.into();
        AppConfig {
            schema: CONFIG_SCHEMA,
            vcpu: 2,
            memory_mib: 1024,
            disk_gib: 8,
            workspace_dir: workspace_dir.into(),
            language: "zh-CN".to_string(),
            update_channel: "stable".to_string(),
            share_transport: None,
            accelerator_preference: default_accel(),
            seed_generation: 0,
            disk_cap_gib: None,
        }
    }

    pub fn load(path: &Path) -> OiResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| OiError::io(path, e))?;
        let cfg: AppConfig = serde_json::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> OiResult<()> {
        self.validate()?;
        write_json_atomic(path, self)
    }

    pub fn validate(&self) -> OiResult<()> {
        if self.schema > CONFIG_SCHEMA {
            return Err(OiError::Config(format!(
                "config.json schema {} is newer than this build understands ({CONFIG_SCHEMA})",
                self.schema
            )));
        }
        if !(1..=8).contains(&self.vcpu) {
            return Err(OiError::Config(format!(
                "vCPU count {} is outside the supported range 1..=8",
                self.vcpu
            )));
        }
        if !(256..=4096).contains(&self.memory_mib) {
            return Err(OiError::Config(format!(
                "guest memory {} MiB is outside the supported range 256..=4096",
                self.memory_mib
            )));
        }
        if !(2..=64).contains(&self.disk_gib) {
            return Err(OiError::Config(format!(
                "disk size {} GiB is outside the supported range 2..=64",
                self.disk_gib
            )));
        }
        if self.workspace_dir.trim().is_empty() {
            return Err(OiError::Config(
                "the Windows workspace directory is not configured".into(),
            ));
        }
        if self.language != "zh-CN" && self.language != "en-US" {
            return Err(OiError::Config(format!(
                "unsupported UI language: {}",
                self.language
            )));
        }
        if self.update_channel != "stable" && self.update_channel != "beta" {
            return Err(OiError::Config(format!(
                "unsupported update channel: {}",
                self.update_channel
            )));
        }
        if !["auto", "whpx", "tcg"].contains(&self.accelerator_preference.as_str()) {
            return Err(OiError::Config(format!(
                "unsupported accelerator preference: {}",
                self.accelerator_preference
            )));
        }
        if let Some(t) = &self.share_transport {
            if t != "direct" && t != "reverse_tunnel" {
                return Err(OiError::Config(format!("unsupported share transport: {t}")));
            }
        }
        Ok(())
    }

    /// Resources may only change while the Linux environment is stopped; callers enforce that.
    pub fn set_resources(&mut self, vcpu: u16, memory_mib: u32) -> OiResult<()> {
        let old = (self.vcpu, self.memory_mib);
        self.vcpu = vcpu;
        self.memory_mib = memory_mib;
        if let Err(e) = self.validate() {
            self.vcpu = old.0;
            self.memory_mib = old.1;
            return Err(e);
        }
        Ok(())
    }
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> OiResult<()> {
    let text = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| OiError::io(parent, e))?;
    }
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| OiError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| OiError::io(path, e))?;
    crate::timeutil::now_unix();
    Ok(())
}

pub fn new_environment(
    environment_id: impl Into<String>,
    alpine_version: impl Into<String>,
    base_sha256: impl Into<String>,
    base_size_bytes: u64,
    image_source: impl Into<String>,
) -> EnvironmentJson {
    EnvironmentJson {
        schema: ENV_SCHEMA,
        environment_id: environment_id.into(),
        product_version: crate::VERSION.to_string(),
        alpine_version: alpine_version.into(),
        base_sha256: base_sha256.into(),
        base_size_bytes,
        created_at: now_rfc3339(),
        image_source: image_source.into(),
        last_relinked_at: None,
        notes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation_rejects_out_of_range_resources() {
        let mut cfg = AppConfig::new("D:\\OIWorkspace", "D:\\OI-Linux-Data");
        assert!(cfg.validate().is_ok());
        cfg.vcpu = 0;
        assert!(cfg.validate().is_err());
        cfg.vcpu = 2;
        cfg.memory_mib = 128;
        assert!(cfg.validate().is_err());
        cfg.memory_mib = 1024;
        cfg.disk_gib = 1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn set_resources_rolls_back_on_failure() {
        let mut cfg = AppConfig::new("D:\\OIWorkspace", "D:\\OI-Linux-Data");
        assert!(cfg.set_resources(4, 2048).is_ok());
        assert_eq!((cfg.vcpu, cfg.memory_mib), (4, 2048));
        assert!(cfg.set_resources(99, 2048).is_err());
        assert_eq!((cfg.vcpu, cfg.memory_mib), (4, 2048));
    }

    #[test]
    fn environment_from_a_newer_schema_is_refused() {
        let mut env = new_environment("id", "3.24.2", "a".repeat(64), 1, "test");
        env.schema = ENV_SCHEMA + 1;
        assert!(env.validate().is_err());
    }
}
