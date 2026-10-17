//! Execution backend selection: WHPX when it really initialises, TCG otherwise.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::sys::command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accelerator {
    Whpx,
    Tcg,
}

impl Accelerator {
    pub fn as_arg(self) -> &'static str {
        match self {
            Accelerator::Whpx => "whpx",
            Accelerator::Tcg => "tcg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Accelerator::Whpx => "WHPX",
            Accelerator::Tcg => "TCG",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccelProbe {
    pub whpx_available: bool,
    pub detail: String,
    /// Wall clock cost of the probe, recorded for diagnostics only.
    pub probe_ms: u64,
}

/// Start QEMU with `-accel whpx` and see whether it really initialises.
///
/// This is deliberately not a registry or `OptionalFeatures` check: `OILinux_PRD.md` requires
/// WHPX to be adopted "only when it can actually be initialised". A machine can have the feature
/// listed as enabled and still fail to initialise inside a nested virtual machine.
pub fn probe_whpx(qemu: &Path) -> AccelProbe {
    let started = std::time::Instant::now();
    let mut cmd = command(qemu);
    cmd.args(["-accel", "whpx", "-m", "64", "-nodefaults", "-display", "none"])
        .args(["-S", "-monitor", "none", "-serial", "none"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::sleep(Duration::from_millis(900));
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut stderr = String::new();
                    if let Some(mut s) = child.stderr.take() {
                        use std::io::Read;
                        let _ = s.read_to_string(&mut stderr);
                    }
                    AccelProbe {
                        whpx_available: false,
                        detail: format!(
                            "qemu -accel whpx exited immediately ({status}): {}",
                            first_line(&stderr)
                        ),
                        probe_ms: started.elapsed().as_millis() as u64,
                    }
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    AccelProbe {
                        whpx_available: true,
                        detail: "qemu with -accel whpx stayed alive, so the backend initialises"
                            .into(),
                        probe_ms: started.elapsed().as_millis() as u64,
                    }
                }
                Err(e) => AccelProbe {
                    whpx_available: false,
                    detail: format!("could not query the probe process: {e}"),
                    probe_ms: started.elapsed().as_millis() as u64,
                },
            }
        }
        Err(e) => AccelProbe {
            whpx_available: false,
            detail: crate::sys::spawn_error(qemu, e),
            probe_ms: started.elapsed().as_millis() as u64,
        },
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QemuInfo {
    pub path: PathBuf,
    pub version: String,
    pub qemu_img: PathBuf,
}

/// Locate `qemu-system-x86_64.exe`, preferring the copy installed next to the application.
///
/// Search order:
/// 1. an explicit override (`OI_LINUX_QEMU`),
/// 2. `qemu\` beside the running executable (the shipped layout),
/// 3. a development checkout (`<repo>\vm\out\qemu`),
/// 4. the system install (`C:\Program Files\qemu`).
pub fn locate_qemu(override_path: Option<&Path>) -> OiResult<QemuInfo> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = override_path {
        candidates.push(p.to_path_buf());
    }
    if let Ok(env) = std::env::var("OI_LINUX_QEMU") {
        candidates.push(PathBuf::from(env));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("qemu").join("qemu-system-x86_64.exe"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("vm").join("out").join("qemu").join("qemu-system-x86_64.exe"));
        if let Some(parent) = cwd.parent() {
            candidates.push(
                parent
                    .join("vm")
                    .join("out")
                    .join("qemu")
                    .join("qemu-system-x86_64.exe"),
            );
        }
    }
    candidates.push(PathBuf::from(r"C:\Program Files\qemu\qemu-system-x86_64.exe"));
    candidates.push(PathBuf::from(r"C:\Program Files\QEMU\qemu-system-x86_64.exe"));

    for candidate in candidates {
        if candidate.is_file() {
            let version = qemu_version(&candidate).unwrap_or_else(|_| "unknown".into());
            let qemu_img = candidate
                .parent()
                .map(|d| d.join("qemu-img.exe"))
                .filter(|p| p.is_file())
                .or_else(|| candidate.parent().map(|d| d.join("qemu-img")))
                .ok_or_else(|| OiError::Vm("the QEMU installation has no qemu-img".into()))?;
            if !qemu_img.is_file() {
                return Err(OiError::Vm(format!(
                    "{} is present but {} is missing",
                    candidate.display(),
                    qemu_img.display()
                )));
            }
            return Ok(QemuInfo {
                path: candidate,
                version,
                qemu_img,
            });
        }
    }
    Err(OiError::Vm(
        "QEMU was not found. Reinstall OI Linux or set OI_LINUX_QEMU to the qemu-system-x86_64 executable."
            .into(),
    ))
}

pub fn qemu_version(qemu: &Path) -> OiResult<String> {
    let out = command(qemu)
        .arg("--version")
        .output()
        .map_err(|e| OiError::Vm(crate::sys::spawn_error(qemu, e)))?;
    if !out.status.success() {
        return Err(OiError::Vm("qemu --version failed".into()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("").trim();
    Ok(first.to_string())
}

/// Decide the backend for a launch.
pub fn choose_backend(preference: &str, probe: &AccelProbe) -> (Accelerator, String) {
    match preference {
        "tcg" => (
            Accelerator::Tcg,
            "TCG was requested explicitly in the settings".into(),
        ),
        "whpx" if probe.whpx_available => (
            Accelerator::Whpx,
            "WHPX was requested explicitly and initialises".into(),
        ),
        "whpx" => (
            Accelerator::Tcg,
            format!(
                "WHPX was requested but cannot initialise ({}), so TCG is used instead",
                probe.detail
            ),
        ),
        _ if probe.whpx_available => (
            Accelerator::Whpx,
            "WHPX initialises on this machine, so hardware acceleration is used".into(),
        ),
        _ => (
            Accelerator::Tcg,
            format!(
                "no hardware acceleration is available ({}), so TCG is used; compilation is slower \
                 but the Linux environment is a real one",
                probe.detail
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whpx_is_only_used_when_it_initialises() {
        let ok = AccelProbe {
            whpx_available: true,
            detail: "fine".into(),
            probe_ms: 1,
        };
        let bad = AccelProbe {
            whpx_available: false,
            detail: "failed".into(),
            probe_ms: 1,
        };
        assert_eq!(choose_backend("auto", &ok).0, Accelerator::Whpx);
        assert_eq!(choose_backend("auto", &bad).0, Accelerator::Tcg);
        assert_eq!(choose_backend("whpx", &ok).0, Accelerator::Whpx);
        // explicit whpx request must still fall back rather than fail
        assert_eq!(choose_backend("whpx", &bad).0, Accelerator::Tcg);
        assert_eq!(choose_backend("tcg", &ok).0, Accelerator::Tcg);
    }
}
