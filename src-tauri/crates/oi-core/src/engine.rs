//! Application engine: the orchestration layer that both the Tauri command surface and the
//! blocking integration tests drive.
//!
//! Everything that has a *product rule* attached to it lives here rather than in the UI layer,
//! so the acceptance tests exercise the same code paths the shipped application uses:
//!
//! * the Linux environment is only ever started because the user asked for it,
//! * the host share service starts before the virtual machine, because the seed disk has to
//!   carry its port,
//! * a share failure never turns into a virtual machine failure and never blocks Linux,
//! * the Windows workspace directory is never created, moved or deleted by any of this.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::share::{ModeDb, ShareConfig, ShareServer, ShareStatus};
use crate::ssh::bridge::{self, ShareTransport};
use crate::ssh::client::{GuestClient, GuestConnectOptions, TerminalEvent, TerminalSession};
use crate::storage::app_settings::AppSettings;
use crate::storage::config::{AppConfig, EnvironmentJson};
use crate::storage::images::locate_base_image;
use crate::storage::init::{self, InitRequest, InitStep};
use crate::storage::layout::DataDir;
use crate::sys::host;
use crate::timeutil::now_unix;
use crate::vm::accelerator::{locate_qemu, probe_whpx, Accelerator, AccelProbe, QemuInfo};
use crate::vm::lifecycle::{VmController, VmState, VmStatus};

/* ------------------------------------------------------------------------------------------ *
 * Snapshots handed to the UI
 * ------------------------------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmStateSnapshot {
    pub status: VmStatus,
    pub accelerator: Option<Accelerator>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub consecutive_start_failures: u32,
    pub uptime_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareStateSnapshot {
    pub status: ShareStatus,
    pub mount_point: String,
    pub sftp_port: Option<u16>,
    pub transport: Option<ShareTransport>,
    pub last_error: Option<String>,
    pub last_check_unix: Option<u64>,
}

impl Default for ShareStateSnapshot {
    fn default() -> Self {
        ShareStateSnapshot {
            status: ShareStatus::NotMounted,
            mount_point: crate::GUEST_WORKSPACE.to_string(),
            sftp_port: None,
            transport: None,
            last_error: None,
            last_check_unix: None,
        }
    }
}

impl From<VmState> for VmStateSnapshot {
    fn from(state: VmState) -> Self {
        VmStateSnapshot {
            status: state.status,
            accelerator: state.accelerator,
            pid: state.pid,
            last_error: state.last_error,
            consecutive_start_failures: state.consecutive_start_failures,
            uptime_secs: state.uptime_secs,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SshStateSnapshot {
    pub host_port: Option<u16>,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    pub initialized: bool,
    pub data_dir: String,
    pub workspace_dir: String,
    pub alpine_version: String,
    pub schema: u32,
    pub created_at: String,
    pub base_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    pub vcpu: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
    pub language: String,
    pub workspace_dir: String,
    pub data_dir: String,
    pub update_channel: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub qemu_path: Option<String>,
    pub qemu_version: Option<String>,
    pub whpx_available: bool,
    pub whpx_enabled: bool,
    pub windows_terminal_available: bool,
    pub vscode_available: bool,
    pub total_memory_mib: u64,
    pub free_disk_mib: u64,
    pub windows_build: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStateSnapshot {
    pub vm: VmStateSnapshot,
    pub share: ShareStateSnapshot,
    pub ssh: SshStateSnapshot,
    pub environment: Option<EnvironmentSnapshot>,
    pub config: Option<ConfigSnapshot>,
    pub host: HostSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightItem {
    pub id: String,
    pub label_key: String,
    pub ok: bool,
    pub detail: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightReport {
    pub ok: bool,
    pub items: Vec<PreflightItem>,
}

impl PreflightReport {
    pub fn blocked(&self) -> bool {
        self.items.iter().any(|i| i.blocking && !i.ok)
    }
}

/// Progress callback used by the initialisation wizard and by the tests.
pub type InitProgress<'a> = &'a mut dyn FnMut(InitStep);

/* ------------------------------------------------------------------------------------------ *
 * The engine
 * ------------------------------------------------------------------------------------------ */

pub struct Engine {
    settings_path: PathBuf,
    settings: AppSettings,
    data: Option<DataDir>,
    config: Option<AppConfig>,
    environment: Option<EnvironmentJson>,
    qemu: Option<QemuInfo>,
    qemu_error: Option<String>,
    probe: Option<AccelProbe>,
    controller: Option<VmController>,
    share_server: Option<ShareServer>,
    guest: Option<GuestClient>,
    ssh_ready: bool,
    share: ShareStateSnapshot,
    terminals: HashMap<String, TerminalSession>,
    next_terminal: u64,
    /// Guest port and share port of an established SSH reverse tunnel. sshd only allows one
    /// forward per port, so a remount has to reuse the existing one instead of asking for a
    /// second one.
    reverse_tunnel: Option<(u32, u16)>,
    base_image_override: Option<PathBuf>,
    qemu_override: Option<PathBuf>,
    logs_dir: PathBuf,
    recovery_attempts: u32,
    next_recovery: Option<Instant>,
    /// Held for as long as this process owns the data directory (`OILinux_PRD.md` §4.2). A second
    /// instance must fail immediately instead of racing on qcow2 writes.
    instance_lock: Option<crate::storage::state::InstanceLock>,
    lock_conflict: Option<String>,
    /// Host discovery spawns QEMU and PowerShell probes, so the result is cached; the UI asks for
    /// a snapshot every couple of seconds and must not pay for that each time.
    host_cache: Option<(Instant, HostSnapshot)>,
}

/// How long a host snapshot (accelerator probe, memory, free disk, optional tools) is reused.
const HOST_CACHE_TTL: Duration = Duration::from_secs(30);

impl Engine {
    /// Create the engine and associate the data directory remembered in the settings file.
    pub fn new(settings_path: impl Into<PathBuf>) -> Self {
        let settings_path = settings_path.into();
        let settings = AppSettings::load(&settings_path);
        let logs_dir = settings_path
            .parent()
            .map(|p| p.join("logs"))
            .unwrap_or_else(|| std::env::temp_dir().join("oi-linux-logs"));
        let mut engine = Engine {
            settings_path,
            settings,
            data: None,
            config: None,
            environment: None,
            qemu: None,
            qemu_error: None,
            probe: None,
            controller: None,
            share_server: None,
            guest: None,
            ssh_ready: false,
            share: ShareStateSnapshot::default(),
            terminals: HashMap::new(),
            next_terminal: 1,
            reverse_tunnel: None,
            base_image_override: None,
            qemu_override: None,
            logs_dir,
            recovery_attempts: 0,
            next_recovery: None,
            instance_lock: None,
            lock_conflict: None,
            host_cache: None,
        };
        engine.reopen();
        engine
    }

    pub fn with_overrides(mut self, qemu: Option<PathBuf>, base_image_dir: Option<PathBuf>) -> Self {
        self.qemu_override = qemu;
        self.base_image_override = base_image_dir;
        self.refresh_qemu();
        self
    }

    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }

    /// Where diagnostics are written: the data directory's `logs\` folder once an environment
    /// exists (`OILinux_PRD.md` §4.2), otherwise the application's own folder.
    pub fn logs_dir(&self) -> PathBuf {
        match self.data_dir_root() {
            Some(root) if root.is_dir() => root.join("logs"),
            _ => self.logs_dir.clone(),
        }
    }

    pub fn data_dir(&self) -> Option<&DataDir> {
        self.data.as_ref()
    }

    pub fn config(&self) -> Option<&AppConfig> {
        self.config.as_ref()
    }

    pub fn probe(&self) -> Option<&AccelProbe> {
        self.probe.as_ref()
    }

    /// Locate QEMU and record the outcome for both diagnostics and preflight.
    fn refresh_qemu(&mut self) {
        match locate_qemu(self.qemu_override.as_deref()) {
            Ok(info) => {
                self.qemu = Some(info);
                self.qemu_error = None;
            }
            Err(e) => {
                self.qemu = None;
                self.qemu_error = Some(e.to_string());
            }
        }
    }

    /// Load the environment pointed at by the settings file, if it is still usable.
    fn reopen(&mut self) {
        let target_lock = self
            .settings
            .data_dir_path()
            .map(|root| DataDir::new(&root).instance_lock());
        // Keep a lock that already belongs to the environment we are opening.
        let same_lock = match (&self.instance_lock, &target_lock) {
            (Some(lock), Some(path)) => lock.path() == path,
            _ => false,
        };
        if !same_lock {
            drop(self.instance_lock.take());
        }
        self.data = None;
        self.config = None;
        self.environment = None;
        self.lock_conflict = None;
        let Some(dir) = self.settings.data_dir_path() else {
            return;
        };
        let data = DataDir::new(&dir);
        // The single-instance lock only makes sense once the directory exists.
        if !same_lock && data.root().is_dir() {
            match crate::storage::state::InstanceLock::acquire(&data.instance_lock()) {
                Ok(lock) => self.instance_lock = Some(lock),
                Err(e) => {
                    tracing::error!(error = %e, "another instance owns this data directory");
                    self.lock_conflict = Some(e.to_string());
                }
            }
        }
        if data.environment_json().is_file() {
            self.environment = EnvironmentJson::load(&data.environment_json()).ok();
            self.config = AppConfig::load(&data.config_json()).ok();
        }
        self.data = Some(data);
    }

    /// Refuse to touch the environment when another process owns it.
    fn require_lock(&self) -> OiResult<()> {
        match &self.lock_conflict {
            Some(message) => Err(OiError::InvalidState(message.clone())),
            None => Ok(()),
        }
    }

    /// Own `root` before changing it. `reopen` keeps the lock when it already points at the same
    /// environment, so this is cheap for the common case.
    fn ensure_lock_for(&mut self, root: &Path) -> OiResult<()> {
        let lock_path = DataDir::new(root).instance_lock();
        if let Some(lock) = self.instance_lock.as_ref() {
            if lock.path() == lock_path {
                return Ok(());
            }
        }
        let lock = crate::storage::state::InstanceLock::acquire(&lock_path)?;
        self.instance_lock = Some(lock);
        self.lock_conflict = None;
        Ok(())
    }

    fn require_data(&self) -> OiResult<&DataDir> {
        self.data.as_ref().ok_or_else(|| {
            OiError::NotInitialized("no Linux data directory is configured yet".into())
        })
    }

    fn require_config(&self) -> OiResult<&AppConfig> {
        self.config.as_ref().ok_or_else(|| {
            OiError::NotInitialized("no Linux environment has been created or associated yet".into())
        })
    }

    /// The workspace directory in use, if an environment exists.
    pub fn workspace_dir(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|c| PathBuf::from(&c.workspace_dir))
    }

    /// True when the QEMU process belonging to this engine is alive.
    pub fn is_running(&self) -> bool {
        self.controller
            .as_ref()
            .map(|c| c.status().is_running())
            .unwrap_or(false)
    }

    /* -------------------------------------------------------------------------------------- *
     * Host and state snapshots
     * -------------------------------------------------------------------------------------- */

    pub fn host_snapshot(&mut self) -> HostSnapshot {
        if let Some((at, cached)) = &self.host_cache {
            if at.elapsed() < HOST_CACHE_TTL {
                return cached.clone();
            }
        }
        let snapshot = self.probe_host();
        self.host_cache = Some((Instant::now(), snapshot.clone()));
        snapshot
    }

    /// Force the host discovery probes to run again.
    pub fn refresh_host(&mut self) -> HostSnapshot {
        self.host_cache = None;
        self.host_snapshot()
    }

    fn probe_host(&mut self) -> HostSnapshot {
        if self.qemu.is_none() {
            self.refresh_qemu();
        }
        let qemu_path = self
            .qemu
            .as_ref()
            .map(|q| q.path.clone())
            .unwrap_or_else(|| PathBuf::from("qemu-system-x86_64"));
        let probe = probe_whpx(&qemu_path);
        let whpx_enabled = host::whpx_feature_state().unwrap_or(probe.whpx_available);
        self.probe = Some(probe.clone());
        let free_disk_mib = self
            .settings
            .data_dir_path()
            .map(|p| host::free_disk_mib(&p))
            .unwrap_or_else(|| host::free_disk_mib(&std::env::temp_dir()));
        HostSnapshot {
            qemu_path: self.qemu.as_ref().map(|q| q.path.to_string_lossy().to_string()),
            qemu_version: self.qemu.as_ref().map(|q| q.version.clone()),
            whpx_available: probe.whpx_available,
            whpx_enabled,
            windows_terminal_available: host::windows_terminal_available(),
            vscode_available: host::vscode_available(),
            total_memory_mib: host::total_memory_mib(),
            free_disk_mib,
            windows_build: host::windows_build(),
        }
    }

    /// Detect a guest that stopped on its own and reflect it in the state.
    fn poll_guest_exit(&mut self) {
        let exited = self
            .controller
            .as_mut()
            .and_then(|c| c.poll_exit().ok().flatten())
            .is_some();
        if exited {
            tracing::info!("the guest powered off");
            if let Some(server) = self.share_server.take() {
                tokio::spawn(async move { server.stop().await });
            }
            self.guest = None;
            self.ssh_ready = false;
            self.reverse_tunnel = None;
            self.share = ShareStateSnapshot::default();
        }
    }

    pub fn snapshot(&mut self) -> AppStateSnapshot {
        self.poll_guest_exit();
        let vm_state = self
            .controller
            .as_ref()
            .map(|c| VmStateSnapshot::from(c.state()))
            .unwrap_or_else(|| {
                let mut s = VmStateSnapshot {
                    status: if self.data.as_ref().map(|d| d.is_initialized()).unwrap_or(false) {
                        VmStatus::Stopped
                    } else {
                        VmStatus::Uninitialized
                    },
                    accelerator: None,
                    pid: None,
                    last_error: None,
                    consecutive_start_failures: 0,
                    uptime_secs: None,
                };
                if let Some(data) = &self.data {
                    s.consecutive_start_failures =
                        crate::storage::state::FailureCounter::load(&data.failure_counter())
                            .consecutive;
                }
                s
            });
        let environment = self
            .data
            .as_ref()
            .map(|data| {
                let initialized = data.is_initialized();
                EnvironmentSnapshot {
                    initialized,
                    data_dir: data.root().to_string_lossy().to_string(),
                    workspace_dir: self
                        .config
                        .as_ref()
                        .map(|c| c.workspace_dir.clone())
                        .unwrap_or_default(),
                    alpine_version: self
                        .environment
                        .as_ref()
                        .map(|e| e.alpine_version.clone())
                        .unwrap_or_default(),
                    schema: self.environment.as_ref().map(|e| e.schema).unwrap_or_default(),
                    created_at: self
                        .environment
                        .as_ref()
                        .map(|e| e.created_at.clone())
                        .unwrap_or_default(),
                    base_sha256: self
                        .environment
                        .as_ref()
                        .map(|e| e.base_sha256.clone())
                        .unwrap_or_default(),
                }
            })
            .filter(|env| env.initialized);
        let config = self.config.as_ref().map(|c| ConfigSnapshot {
            vcpu: c.vcpu,
            memory_mib: c.memory_mib,
            disk_gib: c.disk_gib,
            language: c.language.clone(),
            workspace_dir: c.workspace_dir.clone(),
            data_dir: self
                .data
                .as_ref()
                .map(|d| d.root().to_string_lossy().to_string())
                .unwrap_or_default(),
            update_channel: c.update_channel.clone(),
        });
        AppStateSnapshot {
            vm: vm_state,
            share: self.share.clone(),
            ssh: SshStateSnapshot {
                host_port: self.controller.as_ref().and_then(|c| c.ssh_host_port()),
                ready: self.ssh_ready,
            },
            environment,
            config,
            host: self.host_snapshot(),
        }
    }

    /* -------------------------------------------------------------------------------------- *
     * Preflight
     * -------------------------------------------------------------------------------------- */

    pub fn preflight(&mut self) -> PreflightReport {
        self.refresh_qemu();
        let mut items = Vec::new();

        let arch_ok = cfg!(target_arch = "x86_64") && cfg!(windows);
        items.push(PreflightItem {
            id: "windows_x64".into(),
            label_key: "preflight.windows_x64".into(),
            ok: arch_ok,
            detail: format!(
                "{} on {}",
                std::env::consts::ARCH,
                std::env::consts::OS
            ),
            blocking: true,
        });

        let build = host::windows_build();
        let build_ok = build
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .map(|major| major >= 10)
            .unwrap_or(true);
        items.push(PreflightItem {
            id: "windows_build".into(),
            label_key: "preflight.windows_build".into(),
            ok: build_ok,
            detail: build.clone(),
            blocking: true,
        });

        let memory = host::total_memory_mib();
        items.push(PreflightItem {
            id: "memory".into(),
            label_key: "preflight.memory".into(),
            ok: memory >= 8192,
            detail: format!("{memory} MiB"),
            blocking: true,
        });

        let data_dir = self
            .settings
            .data_dir_path()
            .unwrap_or_else(|| PathBuf::from(r"D:\OI-Linux-Data"));
        let free = host::free_disk_mib(&data_dir);
        items.push(PreflightItem {
            id: "disk_space".into(),
            label_key: "preflight.disk_space".into(),
            ok: free >= 12 * 1024,
            detail: format!("{free} MiB free for {}", data_dir.display()),
            blocking: true,
        });

        let ntfs = host::volume_is_local_ntfs(&data_dir);
        items.push(PreflightItem {
            id: "local_ntfs".into(),
            label_key: "preflight.local_ntfs".into(),
            ok: ntfs.is_ok(),
            detail: ntfs
                .err()
                .unwrap_or_else(|| format!("{} is on a local NTFS volume", data_dir.display())),
            blocking: true,
        });

        match &self.qemu {
            Some(info) => items.push(PreflightItem {
                id: "qemu".into(),
                label_key: "preflight.qemu".into(),
                ok: true,
                detail: format!("{} ({})", info.path.display(), info.version),
                blocking: true,
            }),
            None => items.push(PreflightItem {
                id: "qemu".into(),
                label_key: "preflight.qemu".into(),
                ok: false,
                detail: self
                    .qemu_error
                    .clone()
                    .unwrap_or_else(|| "QEMU was not found".into()),
                blocking: true,
            }),
        }

        let qemu_path = self
            .qemu
            .as_ref()
            .map(|q| q.path.clone())
            .unwrap_or_else(|| PathBuf::from("qemu-system-x86_64"));
        let probe = probe_whpx(&qemu_path);
        let whpx_detail = probe.detail.clone();
        self.probe = Some(probe.clone());
        items.push(PreflightItem {
            id: "whpx".into(),
            label_key: "preflight.whpx".into(),
            ok: probe.whpx_available,
            detail: if probe.whpx_available {
                whpx_detail
            } else {
                format!("{whpx_detail}; QEMU will use TCG")
            },
            blocking: false,
        });

        items.push(PreflightItem {
            id: "vscode".into(),
            label_key: "preflight.vscode".into(),
            ok: host::vscode_available(),
            detail: "Windows VS Code is only used locally; it is never bundled or required".into(),
            blocking: false,
        });

        items.push(PreflightItem {
            id: "windows_terminal".into(),
            label_key: "preflight.windows_terminal".into(),
            ok: host::windows_terminal_available(),
            detail: "the built-in terminal works without Windows Terminal".into(),
            blocking: false,
        });

        let ok = !items.iter().any(|i| i.blocking && !i.ok);
        PreflightReport { ok, items }
    }

    /* -------------------------------------------------------------------------------------- *
     * Initialisation / re-association
     * -------------------------------------------------------------------------------------- */

    pub fn initialize(
        &mut self,
        req: &InitRequest,
        progress: InitProgress<'_>,
    ) -> OiResult<InitReportLite> {
        self.require_lock()?;
        self.refresh_qemu();
        // Re-associating an existing environment must not race another instance that is using it.
        if req.relink {
            self.ensure_lock_for(&req.data_dir)?;
        }
        let qemu = self
            .qemu
            .as_ref()
            .ok_or_else(|| {
                OiError::Vm(self.qemu_error.clone().unwrap_or_else(|| {
                    "QEMU was not found; reinstall OI Linux or set OI_LINUX_QEMU".into()
                }))
            })?
            .clone();
        let (base_path, expected) = locate_base_image(self.base_image_override.as_deref())?;
        let source = crate::storage::init::BaseImageSource {
            path: base_path,
            expected_sha256: expected,
            alpine_version: crate::ALPINE_VERSION.to_string(),
        };
        let report = if req.relink {
            init::relink(&req.data_dir, &req.workspace_dir, &qemu.qemu_img, progress)?
        } else {
            init::initialize(req, &source, &qemu.qemu_img, progress)?
        };

        // Remember the environment so that the next start works without another wizard run.
        self.settings.data_dir = Some(req.data_dir.to_string_lossy().to_string());
        self.settings.language = Some(report.config.language.clone());
        self.settings.save(&self.settings_path)?;
        self.reopen();

        Ok(InitReportLite {
            all_ok: report.all_ok(),
            steps: report.steps,
            relinked: req.relink,
        })
    }

    /* -------------------------------------------------------------------------------------- *
     * Settings
     * -------------------------------------------------------------------------------------- */

    fn assert_stopped(&self) -> OiResult<()> {
        self.require_lock()?;
        if self.is_running() {
            return Err(OiError::InvalidState(
                "the Linux environment is running; stop it first".into(),
            ));
        }
        Ok(())
    }

    pub fn set_workspace_dir(&mut self, path: &str) -> OiResult<()> {
        self.assert_stopped()?;
        let data = self.require_data()?;
        if let Some(issue) = init::workspace_issue(Path::new(path)) {
            return Err(OiError::config(issue));
        }
        host::volume_is_local_ntfs(Path::new(path))
            .map_err(|detail| OiError::config(format!("the workspace directory: {detail}")))?;
        let mut cfg = self.require_config()?.clone();
        cfg.workspace_dir = path.to_string();
        cfg.validate()?;
        cfg.save(&data.config_json())?;
        self.config = Some(cfg);
        Ok(())
    }

    pub fn set_resources(&mut self, vcpu: u16, memory_mib: u32) -> OiResult<()> {
        self.assert_stopped()?;
        let data = self.require_data()?;
        let mut cfg = self.require_config()?.clone();
        cfg.set_resources(vcpu, memory_mib)?;
        cfg.save(&data.config_json())?;
        self.config = Some(cfg);
        Ok(())
    }

    pub fn resize_disk(&mut self, gib: u32) -> OiResult<()> {
        self.assert_stopped()?;
        let data = self.require_data()?.clone();
        let qemu = self.require_qemu()?;
        if gib <= 0 || gib > 512 {
            return Err(OiError::config(format!(
                "the disk size {gib} GiB is outside the supported range"
            )));
        }
        let mut cfg = self.require_config()?.clone();
        let current = cfg.disk_gib;
        if gib < current {
            return Err(OiError::config(format!(
                "the disk can only grow (currently {current} GiB)"
            )));
        }
        if gib > current {
            crate::storage::images::resize_image(&qemu.qemu_img, &data.user_qcow2(), &format!("{gib}G"))?;
        }
        cfg.disk_gib = gib;
        cfg.save(&data.config_json())?;
        self.config = Some(cfg);
        Ok(())
    }

    pub fn set_language(&mut self, language: &str) -> OiResult<()> {
        if language != "zh-CN" && language != "en-US" {
            return Err(OiError::config(format!("unsupported language: {language}")));
        }
        self.settings.language = Some(language.to_string());
        self.settings.save(&self.settings_path)?;
        if let (Some(data), Some(cfg)) = (self.data.clone(), self.config.clone()) {
            let mut cfg = cfg;
            cfg.language = language.to_string();
            cfg.validate()?;
            cfg.save(&data.config_json())?;
            self.config = Some(cfg);
        }
        Ok(())
    }

    pub fn set_update_channel(&mut self, channel: &str) -> OiResult<()> {
        if channel != "stable" && channel != "beta" {
            return Err(OiError::config(format!(
                "unsupported update channel: {channel}"
            )));
        }
        let data = self.require_data()?;
        let mut cfg = self.require_config()?.clone();
        // Channel switches never touch the Linux environment, in either direction
        // (`OILinux_PRD.md` §10.2, decision 68).
        cfg.update_channel = channel.to_string();
        cfg.validate()?;
        cfg.save(&data.config_json())?;
        self.config = Some(cfg);
        Ok(())
    }

    fn require_qemu(&self) -> OiResult<QemuInfo> {
        self.qemu.clone().ok_or_else(|| {
            OiError::Vm(self.qemu_error.clone().unwrap_or_else(|| {
                "QEMU was not found; reinstall OI Linux or set OI_LINUX_QEMU".into()
            }))
        })
    }

    /* -------------------------------------------------------------------------------------- *
     * Guest connection
     * -------------------------------------------------------------------------------------- */

    fn guest_options(&self) -> OiResult<GuestConnectOptions> {
        let data = self.require_data()?;
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| OiError::InvalidState("the Linux environment is not running".into()))?;
        let port = controller
            .ssh_host_port()
            .ok_or_else(|| OiError::InvalidState("the SSH port is not known yet".into()))?;
        let host_public_key =
            std::fs::read_to_string(data.ssh_dir().join("guest_host_ed25519.pub"))
                .map_err(|e| OiError::io(data.ssh_dir().join("guest_host_ed25519.pub"), e))?;
        Ok(GuestConnectOptions::localhost(
            port,
            crate::GUEST_USER,
            data.ssh_dir().join(crate::storage::keys::CLIENT_KEY),
            host_public_key.trim(),
        ))
    }

    /// Wait for the guest to accept SSH and keep the connection for later use.
    pub async fn connect_guest(&mut self, timeout: Duration) -> OiResult<()> {
        if self.guest.is_some() && self.ssh_ready {
            return Ok(());
        }
        let options = self.guest_options()?;
        let client = crate::ssh::client::wait_for_guest(&options, timeout).await?;
        self.guest = Some(client);
        self.ssh_ready = true;
        Ok(())
    }

    fn guest(&self) -> OiResult<&GuestClient> {
        self.guest
            .as_ref()
            .ok_or_else(|| OiError::Ssh("there is no open session to the guest".into()))
    }

    /// The open guest session, if any. Exposed for diagnostics and for the acceptance tests,
    /// which drive the very same connection the application uses.
    pub fn guest_client(&self) -> Option<&GuestClient> {
        self.guest.as_ref()
    }

    /// The port the host share service is listening on, once it is running.
    pub fn share_port(&self) -> Option<u16> {
        self.share.sftp_port.or_else(|| {
            self.controller
                .as_ref()
                .and_then(|c| c.sftp_port())
        })
    }

    pub fn ssh_port(&self) -> Option<u16> {
        self.controller.as_ref().and_then(|c| c.ssh_host_port())
    }

    /// Stop the host share listener without touching the virtual machine. Used to simulate the
    /// share service dying, which by product rule must leave Linux perfectly usable
    /// (`OILinux_PRD.md` §7.4, §11.2 item 7).
    pub async fn stop_share_service(&mut self) {
        if let Some(server) = self.share_server.take() {
            server.stop().await;
        }
        // The forward points at a listener that no longer exists, and sshd would refuse a second
        // request on the same port anyway.
        if let Some((guest_port, _)) = self.reverse_tunnel.take() {
            if let Ok(client) = self.guest_mut() {
                let _ = client.cancel_reverse_forward(guest_port).await;
            }
        }
        self.share.status = ShareStatus::Degraded;
        self.share.last_error = Some("the host share service was stopped".into());
        self.share.last_check_unix = Some(now_unix());
    }

    /// Bring the share service back and remount, which is what "restore the service and verify
    /// read/write" means in the acceptance list.
    pub async fn restart_share_service_and_remount(&mut self) -> OiResult<()> {
        self.start_share_service().await?;
        self.remount().await
    }

    fn guest_mut(&mut self) -> OiResult<&mut GuestClient> {
        self.guest
            .as_mut()
            .ok_or_else(|| OiError::Ssh("there is no open session to the guest".into()))
    }

    /* -------------------------------------------------------------------------------------- *
     * Share
     * -------------------------------------------------------------------------------------- */

    fn set_share(&mut self, status: ShareStatus, error: Option<String>) {
        self.share.status = status;
        self.share.last_error = error;
        self.share.last_check_unix = Some(now_unix());
    }

    /// The port of the share listener that is running *now*. After a restart that is a new port,
    /// so the controller's launch-time value must not be used.
    fn current_share_port(&self) -> Option<u16> {
        self.share_server
            .as_ref()
            .map(|server| server.port)
            .or_else(|| self.controller.as_ref().and_then(|c| c.sftp_port()))
    }

    /// Decide how the guest reaches the host share service. The direct route through QEMU's
    /// user-mode host alias is probed for real; when it does not work the reverse tunnel is used,
    /// which travels inside the SSH session the host already owns.
    async fn negotiate_transport(&mut self) -> OiResult<(ShareTransport, u16)> {
        let sftp_port = self
            .current_share_port()
            .ok_or_else(|| OiError::InvalidState("the share port is not known yet".into()))?;
        let tunnel_port = self
            .controller
            .as_ref()
            .and_then(|c| c.tunnel_port())
            .ok_or_else(|| OiError::InvalidState("the tunnel port is not known yet".into()))?;

        let preferred = self
            .config
            .as_ref()
            .and_then(|c| c.share_transport.clone());

        // An established tunnel survives a remount: ask for the same port again and sshd refuses
        // it because it is still bound.
        if let Some((guest_port, share_port)) = self.reverse_tunnel {
            if share_port == sftp_port {
                tracing::debug!(guest_port, share_port, "reusing the established reverse tunnel");
                return Ok((ShareTransport::ReverseTunnel, guest_port as u16));
            }
            // The share listener was replaced; close the stale forward first.
            if let Ok(client) = self.guest_mut() {
                let _ = client.cancel_reverse_forward(guest_port).await;
            }
            self.reverse_tunnel = None;
        }

        if preferred.as_deref() != Some("reverse_tunnel") {
            let probe = bridge::probe_direct(self.guest()?, sftp_port).await;
            tracing::info!(reachable = probe.reachable, detail = %probe.detail, "direct share probe");
            if probe.reachable {
                return Ok((ShareTransport::Direct, sftp_port));
            }
        }

        let requested = tunnel_port;
        let client = self.guest_mut()?;
        let bound = client.open_reverse_forward(requested as u32, sftp_port).await?;
        // OpenSSH only reports a port when the request asked for port 0; for an explicit port the
        // reply carries no payload, so the requested port is the authoritative one.
        let guest_port = if requested == 0 { bound as u16 } else { requested };
        self.reverse_tunnel = Some((guest_port as u32, sftp_port));
        Ok((ShareTransport::ReverseTunnel, guest_port))
    }

    /// Mount the workspace and verify it with a real read/write/execute test.
    pub async fn mount_share(&mut self) -> OiResult<()> {
        self.set_share(ShareStatus::Mounting, None);
        let sftp_port = self
            .current_share_port()
            .ok_or_else(|| OiError::InvalidState("the share port is not known yet".into()))?;
        let (transport, port) = match self.negotiate_transport().await {
            Ok(pair) => pair,
            Err(e) => {
                self.set_share(ShareStatus::Error, Some(e.to_string()));
                return Err(e);
            }
        };

        let client = self.guest()?;
        if let Err(e) = bridge::mount(client, transport, port).await {
            self.set_share(ShareStatus::Error, Some(e.to_string()));
            return Err(e);
        }

        let nonce = bridge::new_nonce();
        let client = self.guest()?;
        let report = match bridge::health_check(client, &nonce).await {
            Ok(report) => report,
            Err(e) => {
                self.set_share(ShareStatus::Degraded, Some(e.to_string()));
                return Err(e);
            }
        };
        if !report.healthy() {
            // The mount exists but does not behave; take it away so that the guest cannot write
            // into a half-working share (`OILinux_PRD.md` §7.4).
            if let Ok(client) = self.guest() {
                let _ = bridge::quarantine(client).await;
            }
            self.set_share(ShareStatus::Degraded, Some(report.detail.clone()));
            return Err(OiError::Bridge(format!(
                "the shared workspace did not pass its read/write/permission check: {}",
                report.detail
            )));
        }

        self.share.sftp_port = Some(sftp_port);
        self.share.transport = Some(transport);
        self.share.status = ShareStatus::Mounted;
        self.share.last_error = None;
        self.share.last_check_unix = Some(now_unix());
        self.recovery_attempts = 0;
        self.next_recovery = None;

        // Remember which transport worked, so the next launch tries it first.
        if let (Some(data), Some(mut cfg)) = (self.data.clone(), self.config.clone()) {
            cfg.share_transport = Some(transport.as_str().to_string());
            if cfg.save(&data.config_json()).is_ok() {
                self.config = Some(cfg);
            }
        }
        Ok(())
    }

    pub async fn start_share_service(&mut self) -> OiResult<u16> {
        if let Some(server) = self.share_server.as_ref() {
            return Ok(server.port);
        }
        let data = self.require_data()?.clone();
        let config = self.require_config()?.clone();
        let root = PathBuf::from(&config.workspace_dir);
        let db = Arc::new(ModeDb::open(&data.metadata_db())?);
        db.verify()?;
        let guest_public = std::fs::read_to_string(data.ssh_dir().join("guest_client_ed25519.pub"))
            .map_err(|e| OiError::io(data.ssh_dir().join("guest_client_ed25519.pub"), e))?;

        let mut last_error = None;
        for _ in 0..8 {
            let port = crate::vm::ports::allocate_loopback_port()?;
            if !crate::vm::ports::is_free(port) {
                continue;
            }
            let cfg = ShareConfig {
                root: root.clone(),
                ssh_host_key: data.ssh_dir().join("sftp_host_ed25519"),
                authorized_public_key: guest_public.trim().to_string(),
                port,
            };
            match ShareServer::start(cfg, db.clone()).await {
                Ok((server, mut events)) => {
                    tokio::spawn(async move {
                        while events.recv().await.is_ok() {}
                    });
                    self.share_server = Some(server);
                    self.share.sftp_port = Some(port);
                    return Ok(port);
                }
                Err(e) => last_error = Some(e),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            OiError::ShareUnavailable("no loopback port could be bound for the share service".into())
        }))
    }

    /* -------------------------------------------------------------------------------------- *
     * Launch / stop
     * -------------------------------------------------------------------------------------- */

    /// Start the Linux environment. Returns the accelerator summary for diagnostics.
    pub async fn start_vm(&mut self) -> OiResult<StartOutcome> {
        self.require_lock()?;
        if self.is_running() {
            return Err(OiError::InvalidState("running".into()));
        }
        let data = self.require_data()?.clone();
        let config = self.require_config()?.clone();
        let qemu = self.require_qemu()?;
        if !data.is_initialized() {
            return Err(OiError::NotInitialized(
                "create or associate a Linux environment first".into(),
            ));
        }

        // 1. the share service first: the seed disk has to carry its port
        let share_port = self.start_share_service().await?;
        let mut tunnel_port = crate::vm::ports::allocate_loopback_port()?;
        let mut tries = 0;
        while tunnel_port == share_port || !crate::vm::ports::is_free(tunnel_port) {
            tunnel_port = crate::vm::ports::allocate_loopback_port()?;
            tries += 1;
            if tries > 32 {
                return Err(OiError::Other(
                    "could not allocate a distinct tunnel port".into(),
                ));
            }
        }

        // 2. the launcher, told which ports to write into the seed disk
        let mut controller = VmController::new(data.clone(), qemu, config);
        controller.set_reserved_ports(share_port, tunnel_port);

        let report = match controller.start().await {
            Ok(report) => report,
            Err(e) => {
                // a launch failure is *not* a share failure; the share service stays available for
                // the next attempt (and is stopped here because no guest can use it)
                if let Some(server) = self.share_server.take() {
                    server.stop().await;
                }
                self.controller = Some(controller);
                self.set_share(ShareStatus::NotMounted, None);
                return Err(e);
            }
        };
        self.controller = Some(controller);

        // 3. wait for SSH; a share problem must not be able to make this fail
        if let Err(e) = self.connect_guest(Duration::from_secs(180)).await {
            tracing::error!(error = %e, "the guest never became reachable over SSH");
            self.share.status = ShareStatus::Error;
            self.share.last_error = Some(format!(
                "Linux started but did not become reachable over SSH: {e}"
            ));
            return Err(e);
        }

        // 4. the start really succeeded -> clear the failure counter
        if let Some(c) = self.controller.as_mut() {
            let _ = c.record_success();
        }

        // 5. bring the shared workspace up. A failure here is reported as a share problem and
        //    leaves the Linux environment perfectly usable.
        if let Err(e) = self.mount_share().await {
            tracing::warn!(error = %e, "the shared workspace is not available; Linux keeps running");
        }

        Ok(StartOutcome {
            accelerator: report.accelerator,
            accelerator_reason: report.accelerator_reason,
            whpx_probe_detail: report.probe.detail.clone(),
            ssh_host_port: report.ssh_host_port,
            sftp_port: report.sftp_port,
            tunnel_port: report.tunnel_port,
        })
    }

    /// Power the guest off. The SSH path is tried first, then QMP, and only a confirmed user
    /// request skips the graceful wait.
    pub async fn stop_vm(&mut self, force: bool) -> OiResult<()> {
        let Some(controller) = self.controller.as_mut() else {
            self.share = ShareStateSnapshot::default();
            self.ssh_ready = false;
            return Ok(());
        };
        if controller.status() == VmStatus::Stopped {
            if let Some(server) = self.share_server.take() {
                server.stop().await;
            }
            self.guest = None;
            self.ssh_ready = false;
            return Ok(());
        }

        if !force {
            // 1. ask Linux to power off over SSH
            if let Some(client) = self.guest.as_ref() {
                let _ = client
                    .exec_with_timeout("sudo -n /sbin/poweroff 2>/dev/null || sudo -n poweroff 2>/dev/null || true", Duration::from_secs(20))
                    .await;
            }
            // 2. wait for QEMU to exit; QMP is the fallback when SSH never answered
            let deadline = Instant::now() + Duration::from_secs(45);
            let mut sent_qmp = self.guest.is_none();
            loop {
                if let Some(c) = self.controller.as_mut() {
                    if c.poll_exit().ok().flatten().is_some() {
                        break;
                    }
                    if !sent_qmp && Instant::now() + Duration::from_secs(40) > deadline {
                        let _ = c.stop(false).await;
                        sent_qmp = true;
                    }
                }
                if Instant::now() >= deadline {
                    // The product rule is to ask before forcing anything: the environment stays in
                    // the stopping state and the user decides (`OILinux_PRD.md` §6.3).
                    return Err(OiError::timeout(
                        "Linux did not shut down in time; confirm a forced termination to continue",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        } else if let Some(c) = self.controller.as_mut() {
            c.stop(true).await?;
        }

        if let Some(server) = self.share_server.take() {
            server.stop().await;
        }
        self.guest = None;
        self.ssh_ready = false;
        self.reverse_tunnel = None;
        self.share = ShareStateSnapshot::default();
        self.terminals.clear();
        Ok(())
    }

    /// Rebuild the writable layer. The Windows workspace and the base image are not touched.
    pub fn reset_environment(&mut self, progress: InitProgress<'_>) -> OiResult<()> {
        self.assert_stopped()?;
        let data = self.require_data()?.clone();
        let qemu = self.require_qemu()?;
        init::reset(data.root(), &qemu.qemu_img, progress)?;
        self.reopen();
        Ok(())
    }

    /* -------------------------------------------------------------------------------------- *
     * Recovery
     * -------------------------------------------------------------------------------------- */

    /// One recovery step, called by the UI timer (and by the tests). Backoff follows
    /// 1, 2, 4, 8, 16, 30 seconds per round; a failure never triggers a VM reset.
    pub async fn recover_share(&mut self) -> Option<OiResult<()>> {
        if !self.is_running() || self.guest.is_none() {
            return None;
        }
        let due = self.next_recovery.map(|t| Instant::now() >= t).unwrap_or(true);
        if !due {
            return None;
        }
        if self.share.status == ShareStatus::Mounted {
            return None;
        }
        self.share.status = ShareStatus::Recovering;
        let outcome = self.mount_share().await;
        match &outcome {
            Ok(()) => {
                self.recovery_attempts = 0;
                self.next_recovery = None;
            }
            Err(_) => {
                self.recovery_attempts = self.recovery_attempts.saturating_add(1);
                let secs = [1u64, 2, 4, 8, 16, 30][(self.recovery_attempts as usize - 1).min(5)];
                self.next_recovery = Some(Instant::now() + Duration::from_secs(secs));
                self.share.status = ShareStatus::Degraded;
            }
        }
        Some(outcome)
    }

    /// Manual "retry mount": the user pressed the button, so it runs immediately.
    pub async fn remount(&mut self) -> OiResult<()> {
        if !self.is_running() {
            return Err(OiError::InvalidState(
                "start the Linux environment first".into(),
            ));
        }
        if self.guest.is_none() {
            // a guest reboot drops the session; reconnecting is part of the manual retry
            self.connect_guest(Duration::from_secs(60)).await?;
        }
        self.recovery_attempts = 0;
        self.next_recovery = None;
        self.mount_share().await
    }

    /* -------------------------------------------------------------------------------------- *
     * Terminals
     * -------------------------------------------------------------------------------------- */

    pub async fn open_terminal(
        &mut self,
        cols: u32,
        rows: u32,
        events: tokio::sync::mpsc::UnboundedSender<TerminalEvent>,
    ) -> OiResult<String> {
        if self.guest.is_none() {
            self.connect_guest(Duration::from_secs(30)).await?;
        }
        let id = format!("t{}", self.next_terminal);
        self.next_terminal += 1;
        let client = self.guest()?;
        let session = client.open_terminal(id.clone(), cols, rows, events).await?;
        self.terminals.insert(id.clone(), session);
        Ok(id)
    }

    pub async fn terminal_write(&mut self, id: &str, data: &[u8]) -> OiResult<()> {
        let session = self
            .terminals
            .get(id)
            .ok_or_else(|| OiError::Ssh(format!("unknown terminal session {id}")))?;
        session.write(data).await
    }

    pub async fn terminal_resize(&mut self, id: &str, cols: u32, rows: u32) -> OiResult<()> {
        let session = self
            .terminals
            .get(id)
            .ok_or_else(|| OiError::Ssh(format!("unknown terminal session {id}")))?;
        session.resize(cols, rows).await
    }

    pub async fn terminal_close(&mut self, id: &str) -> OiResult<()> {
        if let Some(session) = self.terminals.remove(id) {
            session.close().await;
        }
        Ok(())
    }

    pub fn terminal_count(&self) -> usize {
        self.terminals.len()
    }

    /* -------------------------------------------------------------------------------------- *
     * Diagnostics
     * -------------------------------------------------------------------------------------- */

    pub fn list_logs(&self) -> OiResult<Vec<crate::logging::LogFileInfo>> {
        crate::logging::list_logs(&self.logs_dir())
    }

    pub fn export_logs(&self, dest: &Path) -> OiResult<Vec<String>> {
        crate::logging::export_logs(&self.logs_dir(), dest)
    }

    /// Stop everything the engine owns. Used when the application window closes.
    pub async fn shutdown(&mut self) {
        let _ = self.stop_vm(true).await;
        if let Some(server) = self.share_server.take() {
            server.stop().await;
        }
        if let Some(client) = self.guest.take() {
            client.close().await;
        }
    }

    /// The channel used by the UI timer to notice a guest that stopped on its own.
    pub fn tick(&mut self) {
        self.poll_guest_exit();
    }

    /* -------------------------------------------------------------------------------------- *
     * External tools
     * -------------------------------------------------------------------------------------- */

    /// Write the SSH material Windows Terminal uses. Both files live in the data directory,
    /// which is restricted to the current Windows user, so the private key is never exposed
    /// through the shared workspace (`OILinux_PRD.md` §5.3).
    pub fn write_external_ssh_config(&self) -> OiResult<PathBuf> {
        let data = self.require_data()?;
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| OiError::InvalidState("the Linux environment is not running".into()))?;
        let port = controller
            .ssh_host_port()
            .ok_or_else(|| OiError::InvalidState("the SSH port is not known yet".into()))?;
        let dir = data.ssh_dir().join("windows");
        std::fs::create_dir_all(&dir).map_err(|e| OiError::io(&dir, e))?;

        let host_public_key =
            std::fs::read_to_string(data.ssh_dir().join("guest_host_ed25519.pub"))
                .map_err(|e| OiError::io(data.ssh_dir().join("guest_host_ed25519.pub"), e))?;
        let private_key = data.ssh_dir().join(crate::storage::keys::CLIENT_KEY);

        let known_hosts = dir.join("known_hosts");
        std::fs::write(
            &known_hosts,
            crate::storage::keys::known_hosts_line("127.0.0.1", port, host_public_key.trim()),
        )
        .map_err(|e| OiError::io(&known_hosts, e))?;

        let config = dir.join("config");
        let body = format!(
            "# Written by OI Linux. Regenerated on every launch because the port is dynamic.\n\
             Host oi-linux\n\
             \x20 HostName 127.0.0.1\n\
             \x20 Port {port}\n\
             \x20 User {user}\n\
             \x20 IdentityFile \"{key}\"\n\
             \x20 IdentitiesOnly yes\n\
             \x20 StrictHostKeyChecking yes\n\
             \x20 UserKnownHostsFile \"{known}\"\n\
             \x20 GlobalKnownHostsFile NONE\n\
             \x20 ServerAliveInterval 30\n",
            user = crate::GUEST_USER,
            key = private_key.display(),
            known = known_hosts.display(),
        );
        std::fs::write(&config, body.as_bytes()).map_err(|e| OiError::io(&config, e))?;
        crate::sys::acl::restrict_tree_to_current_user(&dir)?;
        Ok(config)
    }

    /// Open Windows Terminal on the running environment. The button is disabled in the UI when
    /// Windows Terminal is missing, so this is only reached when it exists.
    pub fn open_external_terminal(&mut self) -> OiResult<()> {
        let config = self.write_external_ssh_config()?;
        let wt = host::which("wt.exe").ok_or_else(|| {
            OiError::Other("Windows Terminal is not installed; the built-in terminal works".into())
        })?;
        host::open(
            &wt,
            &[
                "new-tab",
                "--title",
                "OI Linux",
                "ssh.exe",
                "-F",
                &config.to_string_lossy(),
                "oi-linux",
            ],
        )
    }

    /// Open the Windows workspace directory in VS Code, if VS Code is installed.
    pub fn open_vscode(&mut self) -> OiResult<()> {
        let workspace = self
            .workspace_dir()
            .ok_or_else(|| OiError::NotInitialized("no workspace directory is configured".into()))?;
        let code = host::which("code.exe").or_else(|| {
            let mut fallbacks = Vec::new();
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                fallbacks.push(
                    PathBuf::from(local)
                        .join("Programs")
                        .join("Microsoft VS Code")
                        .join("Code.exe"),
                );
            }
            fallbacks.push(PathBuf::from(r"C:\Program Files\Microsoft VS Code\Code.exe"));
            fallbacks.into_iter().find(|p| p.is_file())
        });
        let code = code.ok_or_else(|| {
            OiError::Other(
                "Windows VS Code was not found; install it to edit the workspace locally".into(),
            )
        })?;
        host::open(&code, &[&workspace.to_string_lossy()])
    }

    pub fn data_dir_root(&self) -> Option<PathBuf> {
        self.data.as_ref().map(|d| d.root().to_path_buf())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartOutcome {
    pub accelerator: Accelerator,
    pub accelerator_reason: String,
    pub whpx_probe_detail: String,
    pub ssh_host_port: u16,
    pub sftp_port: u16,
    pub tunnel_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitReportLite {
    pub steps: Vec<InitStep>,
    pub all_ok: bool,
    pub relinked: bool,
}
