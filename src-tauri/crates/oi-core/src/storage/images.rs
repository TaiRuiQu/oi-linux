//! qcow2 image handling: hashing, overlay creation and backing chain verification.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{OiError, OiResult};
use crate::sys::command;

/// Incremental SHA-256 of a file, returned as lower case hex.
pub fn sha256_file(path: &Path) -> OiResult<String> {
    let mut file = std::fs::File::open(path).map_err(|e| OiError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| OiError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayChain {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub format: String,
    #[serde(rename = "virtual-size", default)]
    pub virtual_size: u64,
    #[serde(rename = "actual-size", default)]
    pub actual_size: Option<u64>,
    #[serde(rename = "backing-filename", default)]
    pub backing_filename: Option<String>,
    #[serde(rename = "backing-filename-format", default)]
    pub backing_filename_format: Option<String>,
    #[serde(default)]
    pub corrupt: Option<bool>,
}

/// Run `qemu-img info --backing-chain --output=json` and return the parsed chain, newest layer
/// first (the ordering qemu-img itself uses).
pub fn verify_overlay_chain(qemu_img: &Path, image: &Path) -> OiResult<Vec<OverlayChain>> {
    let out = command(qemu_img)
        .arg("info")
        .arg("--backing-chain")
        .arg("--output=json")
        .arg(image)
        .output()
        .map_err(|e| OiError::ImageVerification(crate::sys::spawn_error(qemu_img, e)))?;
    if !out.status.success() {
        return Err(OiError::ImageVerification(format!(
            "qemu-img info failed for {}: {}",
            image.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let chain: Vec<OverlayChain> = serde_json::from_slice(&out.stdout).map_err(|e| {
        OiError::ImageVerification(format!("qemu-img info produced unparsable output: {e}"))
    })?;
    Ok(chain)
}

/// Verify that `user.qcow2` is an overlay whose backing file is the relative `base.qcow2` that
/// sits next to it, as required by the storage model. Returns the base image path.
pub fn check_overlay_points_at(
    qemu_img: &Path,
    user_image: &Path,
    expected_backing: &str,
) -> OiResult<PathBuf> {
    let chain = verify_overlay_chain(qemu_img, user_image)?;
    let leaf = chain
        .first()
        .ok_or_else(|| OiError::ImageVerification("qemu-img reported an empty backing chain".into()))?;
    if leaf.corrupt == Some(true) {
        return Err(OiError::ImageVerification(format!(
            "{} is reported as corrupt by qemu-img",
            user_image.display()
        )));
    }
    let backing = leaf
        .backing_filename
        .clone()
        .ok_or_else(|| OiError::ImageVerification(format!("{} has no backing file", user_image.display())))?;
    let normalized = backing.replace('\\', "/");
    if normalized != expected_backing {
        return Err(OiError::ImageVerification(format!(
            "the overlay records backing file {:?} instead of the expected relative {:?}",
            backing, expected_backing
        )));
    }
    Ok(user_image.with_file_name(expected_backing))
}

/// Create a qcow2 overlay with a *relative* backing path. `qemu-img` is executed with the data
/// directory as its working directory so the recorded path stays portable.
pub fn create_overlay(
    qemu_img: &Path,
    data_dir: &Path,
    base_name: &str,
    user_name: &str,
) -> OiResult<()> {
    let out = command(qemu_img)
        .current_dir(data_dir)
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b", base_name])
        .arg(user_name)
        .output()
        .map_err(|e| OiError::ImageVerification(crate::sys::spawn_error(qemu_img, e)))?;
    if !out.status.success() {
        return Err(OiError::ImageVerification(format!(
            "qemu-img create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Where the shipped base image can be found.
///
/// Search order (`OILinux_PRD.md` §10.1 – the installer must be complete offline):
///
/// 1. an explicit override passed by the caller (the Tauri bundle resolves the resource
///    directory and passes it in),
/// 2. `OI_LINUX_BASE_IMAGE`,
/// 3. `images\base.qcow2` inside the Tauri resource directory of the running executable,
/// 4. `qemu\..\images\base.qcow2` beside the executable,
/// 5. a development checkout (`<cwd>\vm\out\base.qcow2` and its parent).
///
/// The companion `.sha256` file, when present, is returned so that initialisation can verify the
/// copy instead of trusting it.
pub fn locate_base_image(override_dir: Option<&Path>) -> OiResult<(PathBuf, Option<String>)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = override_dir {
        candidates.push(dir.join("images").join("base.qcow2"));
        candidates.push(dir.join("base.qcow2"));
        candidates.push(dir.to_path_buf());
    }
    if let Ok(env) = std::env::var("OI_LINUX_BASE_IMAGE") {
        candidates.push(PathBuf::from(env));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("images").join("base.qcow2"));
            // resources are unpacked next to the executable by the NSIS installer
            candidates.push(dir.join("resources").join("images").join("base.qcow2"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for base in [Some(cwd.clone()), cwd.parent().map(|p| p.to_path_buf())]
            .into_iter()
            .flatten()
        {
            candidates.push(base.join("vm").join("out").join("base.qcow2"));
            candidates.push(base.join("images").join("base.qcow2"));
        }
    }

    for candidate in candidates {
        if candidate.is_file() {
            let hash_file = candidate.with_extension("qcow2.sha256");
            let expected = std::fs::read_to_string(&hash_file)
                .ok()
                .and_then(|text| text.split_whitespace().next().map(|s| s.to_string()))
                .filter(|s| s.len() == 64);
            return Ok((candidate, expected));
        }
    }
    Err(OiError::ImageVerification(
        "the Alpine base image (base.qcow2) was not found; the installation is incomplete. \
         Reinstall OI Linux, or set OI_LINUX_BASE_IMAGE to the image file."
            .into(),
    ))
}

/// Create the overlay only if it is missing, then verify the dependency chain.
pub fn ensure_overlay(
    qemu_img: &Path,
    data_dir: &Path,
    base_name: &str,
    user_name: &str,
) -> OiResult<PathBuf> {
    let user = data_dir.join(user_name);
    if !user.is_file() {
        create_overlay(qemu_img, data_dir, base_name, user_name)?;
    }
    check_overlay_points_at(qemu_img, &user, base_name)
}

/// Resize the writable layer. Disk growth is an explicit, separate operation; the guest file
/// system still has to be grown afterwards.
pub fn resize_image(qemu_img: &Path, image: &Path, size: &str) -> OiResult<()> {
    let out = command(qemu_img)
        .arg("resize")
        .arg(image)
        .arg(size)
        .output()
        .map_err(|e| OiError::ImageVerification(crate::sys::spawn_error(qemu_img, e)))?;
    if !out.status.success() {
        return Err(OiError::ImageVerification(format!(
            "qemu-img resize failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = sha256_file(&dir.path().join("nope")).unwrap_err();
        assert!(matches!(err, OiError::Io { .. }));
    }
}
