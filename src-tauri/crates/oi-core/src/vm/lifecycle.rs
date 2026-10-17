//! Virtual machine lifecycle: the state machine from `OILinux_PRD.md` §6.3.
//!
//! ```text
//! UNINITIALIZED -> STOPPED -> STARTING -> RUNNING -> STOPPING -> STOPPED
//!                                    \-> START_ERROR
//! RUNNING + SHARED | SHARE_DEGRADED | SHARE_RECOVERING
//! ```
//!
//! Anything to do with the shared folder is *reported* separately: a share failure never turns
//! into a VM failure and never blocks the use of the Linux environment.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::storage::config::AppConfig;
use crate::storage::layout::DataDir;
use crate::storage::state::FailureCounter;
use crate::timeutil::now_unix;

use super::accelerator::{choose_backend, locate_qemu, probe_whpx, Accelerator, AccelProbe, QemuInfo};
use super::ports;
use super::qemu::{find_uefi_firmware, QemuLaunchSpec, QemuProcess};
use super::qmp::QmpClient;
use super::seed::{SeedSpec, SEED_SCHEMA};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmStatus {
    Uninitialized,
    Stopped,
    Starting,
    Running,
    Stopping,
    StartError,
}

impl VmStatus {
    pub fn is_running(self) -> bool {
        matches!(self, VmStatus::Running | VmStatus::Stopping)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareTransport {
    Direct,
    ReverseTunnel,
}

impl ShareTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            ShareTransport::Direct => "direct",
            ShareTransport::ReverseTunnel => "reverse_tunnel",
        }
    }

    pub fn from_str(s: &str) -> Option<ShareTransport> {
        match s {
            "direct" => Some(ShareTransport::Direct),
            "reverse_tunnel" => Some(ShareTransport::ReverseTunnel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmState {
    pub status: VmStatus,
    pub accelerator: Option<Accelerator>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub consecutive_start_failures: u32,
    pub uptime_secs: Option<u64>,
}

impl VmState {
    pub fn uninitialized() -> Self {
        VmState {
            status: VmStatus::Uninitialized,
            accelerator: None,
            pid: None,
            last_error: None,
            consecutive_start_failures: 0,
            uptime_secs: None,
        }
    }

    pub fn stopped() -> Self {
        VmState {
            status: VmStatus::Stopped,
            ..VmState::uninitialized()
        }
    }
}

/// Everything a successful launch discovered, kept for the UI and for diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupReport {
    pub accelerator: Accelerator,
    pub accelerator_reason: String,
    pub probe: AccelProbe,
    pub ssh_host_port: u16,
    pub qmp_port: u16,
    pub sftp_port: u16,
    pub tunnel_port: u16,
    pub launch_secs: f64,
    pub seed_regenerated: bool,
}

/// Owns the QEMU process for one environment. The caller is responsible for the share service and
/// for the SSH readiness wait, both of which live in their own modules.
pub struct VmController {
    pub data: DataDir,
    pub qemu: QemuInfo,
    pub config: AppConfig,
    process: Option<QemuProcess>,
    qmp: Option<QmpClient>,
    status: VmStatus,
    accelerator: Option<Accelerator>,
    started_at: Option<Instant>,
    last_error: Option<String>,
    probe: Option<AccelProbe>,
    ssh_host_port: Option<u16>,
    qmp_port: Option<u16>,
    sftp_port: Option<u16>,
    tunnel_port: Option<u16>,
    /// Ports reserved by the caller (the share service has to know its own port before the seed
    /// disk is written, so the launcher accepts them instead of allocating its own).
    port_override: Option<(u16, u16)>,
}

impl VmController {
    pub fn new(data: DataDir, qemu: QemuInfo, config: AppConfig) -> Self {
        let status = if data.is_initialized() {
            VmStatus::Stopped
        } else {
            VmStatus::Uninitialized
        };
        VmController {
            data,
            qemu,
            config,
            process: None,
            qmp: None,
            status,
            accelerator: None,
            started_at: None,
            last_error: None,
            probe: None,
            ssh_host_port: None,
            qmp_port: None,
            sftp_port: None,
            tunnel_port: None,
            port_override: None,
        }
    }

    /// Hand the launcher the ports that the caller has already bound (the host share listener
    /// and the guest side of the reverse tunnel). When they are not supplied the launcher
    /// allocates a fresh pair.
    pub fn set_reserved_ports(&mut self, sftp_port: u16, tunnel_port: u16) {
        self.port_override = Some((sftp_port, tunnel_port));
    }

    pub fn status(&self) -> VmStatus {
        self.status
    }

    pub fn set_status(&mut self, status: VmStatus) {
        self.status = status;
    }

    pub fn accelerator(&self) -> Option<Accelerator> {
        self.accelerator
    }

    pub fn probe(&self) -> Option<&AccelProbe> {
        self.probe.as_ref()
    }

    pub fn ssh_host_port(&self) -> Option<u16> {
        self.ssh_host_port
    }

    pub fn qmp_port(&self) -> Option<u16> {
        self.qmp_port
    }

    pub fn sftp_port(&self) -> Option<u16> {
        self.sftp_port
    }

    pub fn tunnel_port(&self) -> Option<u16> {
        self.tunnel_port
    }

    pub fn state(&self) -> VmState {
        let failures = FailureCounter::load(&self.data.failure_counter());
        VmState {
            status: self.status,
            accelerator: self.accelerator,
            pid: self.process.as_ref().map(|p| p.pid()),
            last_error: self.last_error.clone(),
            consecutive_start_failures: failures.consecutive,
            uptime_secs: self.started_at.map(|t| t.elapsed().as_secs()),
        }
    }

    pub fn is_process_alive(&mut self) -> bool {
        self.process.as_mut().map(|p| p.is_running()).unwrap_or(false)
    }

    /// Detect a guest that stopped on its own (`poweroff` inside Linux, or a crash).
    pub fn poll_exit(&mut self) -> OiResult<Option<std::process::ExitStatus>> {
        let Some(process) = self.process.as_mut() else {
            return Ok(None);
        };
        match process.child.try_wait() {
            Ok(Some(status)) => {
                let stderr = process.take_stderr();
                self.process = None;
                self.qmp = None;
                self.started_at = None;
                self.status = VmStatus::Stopped;
                if !stderr.trim().is_empty() {
                    tracing::warn!(stderr = %stderr.trim(), "qemu wrote to stderr");
                }
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(OiError::vm(format!("could not query the QEMU process: {e}"))),
        }
    }

    /// Launch QEMU. Returns as soon as QEMU and its QMP channel are up; the caller then waits for
    /// SSH to become reachable.
    pub async fn start(&mut self) -> OiResult<StartupReport> {
        if self.status.is_running() {
            return Err(OiError::InvalidState("running".into()));
        }
        if !self.data.is_initialized() {
            self.status = VmStatus::Uninitialized;
            return Err(OiError::NotInitialized(
                "create or associate a Linux environment first".into(),
            ));
        }
        self.config.validate()?;

        self.status = VmStatus::Starting;
        self.last_error = None;
        let started = Instant::now();

        // Accelerator: probe, then honour the configured preference.
        let probe = probe_whpx(&self.qemu.path);
        let (accelerator, reason) = choose_backend(&self.config.accelerator_preference, &probe);
        tracing::info!(
            accelerator = accelerator.label(),
            reason = %reason,
            "starting the Linux environment"
        );
        self.probe = Some(probe.clone());
        self.accelerator = Some(accelerator);

        // Dynamic ports, retried when a port turns out to be occupied.
        let (sftp_port, tunnel_port) = match self.port_override.take() {
            Some(pair) => pair,
            None => ports::reserve_pair()?,
        };
        let mut launch_error: Option<OiError> = None;

        for attempt in 0..5 {
            let ssh_port = ports::allocate_loopback_port()?;
            let qmp_port = ports::allocate_loopback_port()?;
            if !ports::is_free(ssh_port) || !ports::is_free(qmp_port) {
                continue;
            }

            // The seed disk carries the endpoints, so it is regenerated whenever they change.
            let seed_regenerated =
                self.regenerate_seed(sftp_port, tunnel_port, accelerator)?;

            let console_log = self.data.logs_dir().join("console.log");
            if let Some(parent) = console_log.parent() {
                std::fs::create_dir_all(parent).map_err(|e| OiError::io(parent, e))?;
            }
            let (uefi_code, uefi_vars) = match find_uefi_firmware(&self.qemu.path) {
                Some((code, vars)) if !vars.as_os_str().is_empty() => {
                    (Some(code), Some(self.prepare_uefi_vars(&vars)?))
                }
                _ => (None, None),
            };

            let spec = QemuLaunchSpec {
                qemu_path: self.qemu.path.clone(),
                accelerator,
                vcpu: self.config.vcpu,
                memory_mib: self.config.memory_mib,
                user_qcow2: self.data.user_qcow2(),
                seed_iso: Some(self.data.seed_iso()),
                ssh_host_port: ssh_port,
                qmp_port,
                console_log,
                uefi_code,
                uefi_vars,
            };

            match QemuProcess::spawn(&spec) {
                Ok(process) => {
                    self.process = Some(process);
                    self.ssh_host_port = Some(ssh_port);
                    self.qmp_port = Some(qmp_port);
                    self.sftp_port = Some(sftp_port);
                    self.tunnel_port = Some(tunnel_port);

                    match QmpClient::connect(qmp_port, Duration::from_secs(20)).await {
                        Ok(client) => {
                            self.qmp = Some(client);
                            self.started_at = Some(Instant::now());
                            self.status = VmStatus::Running;
                            return Ok(StartupReport {
                                accelerator,
                                accelerator_reason: reason,
                                probe,
                                ssh_host_port: ssh_port,
                                qmp_port,
                                sftp_port,
                                tunnel_port,
                                launch_secs: started.elapsed().as_secs_f64(),
                                seed_regenerated,
                            });
                        }
                        Err(e) => {
                            let stderr = self
                                .process
                                .as_mut()
                                .map(|p| p.take_stderr())
                                .unwrap_or_default();
                            if let Some(p) = self.process.as_mut() {
                                p.kill();
                            }
                            self.process = None;
                            launch_error = Some(OiError::vm(format!(
                                "QEMU started but its control channel did not come up: {e}{}",
                                if stderr.trim().is_empty() {
                                    String::new()
                                } else {
                                    format!(" (qemu: {})", stderr.trim())
                                }
                            )));
                        }
                    }
                    let _ = attempt;
                }
                Err(e) => {
                    // most likely a port collision: try again with fresh ports
                    launch_error = Some(e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }

        let err = launch_error.unwrap_or_else(|| {
            OiError::vm("the environment could not be started after several attempts")
        });
        self.status = VmStatus::StartError;
        self.last_error = Some(err.to_string());
        let mut counter = FailureCounter::load(&self.data.failure_counter());
        counter.record_failure(err.to_string());
        counter.save(&self.data.failure_counter())?;
        Err(err)
    }

    /// Record that a start really succeeded, clearing the failure counter.
    pub fn record_success(&mut self) -> OiResult<()> {
        let mut counter = FailureCounter::load(&self.data.failure_counter());
        counter.record_success();
        counter.save(&self.data.failure_counter())
    }

    /// Stop the environment: ask Linux to power off over QMP, wait, and only then fall back to
    /// killing QEMU. `force` skips the graceful wait once the caller has confirmed it with the
    /// user.
    pub async fn stop(&mut self, force: bool) -> OiResult<()> {
        if self.process.is_none() {
            self.status = VmStatus::Stopped;
            return Ok(());
        }
        self.status = VmStatus::Stopping;

        if !force {
            if let Some(qmp) = self.qmp.as_mut() {
                if let Err(e) = qmp.system_powerdown().await {
                    tracing::warn!(error = %e, "system_powerdown was refused; falling back to quit");
                    let _ = qmp.quit().await;
                }
            }
            if let Some(process) = self.process.as_mut() {
                if process.wait_timeout(Duration::from_secs(60)) {
                    self.process = None;
                    self.qmp = None;
                    self.started_at = None;
                    self.status = VmStatus::Stopped;
                    return Ok(());
                }
            }
            return Err(OiError::timeout(
                "Linux did not shut down in time; confirm a forced termination to continue",
            ));
        }

        if let Some(qmp) = self.qmp.as_mut() {
            let _ = qmp.quit().await;
        }
        if let Some(mut process) = self.process.take() {
            if !process.wait_timeout(Duration::from_secs(10)) {
                process.kill();
            }
        }
        self.qmp = None;
        self.started_at = None;
        self.status = VmStatus::Stopped;
        Ok(())
    }

    /// Last resort used when the UI process is about to exit and a graceful stop failed.
    pub fn terminate_now(&mut self) {
        if let Some(process) = self.process.as_mut() {
            if let Some(job) = process.job.as_ref() {
                let _ = job.terminate();
            }
            process.kill();
        }
        self.process = None;
        self.qmp = None;
        self.started_at = None;
        self.status = VmStatus::Stopped;
    }

    fn prepare_uefi_vars(&self, template: &std::path::Path) -> OiResult<PathBuf> {
        let target = self.data.state_dir().join("uefi-vars.fd");
        if !target.is_file() {
            std::fs::copy(template, &target).map_err(|e| OiError::io(&target, e))?;
        }
        Ok(target)
    }

    /// Rewrite the seed configuration disk so that it carries the current endpoints and keys.
    fn regenerate_seed(
        &mut self,
        sftp_port: u16,
        tunnel_port: u16,
        _accelerator: Accelerator,
    ) -> OiResult<bool> {
        let ids = crate::storage::keys::SshIdentities::new(self.data.ssh_dir());
        if !ids.exists() {
            return Err(OiError::config(
                "the SSH identities are missing; re-run initialisation",
            ));
        }
        let material = ids.load_public_material()?;
        let guest_host_private = std::fs::read_to_string(ids.guest_host_private())?;
        let guest_client_private = std::fs::read_to_string(ids.guest_client_private())?;
        let generation = self.config.seed_generation + 1;

        let spec = SeedSpec {
            schema: SEED_SCHEMA,
            generation,
            hostname: "oi-linux".into(),
            guest_user: crate::GUEST_USER.into(),
            guest_workspace: crate::GUEST_WORKSPACE.into(),
            share_transport: self
                .config
                .share_transport
                .clone()
                .unwrap_or_else(|| "reverse_tunnel".into()),
            host_sftp_address: super::seed::SLIRP_HOST_ALIAS.into(),
            host_sftp_port: sftp_port,
            reverse_tunnel_port: tunnel_port,
            client_public_key: material.client_public_openssh.clone(),
            sftp_host_public_key: material.sftp_host_public_openssh.clone(),
            authorized_keys: material.client_public_openssh.clone(),
            win_known_hosts: super::seed::win_known_hosts(
                sftp_port,
                tunnel_port,
                &material.sftp_host_public_openssh,
            ),
            guest_host_private_key: guest_host_private.trim().to_string(),
            guest_client_private_key: guest_client_private.trim().to_string(),
        };
        let generation_before = self.config.seed_generation;
        spec.write_iso(&self.data.seed_iso())?;
        self.config.seed_generation = generation;
        self.config.save(&self.data.config_json())?;
        Ok(generation != generation_before)
    }

    /// Discover QEMU using the standard search order.
    pub fn locate(qemu_override: Option<&std::path::Path>) -> OiResult<QemuInfo> {
        locate_qemu(qemu_override)
    }

    pub fn now() -> u64 {
        now_unix()
    }
}
