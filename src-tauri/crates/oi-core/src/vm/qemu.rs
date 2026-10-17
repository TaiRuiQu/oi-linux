//! QEMU argument construction and process launch.
//!
//! Arguments are always built as a structured vector of `OsString`s and handed to
//! `std::process::Command` directly – never assembled into a shell string – so that paths
//! containing spaces or Chinese characters cannot be reinterpreted (`OILinux_PRD.md` §6.2).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::sys::command;

use super::accelerator::Accelerator;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QemuLaunchSpec {
    pub qemu_path: PathBuf,
    pub accelerator: Accelerator,
    pub vcpu: u16,
    pub memory_mib: u32,
    pub user_qcow2: PathBuf,
    pub seed_iso: Option<PathBuf>,
    pub ssh_host_port: u16,
    pub qmp_port: u16,
    pub console_log: PathBuf,
    pub uefi_code: Option<PathBuf>,
    pub uefi_vars: Option<PathBuf>,
}

impl QemuLaunchSpec {
    pub fn build_args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::with_capacity(48);

        a.push("-name".into());
        a.push("OI Linux".into());

        a.push("-accel".into());
        a.push(match self.accelerator {
            Accelerator::Whpx => "whpx".into(),
            // Multi-threaded TCG is the whole point of the TCG fallback on a slow machine.
            Accelerator::Tcg => "tcg,thread=multi".into(),
        });

        a.push("-m".into());
        a.push(self.memory_mib.to_string().into());
        a.push("-smp".into());
        a.push(self.vcpu.to_string().into());

        if let (Some(code), Some(vars)) = (&self.uefi_code, &self.uefi_vars) {
            a.push("-drive".into());
            a.push(format!(
                "if=pflash,format=raw,readonly=on,file={}",
                code.display()
            )
            .into());
            a.push("-drive".into());
            a.push(format!("if=pflash,format=raw,file={}", vars.display()).into());
        }

        // Root disk: the writable qcow2 overlay.
        a.push("-drive".into());
        a.push(format!(
            "file={},format=qcow2,if=virtio,cache=writeback,discard=unmap",
            self.user_qcow2.display()
        )
        .into());

        // Boot-time seed configuration disk (read-only optical-style medium).
        if let Some(seed) = &self.seed_iso {
            a.push("-drive".into());
            a.push(format!(
                "file={},format=raw,if=virtio,readonly=on,id=oiseed",
                seed.display()
            )
            .into());
        }

        // Networking: user mode with a single loopback-only SSH forward.
        //
        // `OI_LINUX_QEMU_RESTRICT_NET=1` isolates the guest from every network except the
        // forwarded ports. The acceptance tests use it to prove that the *core* flow (boot,
        // share, toolchain) needs no internet at all.
        let restrict = std::env::var("OI_LINUX_QEMU_RESTRICT_NET")
            .map(|v| v == "1")
            .unwrap_or(false);
        a.push("-netdev".into());
        a.push(
            if restrict {
                format!(
                    "user,id=oinet,restrict=on,hostfwd=tcp:127.0.0.1:{}-:22",
                    self.ssh_host_port
                )
            } else {
                format!(
                    "user,id=oinet,hostfwd=tcp:127.0.0.1:{}-:22",
                    self.ssh_host_port
                )
            }
            .into(),
        );
        a.push("-device".into());
        a.push("virtio-net-pci,netdev=oinet".into());

        a.push("-device".into());
        a.push("virtio-rng-pci".into());

        a.push("-display".into());
        a.push("none".into());
        a.push("-monitor".into());
        a.push("none".into());
        a.push("-serial".into());
        a.push(format!("file:{}", self.console_log.display()).into());
        // A guest-initiated `reboot` must be possible: the user reconnects manually afterwards.
        a.push("-boot".into());
        a.push("order=c".into());
        a.push("-rtc".into());
        a.push("base=utc".into());

        // QMP is the only control channel. It is a TCP socket bound to loopback on a random
        // ephemeral port; nothing is reachable from the network.
        a.push("-qmp".into());
        a.push(
            format!(
                "tcp:127.0.0.1:{},server=on,wait=off",
                self.qmp_port
            )
            .into(),
        );

        a
    }
}

pub struct QemuProcess {
    pub child: std::process::Child,
    pub job: Option<super::job::KillOnCloseJob>,
}

impl QemuProcess {
    /// Launch QEMU and immediately place it in a kill-on-close job object so that a crash of the
    /// UI process cannot leave an orphaned writer attached to `user.qcow2`.
    pub fn spawn(spec: &QemuLaunchSpec) -> OiResult<QemuProcess> {
        if !spec.qemu_path.is_file() {
            return Err(OiError::Vm(format!(
                "{} does not exist",
                spec.qemu_path.display()
            )));
        }
        let args = spec.build_args();
        let mut cmd = command(&spec.qemu_path);
        cmd.args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let child = cmd
            .spawn()
            .map_err(|e| OiError::Vm(crate::sys::spawn_error(&spec.qemu_path, e)))?;

        match super::job::KillOnCloseJob::new().and_then(|job| {
            job.assign(&child)?;
            Ok(job)
        }) {
            Ok(job) => Ok(QemuProcess {
                child,
                job: Some(job),
            }),
            Err(e) => {
                // Without the job object the crash-safety guarantee is gone; do not keep an
                // unmanaged writer alive.
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                Err(OiError::Vm(format!(
                    "the QEMU process could not be placed in a job object ({e}); \
                     the launch was aborted instead of running unmanaged"
                )))
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Collect whatever QEMU wrote to stderr; used to explain a failed launch.
    pub fn take_stderr(&mut self) -> String {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut buf);
        }
        buf
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Wait up to `timeout` for QEMU to exit on its own.
    pub fn wait_timeout(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(_) => return true,
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// Locate the UEFI firmware shipped with QEMU, if the installed image needs it.
pub fn find_uefi_firmware(qemu_path: &Path) -> Option<(PathBuf, PathBuf)> {
    let share = qemu_path.parent()?.join("share");
    let code = share.join("edk2-x86_64-code.fd");
    let vars = share.join("edk2-i386-vars.fd");
    if code.is_file() {
        Some((code, if vars.is_file() { vars } else { PathBuf::new() }))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> QemuLaunchSpec {
        QemuLaunchSpec {
            qemu_path: PathBuf::from(r"C:\Program Files\qemu\qemu-system-x86_64.exe"),
            accelerator: Accelerator::Tcg,
            vcpu: 2,
            memory_mib: 1024,
            user_qcow2: PathBuf::from(r"D:\OI 数据\user.qcow2"),
            seed_iso: Some(PathBuf::from(r"D:\OI 数据\state\seed.iso")),
            ssh_host_port: 51234,
            qmp_port: 51235,
            console_log: PathBuf::from(r"D:\OI 数据\logs\console.log"),
            uefi_code: None,
            uefi_vars: None,
        }
    }

    fn args_strings(spec: &QemuLaunchSpec) -> Vec<String> {
        spec.build_args()
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn tcg_backend_is_multithreaded_and_explicit() {
        let a = args_strings(&spec());
        let i = a.iter().position(|x| x == "-accel").unwrap();
        assert_eq!(a[i + 1], "tcg,thread=multi");
    }

    #[test]
    fn whpx_backend_replaces_tcg() {
        let mut s = spec();
        s.accelerator = Accelerator::Whpx;
        let a = args_strings(&s);
        let i = a.iter().position(|x| x == "-accel").unwrap();
        assert_eq!(a[i + 1], "whpx");
        assert!(!a.iter().any(|x| x.contains("thread=multi")));
    }

    #[test]
    fn ssh_forward_is_bound_to_loopback_only() {
        let a = args_strings(&spec());
        let netdev = a
            .iter()
            .find(|x| x.starts_with("user,id=oinet"))
            .expect("netdev");
        assert!(netdev.contains("hostfwd=tcp:127.0.0.1:51234-:22"), "{netdev}");
        assert!(!netdev.contains("0.0.0.0"));
        assert!(!a.iter().any(|x| x.contains(":2222")));
    }

    #[test]
    fn paths_with_spaces_and_non_ascii_survive_as_single_arguments() {
        let a = spec().build_args();
        assert!(a.iter().any(|x| {
            let s = x.to_string_lossy();
            s.starts_with("file=D:\\OI 数据\\user.qcow2")
        }));
        assert!(a.iter().any(|x| {
            let s = x.to_string_lossy();
            s.starts_with("file=D:\\OI 数据\\state\\seed.iso")
        }));
    }

    #[test]
    fn display_is_headless_and_qmp_is_loopback() {
        let a = args_strings(&spec());
        let i = a.iter().position(|x| x == "-display").unwrap();
        assert_eq!(a[i + 1], "none");
        let q = a.iter().position(|x| x == "-qmp").unwrap();
        assert!(a[q + 1].starts_with("tcp:127.0.0.1:51235"));
        assert!(a[q + 1].contains("wait=off"));
    }

    #[test]
    fn seed_disk_is_read_only() {
        let a = args_strings(&spec());
        let drive = a
            .iter()
            .find(|x| x.contains("seed.iso"))
            .expect("seed drive");
        assert!(drive.contains("readonly=on"), "{drive}");
    }

    #[test]
    fn missing_seed_disk_is_simply_absent() {
        let mut s = spec();
        s.seed_iso = None;
        let a = args_strings(&s);
        assert!(!a.iter().any(|x| x.contains("seed.iso")));
    }
}
