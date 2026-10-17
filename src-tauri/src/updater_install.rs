//! Update installation: download the package published for the selected channel, verify its
//! SHA-256 **and** its signature against the public key pinned in this build, and only then hand
//! it to Windows.
//!
//! The installer itself is not Authenticode signed in V1, so the signature check is what carries
//! the trust (`OILinux_PRD.md` §10.1/§10.2). Nothing is ever downloaded or applied automatically:
//! this only runs because the user pressed the button.

use std::path::PathBuf;

use futures::StreamExt;

use oi_core::error::{OiError, OiResult};
use oi_core::updater::{signature, UpdateInfo};

/// Environment variable used by the release job and by developers to pin the update public key.
pub const UPDATE_PUBLIC_KEY_ENV: &str = "OI_LINUX_UPDATE_PUBKEY";

/// The update public key compiled into this build. The release job injects it; when it is absent
/// the updater refuses to install anything rather than trusting an unsigned package.
pub fn pinned_public_key() -> Option<String> {
    std::env::var(UPDATE_PUBLIC_KEY_ENV)
        .ok()
        .or_else(|| option_env!("OI_LINUX_UPDATE_PUBKEY").map(|s| s.to_string()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Download the package, verify it and start the installer. Returns the path of the verified
/// installer.
pub async fn download_and_launch<F>(info: &UpdateInfo, progress: F) -> OiResult<PathBuf>
where
    F: Fn(u64, Option<u64>),
{
    let key = pinned_public_key().ok_or_else(|| {
        OiError::other(
            "this build has no update signing key pinned, so an update could not be verified",
        )
    })?;
    let public = signature::decode_public_key(&key)?;

    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(format!("OI-Linux/{}", oi_core::VERSION))
        .build()
        .map_err(|e| OiError::other(format!("could not create the http client: {e}")))?
        .get(&info.download_url)
        .send()
        .await
        .map_err(|e| OiError::other(format!("the update package could not be downloaded: {e}")))?;
    if !response.status().is_success() {
        return Err(OiError::other(format!(
            "the update package answered with {}",
            response.status()
        )));
    }
    let total = response.content_length();

    let mut bytes: Vec<u8> = Vec::with_capacity(total.unwrap_or(1 << 20) as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| OiError::other(format!("the download was interrupted: {e}")))?;
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }

    if let Some(expected) = info.sha256.as_ref().filter(|s| s.len() == 64) {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(OiError::other(format!(
                "the downloaded package has SHA-256 {actual} instead of the published {expected}"
            )));
        }
    } else {
        return Err(OiError::other(
            "the release does not publish a SHA-256 for this package, so it cannot be verified",
        ));
    }

    let signature = info.signature.as_ref().ok_or_else(|| {
        OiError::other("the release does not publish a signature for this package")
    })?;
    signature::verify(&public, &bytes, signature)?;

    let target = std::env::temp_dir().join(format!(
        "oi-linux-update-{}-{}.exe",
        info.version,
        oi_core::timeutil::now_unix()
    ));
    std::fs::write(&target, &bytes).map_err(|e| OiError::io(&target, e))?;
    tracing::info!(path = %target.display(), version = %info.version, "verified update package");

    oi_core::sys::command(&target)
        .spawn()
        .map_err(|e| OiError::other(format!("the installer could not be started: {e}")))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unpinned_build_refuses_to_install() {
        std::env::remove_var(UPDATE_PUBLIC_KEY_ENV);
        // the compile-time value is absent in development builds
        if option_env!("OI_LINUX_UPDATE_PUBKEY").is_none() {
            let info = UpdateInfo {
                channel: "stable".into(),
                version: "9.9.9".into(),
                notes: String::new(),
                published_at: String::new(),
                sha256: Some("a".repeat(64)),
                download_url: "https://example.invalid/x.exe".into(),
                downgrade: false,
                signature: None,
            };
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let err = rt
                .block_on(download_and_launch(&info, |_, _| {}))
                .unwrap_err();
            assert!(err.to_string().contains("signing key"), "{err}");
        }
    }
}
