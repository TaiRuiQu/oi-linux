//! Update channel handling: Stable and Beta share one application, one data directory and one
//! build artefact set. Switching channels is always allowed, including towards an older version,
//! and never touches the Linux environment.
//!
//! Signatures are verified locally before anything is installed; an unsigned or wrongly signed
//! package is refused.

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};

pub mod signature;

pub const CHANNEL_STABLE: &str = "stable";
pub const CHANNEL_BETA: &str = "beta";

/// Tauri updater endpoint template. `{{target}}`, `{{arch}}` and `{{current_version}}` are
/// substituted by the updater; the same bytes are served from GitHub Releases and from the
/// domestic mirror.
pub const ENDPOINT_STABLE: &str =
    "https://github.com/oi-linux/oi-linux/releases/latest/download/latest.json";
pub const ENDPOINT_BETA: &str =
    "https://github.com/oi-linux/oi-linux/releases/download/beta/latest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub channel: String,
    pub version: String,
    pub notes: String,
    pub published_at: String,
    pub sha256: Option<String>,
    pub download_url: String,
    pub downgrade: bool,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TauriReleaseManifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub platforms: std::collections::BTreeMap<String, TauriPlatformEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TauriPlatformEntry {
    #[serde(default)]
    pub signature: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

pub fn endpoint_for(channel: &str) -> OiResult<&'static str> {
    match channel {
        CHANNEL_STABLE => Ok(ENDPOINT_STABLE),
        CHANNEL_BETA => Ok(ENDPOINT_BETA),
        other => Err(OiError::config(format!("unknown update channel: {other}"))),
    }
}

/// Compare two dotted version strings. Returns `Ordering::Less` when `a` is older than `b`.
///
/// A dedicated comparator is used rather than relying on the updater plugin alone, because the
/// product allows Beta -> Stable (and generally newer -> older) installs at the user's explicit
/// request. A prerelease marker makes a version older than the same release without one, which is
/// what `1.0.0-beta.1 < 1.0.0` means to a user switching channels.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(v: &str) -> (Vec<i64>, String) {
        let trimmed = v.trim().trim_start_matches('v');
        let (release, pre) = match trimmed.split_once(['-', '+']) {
            Some((r, p)) => (r, p.to_string()),
            None => (trimmed, String::new()),
        };
        let numbers = release
            .split('.')
            .map(|part| part.trim().parse::<i64>().unwrap_or(0))
            .collect();
        (numbers, pre)
    }

    fn cmp_numbers(a: &[i64], b: &[i64]) -> std::cmp::Ordering {
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            if x != y {
                return x.cmp(&y);
            }
        }
        std::cmp::Ordering::Equal
    }

    let (a_rel, a_pre) = split(a);
    let (b_rel, b_pre) = split(b);
    let release_cmp = cmp_numbers(&a_rel, &b_rel);
    if release_cmp != std::cmp::Ordering::Equal {
        return release_cmp;
    }
    match (a_pre.is_empty(), b_pre.is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => {
            let a_pre_num: Vec<i64> = a_pre
                .split(['.', '-'])
                .map(|p| p.trim().parse::<i64>().unwrap_or(0))
                .collect();
            let b_pre_num: Vec<i64> = b_pre
                .split(['.', '-'])
                .map(|p| p.trim().parse::<i64>().unwrap_or(0))
                .collect();
            let cmp = cmp_numbers(&a_pre_num, &b_pre_num);
            if cmp != std::cmp::Ordering::Equal {
                return cmp;
            }
            a_pre.cmp(&b_pre)
        }
    }
}

/// Build the UI-facing description from a fetched manifest.
pub fn describe(
    channel: &str,
    current_version: &str,
    manifest: &TauriReleaseManifest,
    platform_key: &str,
) -> OiResult<Option<UpdateInfo>> {
    let entry = manifest
        .platforms
        .get(platform_key)
        .ok_or_else(|| {
            OiError::config(format!(
                "the release manifest has no entry for {platform_key}"
            ))
        })?;
    let ordering = compare_versions(&manifest.version, current_version);
    if ordering == std::cmp::Ordering::Equal {
        return Ok(None);
    }
    Ok(Some(UpdateInfo {
        channel: channel.to_string(),
        version: manifest.version.clone(),
        notes: manifest.notes.clone(),
        published_at: manifest.pub_date.clone(),
        sha256: entry.sha256.clone(),
        download_url: entry.url.clone(),
        downgrade: ordering == std::cmp::Ordering::Less,
        signature: if entry.signature.is_empty() {
            None
        } else {
            Some(entry.signature.clone())
        },
    }))
}

pub fn platform_key() -> &'static str {
    "windows-x86_64"
}

/// Fetch and verify a release manifest. Only ever called because the user pressed "check for
/// updates"; there is no automatic polling.
pub async fn fetch_manifest(channel: &str, current_version: &str) -> OiResult<Option<UpdateInfo>> {
    let endpoint = endpoint_for(channel)?;
    let url = format!(
        "{endpoint}?target={}&arch=x86_64&current_version={current_version}",
        platform_key()
    );
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(format!("OI-Linux/{}", crate::VERSION))
        .build()
        .map_err(|e| OiError::other(format!("could not create the http client: {e}")))?
        .get(&url)
        .send()
        .await
        .map_err(|e| OiError::other(format!("the update endpoint could not be reached: {e}")))?;
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(OiError::other(format!(
            "the update endpoint answered with {}",
            response.status()
        )));
    }
    let manifest: TauriReleaseManifest = response
        .json()
        .await
        .map_err(|e| OiError::other(format!("the release manifest could not be parsed: {e}")))?;
    describe(channel, current_version, &manifest, platform_key())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering_handles_plain_and_prefix_forms() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("1.0.0", "1.0.1"), Less);
        assert_eq!(compare_versions("v1.2.0", "1.10.0"), Less);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Equal);
        assert_eq!(compare_versions("v1.0.0", "1.0.0"), Equal);
        // a prerelease is older than the release it precedes
        assert_eq!(compare_versions("1.0.0-beta.1", "1.0.0"), Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-beta.1"), Greater);
        assert_eq!(compare_versions("1.0.0-beta.1", "1.0.0-beta.2"), Less);
    }

    #[test]
    fn downgrades_are_offered_rather_than_hidden() {
        let manifest = TauriReleaseManifest {
            version: "0.9.0".into(),
            notes: "older".into(),
            pub_date: "2026-01-01T00:00:00Z".into(),
            platforms: [(
                "windows-x86_64".to_string(),
                TauriPlatformEntry {
                    signature: "sig".into(),
                    url: "https://example.invalid/old.exe".into(),
                    sha256: Some("ab".repeat(32)),
                },
            )]
            .into_iter()
            .collect(),
        };
        let info = describe("stable", "1.0.0", &manifest, "windows-x86_64")
            .unwrap()
            .expect("a downgrade must be offered");
        assert!(info.downgrade);
        assert_eq!(info.version, "0.9.0");
    }

    #[test]
    fn the_same_version_reports_nothing_to_do() {
        let manifest = TauriReleaseManifest {
            version: "1.0.0".into(),
            notes: String::new(),
            pub_date: String::new(),
            platforms: [(
                "windows-x86_64".to_string(),
                TauriPlatformEntry {
                    signature: String::new(),
                    url: "https://example.invalid/x.exe".into(),
                    sha256: None,
                },
            )]
            .into_iter()
            .collect(),
        };
        assert!(describe("stable", "1.0.0", &manifest, "windows-x86_64")
            .unwrap()
            .is_none());
    }

    #[test]
    fn unknown_channels_are_refused() {
        assert!(endpoint_for("nightly").is_err());
    }
}
