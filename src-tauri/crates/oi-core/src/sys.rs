//! Platform helpers: console-less process spawning and strict ACLs on Windows.

use std::path::Path;
use std::process::Command;

/// Create a `Command` that never flashes a console window on Windows.
pub fn command(program: impl AsRef<Path>) -> Command {
    let mut cmd = Command::new(program.as_ref());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub fn spawn_error(program: impl AsRef<Path>, e: std::io::Error) -> String {
    format!(
        "failed to start {}: {e}",
        program.as_ref().display()
    )
}

#[cfg(windows)]
pub mod acl {
    use std::path::Path;

    use crate::error::{OiError, OiResult};
    use crate::sys::command;

    /// Current user's SID, obtained from `whoami` so that domain and local accounts both work.
    pub fn current_user_sid() -> OiResult<String> {
        let out = command("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .map_err(|e| OiError::Other(format!("whoami failed: {e}")))?;
        if !out.status.success() {
            return Err(OiError::Other("whoami /user returned a failure status".into()));
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        parse_whoami_sid(&text)
            .ok_or_else(|| OiError::Other(format!("could not parse whoami output: {text}")))
    }

    fn parse_whoami_sid(text: &str) -> Option<String> {
        let field = text.split(',').nth(1)?;
        let sid = field.trim().trim_matches('"').trim();
        if sid.starts_with("S-1-") {
            Some(sid.to_string())
        } else {
            None
        }
    }

    /// Replace the DACL of `path` with a single ACE granting only the current user full control,
    /// with inheritance disabled. Used for private keys, the metadata database and log files.
    pub fn restrict_to_current_user(path: &Path) -> OiResult<()> {
        let sid = current_user_sid()?;
        // Inheritance flags are only meaningful on directories. Asking `icacls` to grant
        // `(OI)(CI)F` on a *file* leaves the file with an empty DACL, which denies access to
        // everyone - including the user who owns it.
        let grant = if path.is_dir() {
            format!("*{sid}:(OI)(CI)F")
        } else {
            format!("*{sid}:F")
        };
        let out = command("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(&grant)
            .output()
            .map_err(|e| OiError::Other(format!("icacls failed to start: {e}")))?;
        if !out.status.success() {
            return Err(OiError::Other(format!(
                "icacls failed for {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stdout).trim()
            )));
        }
        Ok(())
    }

    /// Restrict a directory and everything under it to the current user.
    ///
    /// The children are made to inherit the directory's ACE instead of being granted
    /// individually, so that no object can end up with an empty DACL.
    pub fn restrict_tree_to_current_user(path: &Path) -> OiResult<()> {
        restrict_to_current_user(path)?;
        if !path.is_dir() {
            return Ok(());
        }
        let out = command("icacls")
            .arg(format!("{}\\*", path.display()))
            .arg("/inheritance:e")
            .arg("/T")
            .arg("/C")
            .arg("/Q")
            .output()
            .map_err(|e| OiError::Other(format!("icacls failed to start: {e}")))?;
        if !out.status.success() {
            return Err(OiError::Other(format!(
                "icacls /T failed for {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stdout).trim()
            )));
        }
        Ok(())
    }

    /// Read the DACL back as SDDL so tests can assert that private material is not readable by
    /// everyone.
    pub fn sddl(path: &Path) -> OiResult<String> {
        let out = command("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "(Get-Acl -LiteralPath '{}').Sddl",
                    path.display().to_string().replace('\'', "''")
                ),
            ])
            .output()
            .map_err(|e| OiError::Other(format!("Get-Acl failed to start: {e}")))?;
        if !out.status.success() {
            return Err(OiError::Other(format!(
                "Get-Acl failed for {}",
                path.display()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

#[cfg(not(windows))]
pub mod acl {
    use std::path::Path;

    use crate::error::{OiError, OiResult};

    pub fn current_user_sid() -> OiResult<String> {
        Ok("uid".into())
    }

    pub fn restrict_to_current_user(path: &Path) -> OiResult<()> {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| OiError::io(path, e))?;
        let mut perms = meta.permissions();
        perms.set_mode(if meta.is_dir() { 0o700 } else { 0o600 });
        std::fs::set_permissions(path, perms).map_err(|e| OiError::io(path, e))
    }

    pub fn restrict_tree_to_current_user(path: &Path) -> OiResult<()> {
        restrict_to_current_user(path)?;
        if path.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| OiError::io(path, e))? {
                let entry = entry.map_err(|e| OiError::io(path, e))?;
                restrict_tree_to_current_user(&entry.path())?;
            }
        }
        Ok(())
    }

    pub fn sddl(_path: &Path) -> OiResult<String> {
        Ok(String::new())
    }
}

#[cfg(windows)]
pub mod host {
    use std::path::{Path, PathBuf};

    use crate::sys::command;

    /// Total physical memory in MiB.
    pub fn total_memory_mib() -> u64 {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            status.ullTotalPhys / (1024 * 1024)
        } else {
            0
        }
    }

    /// Free space of the volume that contains `path`, in MiB.
    pub fn free_disk_mib(path: &Path) -> u64 {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let existing = nearest_existing_ancestor(path);
        let wide = HSTRING::from(existing.to_string_lossy().to_string());
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(&wide, Some(&mut free), None, None)
        }
        .is_ok();
        if ok {
            free / (1024 * 1024)
        } else {
            0
        }
    }

    /// Check that `path` sits on a local, fixed NTFS volume. Network shares and non-NTFS file
    /// systems are refused, because the product only guarantees its semantics there
    /// (`OILinux_PRD.md` §1.2, §11.2 item 9).
    pub fn volume_is_local_ntfs(path: &Path) -> Result<String, String> {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{
            GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
        };

        // `DRIVE_FIXED` from winbase.h; the windows crate exposes it as a plain value.
        const DRIVE_FIXED: u32 = 3;

        let text = path.to_string_lossy().to_string();
        if text.starts_with("\\\\") || text.starts_with("//") {
            return Err("network share paths are not supported; choose a local folder".into());
        }
        let existing = nearest_existing_ancestor(path);

        let mut buffer = [0u16; 261];
        let root = unsafe {
            GetVolumePathNameW(&HSTRING::from(existing.to_string_lossy().to_string()), &mut buffer)
        };
        if root.is_err() {
            return Err(format!(
                "the volume of {} could not be determined",
                existing.display()
            ));
        }
        let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        let root = String::from_utf16_lossy(&buffer[..len]);

        let drive_type = unsafe { GetDriveTypeW(&HSTRING::from(root.clone())) };
        if drive_type != DRIVE_FIXED {
            return Err(format!(
                "{root} is not a fixed local drive; the workspace must be on a local NTFS volume"
            ));
        }

        let mut fs_name = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                &HSTRING::from(root.clone()),
                None,
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        };
        if ok.is_err() {
            return Err(format!("{root} could not be queried for its file system"));
        }
        let fs_len = fs_name.iter().position(|c| *c == 0).unwrap_or(fs_name.len());
        let fs = String::from_utf16_lossy(&fs_name[..fs_len]);
        if !fs.eq_ignore_ascii_case("NTFS") {
            return Err(format!(
                "{root} uses {fs}; the shared workspace is only supported on NTFS"
            ));
        }
        Ok(format!("{} is on {root} ({fs})", existing.display()))
    }

    fn nearest_existing_ancestor(path: &Path) -> PathBuf {
        let mut current = path.to_path_buf();
        loop {
            if current.exists() {
                return current;
            }
            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => return PathBuf::from(r"C:\"),
            }
        }
    }

    /// Version string of the running Windows installation, e.g. `10.0.20348.2113`.
    ///
    /// `cmd /c ver` localises its wording, so the version is picked out by shape rather than by
    /// the surrounding English text.
    pub fn windows_build() -> String {
        if let Ok(out) = command("cmd").args(["/C", "ver"]).output() {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if let Some(version) = find_version_token(&text) {
                return version;
            }
        }
        "unknown".to_string()
    }

    fn find_version_token(text: &str) -> Option<String> {
        let mut token = String::new();
        let mut tokens: Vec<String> = Vec::new();
        for ch in text.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                token.push(ch);
            } else if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        }
        if !token.is_empty() {
            tokens.push(token);
        }
        tokens.into_iter().find(|candidate| {
            let parts: Vec<&str> = candidate.split('.').collect();
            parts.len() >= 3
                && parts.iter().all(|part| !part.is_empty() && part.parse::<u32>().is_ok())
                && parts[0].parse::<u32>().map(|major| major >= 5).unwrap_or(false)
        })
    }

    /// Resolve an executable by name on `PATH` (the Windows equivalent of `which`).
    pub fn which(exe: &str) -> Option<PathBuf> {
        let out = command("where").arg(exe).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(PathBuf::from)
    }

    /// Is the Windows VS Code installation present?
    pub fn vscode_available() -> bool {
        if which("code.exe").is_some() {
            return true;
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Microsoft VS Code")
                    .join("Code.exe"),
            );
        }
        candidates.push(
            PathBuf::from(r"C:\Program Files\Microsoft VS Code\Code.exe"),
        );
        candidates.push(PathBuf::from(
            r"C:\Program Files (x86)\Microsoft VS Code\Code.exe",
        ));
        candidates.iter().any(|p| p.is_file())
    }

    /// Is Windows Terminal installed? The product keeps working without it.
    pub fn windows_terminal_available() -> bool {
        if which("wt.exe").is_some() {
            return true;
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let packages = PathBuf::from(local).join("Microsoft").join("WindowsApps");
            if let Ok(entries) = std::fs::read_dir(&packages) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if name.starts_with("microsoft.windowsterminal") && entry.path().join("wt.exe").is_file() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// State of the `HypervisorPlatform` optional feature as reported by DISM, when the caller
    /// has the rights to ask. `None` means "could not be determined".
    pub fn whpx_feature_state() -> Option<bool> {
        let out = command("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform).State",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        if text.contains("enabled") && !text.contains("disabled") {
            Some(true)
        } else if text.contains("disabled") {
            Some(false)
        } else {
            None
        }
    }

    /// Ask Windows to enable the Windows Hypervisor Platform feature. Elevation is requested by
    /// the operating system; a refusal is reported as a normal outcome and never blocks QEMU's
    /// TCG fallback (`OILinux_PRD.md` §6.1).
    pub fn enable_whpx_feature() -> (bool, String) {
        let script = "Start-Process -FilePath dism.exe -ArgumentList '/online','/enable-feature',\
                      '/featurename:HypervisorPlatform','/all','/norestart' -Verb RunAs -Wait";
        match command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
        {
            Ok(out) if out.status.success() => (
                true,
                "Windows was asked to enable the Windows Hypervisor Platform; a restart may be \
                 required before WHPX can be used."
                    .to_string(),
            ),
            Ok(out) => {
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                (
                    false,
                    if detail.is_empty() {
                        "the elevation prompt was refused or denied; TCG remains available".into()
                    } else {
                        format!("enabling the feature did not complete: {detail}")
                    },
                )
            }
            Err(e) => (false, format!("could not start PowerShell: {e}")),
        }
    }

    /// Launch a program with an argument, without a console window.
    pub fn open(program: &Path, args: &[&str]) -> crate::error::OiResult<()> {
        let mut cmd = command(program);
        cmd.args(args);
        cmd.spawn().map(|_| ()).map_err(|e| {
            crate::error::OiError::Other(format!(
                "could not start {}: {e}",
                program.display()
            ))
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn version_parsing_survives_localisation() {
            // A console-less `cmd /c ver` reports in the OEM code page, so the wording around the
            // number is not stable; only the shape is.
            assert_eq!(
                find_version_token("Microsoft Windows [Version 10.0.20348.2966]").as_deref(),
                Some("10.0.20348.2966")
            );
            assert_eq!(
                find_version_token("Microsoft Windows [版本 10.0.20348.2966]").as_deref(),
                Some("10.0.20348.2966")
            );
            assert_eq!(find_version_token("no version here"), None);
        }
    }
}

#[cfg(not(windows))]
pub mod host {
    use std::path::{Path, PathBuf};

    pub fn total_memory_mib() -> u64 {
        0
    }

    pub fn free_disk_mib(_path: &Path) -> u64 {
        0
    }

    pub fn windows_build() -> String {
        "not windows".to_string()
    }

    pub fn which(_exe: &str) -> Option<PathBuf> {
        None
    }

    pub fn volume_is_local_ntfs(path: &Path) -> Result<String, String> {
        Ok(format!("{} is on a POSIX file system", path.display()))
    }

    pub fn vscode_available() -> bool {
        false
    }

    pub fn windows_terminal_available() -> bool {
        false
    }

    pub fn whpx_feature_state() -> Option<bool> {
        None
    }

    pub fn enable_whpx_feature() -> (bool, String) {
        (
            false,
            "the Windows Hypervisor Platform is a Windows facility".to_string(),
        )
    }

    pub fn open(program: &Path, args: &[&str]) -> crate::error::OiResult<()> {
        std::process::Command::new(program)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|e| {
                crate::error::OiError::Other(format!(
                    "could not start {}: {e}",
                    program.display()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_runs_without_shell_interpretation() {
        #[cfg(windows)]
        let out = command("cmd").args(["/C", "echo", "hello world"]).output();
        #[cfg(not(windows))]
        let out = command("echo").arg("hello world").output();
        let out = out.expect("spawn");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hello world"));
    }
}
