//! Application-level settings, kept *outside* the Linux data directory.
//!
//! The data directory itself holds `config.json`, but the application has to remember which
//! directory that is – including after the system drive was restored and the environment is
//! re-associated (`OILinux_PRD.md` §4.3). The file therefore lives next to the user's other
//! application data and contains nothing but a pointer and the UI preferences that must survive
//! before an environment exists.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::OiResult;

pub const SETTINGS_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    pub schema: u32,
    /// Absolute path of the Linux data directory currently in use, if any.
    #[serde(default)]
    pub data_dir: Option<String>,
    /// Last UI language, used before an environment has been opened.
    #[serde(default)]
    pub language: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            schema: SETTINGS_SCHEMA,
            data_dir: None,
            language: None,
        }
    }
}

impl AppSettings {
    pub fn load(path: &Path) -> AppSettings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<AppSettings>(&text).ok())
            .filter(|s| s.schema <= SETTINGS_SCHEMA)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> OiResult<()> {
        crate::storage::config::write_json_atomic(path, self)
    }

    pub fn data_dir_path(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(PathBuf::from)
    }

    /// Default location: `%APPDATA%\OI Linux\settings.json`, falling back to the local
    /// application data directory and finally to a folder inside the temporary directory so that
    /// the engine still works in a stripped-down environment.
    pub fn default_path() -> PathBuf {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("OI Linux").join("settings.json");
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local).join("OI Linux").join("settings.json");
        }
        std::env::temp_dir().join("oi-linux").join("settings.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_unknown_files_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(AppSettings::load(&path), AppSettings::default());

        let settings = AppSettings {
            schema: SETTINGS_SCHEMA,
            data_dir: Some(r"D:\OI-Linux-Data".into()),
            language: Some("zh-CN".into()),
        };
        settings.save(&path).unwrap();
        assert_eq!(AppSettings::load(&path), settings);

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(AppSettings::load(&path), AppSettings::default());

        let mut newer = settings.clone();
        newer.schema = SETTINGS_SCHEMA + 1;
        crate::storage::config::write_json_atomic(&path, &newer).unwrap();
        assert_eq!(AppSettings::load(&path), AppSettings::default());
    }
}
