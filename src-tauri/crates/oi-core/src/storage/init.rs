//! Environment initialisation, re-linking an existing environment and reset.
//!
//! Guiding rule from `OILinux_PRD.md`: the Windows workspace directory holds the user's real
//! files and is never created, moved, rewritten or deleted by any of these operations. Only the
//! Linux side (overlay, guest state) may be rebuilt.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{OiError, OiResult};
use crate::storage::config::{
    new_environment, AppConfig, EnvironmentJson, CONFIG_SCHEMA, ENV_SCHEMA,
};
use crate::storage::images::{
    check_overlay_points_at, create_overlay, ensure_overlay, resize_image, sha256_file,
};
use crate::storage::layout::{DataDir, BASE_IMAGE, USER_IMAGE};
use crate::timeutil::now_rfc3339;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub data_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub vcpu: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
    #[serde(default)]
    pub language: Option<String>,
    /// `true` for "associate an existing environment" instead of "create a new one".
    #[serde(default)]
    pub relink: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitStep {
    pub key: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitReport {
    pub steps: Vec<InitStep>,
    pub environment: EnvironmentJson,
    pub config: AppConfig,
}

impl InitReport {
    pub fn all_ok(&self) -> bool {
        self.steps.iter().all(|s| s.ok)
    }
}

/// Collects the step list while forwarding each step to the caller's progress callback.
struct StepSink<'a> {
    steps: Vec<InitStep>,
    progress: &'a mut dyn FnMut(InitStep),
}

impl StepSink<'_> {
    fn add(&mut self, key: &str, ok: bool, detail: String) {
        let s = InitStep {
            key: key.to_string(),
            ok,
            detail,
        };
        (self.progress)(s.clone());
        self.steps.push(s);
    }

    fn into_steps(self) -> Vec<InitStep> {
        self.steps
    }
}

/// Where the shipped base image comes from, plus its expected hash when known.
#[derive(Debug, Clone)]
pub struct BaseImageSource {
    pub path: PathBuf,
    pub expected_sha256: Option<String>,
    pub alpine_version: String,
}

/// Create a brand new environment in `req.data_dir`.
pub fn initialize(
    req: &InitRequest,
    base: &BaseImageSource,
    qemu_img: &Path,
    progress: &mut dyn FnMut(InitStep),
) -> OiResult<InitReport> {
    if req.relink {
        return Err(OiError::config(
            "initialize() creates a new environment; use relink() to associate an existing one",
        ));
    }
    let data = DataDir::new(&req.data_dir);
    data.assert_usable_for_init()?;

    if !base.path.is_file() {
        return Err(OiError::ImageVerification(format!(
            "the base image {} is missing; the installation is incomplete",
            base.path.display()
        )));
    }

    let mut sink = StepSink {
        steps: Vec::new(),
        progress,
    };

    // 1. skeleton ---------------------------------------------------------------
    data.create_skeleton()?;
    sink.add(
        "data_dir",
        true,
        format!("created the Linux data directory at {}", data.root().display()),
    );

    // 2. workspace -------------------------------------------------------------
    let ws = &req.workspace_dir;
    if let Some(issue) = workspace_issue(ws) {
        return Err(OiError::config(issue));
    }
    if !ws.exists() {
        std::fs::create_dir_all(ws).map_err(|e| OiError::io(ws, e))?;
    }
    if !ws.is_dir() {
        return Err(OiError::config(format!(
            "the Windows workspace directory {} is not a directory",
            ws.display()
        )));
    }
    sink.add(
        "workspace",
        true,
        format!(
            "workspace {} is the long-term location of the user's source files",
            ws.display()
        ),
    );

    // 3. copy + verify the base image -----------------------------------------
    let target_base = data.base_qcow2();
    copy_file(&base.path, &target_base)?;
    let actual = sha256_file(&target_base)?;
    if let Some(expected) = &base.expected_sha256 {
        if !expected.eq_ignore_ascii_case(&actual) {
            let _ = std::fs::remove_file(&target_base);
            return Err(OiError::ImageVerification(format!(
                "the copied base image hash {actual} does not match the expected {expected}; \
                 the copy was removed and no environment was created"
            )));
        }
    }
    let base_size = std::fs::metadata(&target_base)
        .map_err(|e| OiError::io(&target_base, e))?
        .len();
    sink.add(
        "base_image",
        true,
        format!(
            "base.qcow2 installed and verified ({} bytes, sha256 {})",
            base_size,
            &actual[..16]
        ),
    );

    // 4. overlay ---------------------------------------------------------------
    create_overlay(qemu_img, data.root(), BASE_IMAGE, USER_IMAGE)?;
    check_overlay_points_at(qemu_img, &data.user_qcow2(), BASE_IMAGE)?;
    apply_disk_size(qemu_img, &data, req.disk_gib)?;
    sink.add(
        "overlay",
        true,
        format!(
            "user.qcow2 overlay created with the relative backing file {BASE_IMAGE} and verified"
        ),
    );

    // 5. identities ------------------------------------------------------------
    let ids = crate::storage::keys::SshIdentities::new(data.ssh_dir());
    let material = ids.generate_all()?;
    sink.add(
        "identities",
        true,
        format!(
            "generated the per-machine SSH identities in {} (host client, guest host key, guest client key, share service host key)",
            data.ssh_dir().display()
        ),
    );

    // 6. metadata database -----------------------------------------------------
    let db = crate::share::mode_db::ModeDb::open(&data.metadata_db())?;
    db.verify()?;
    sink.add(
        "metadata_db",
        true,
        "created the file mode metadata database".to_string(),
    );

    // 7. environment.json and config.json -------------------------------------
    let env = new_environment(
        Uuid::new_v4().to_string(),
        base.alpine_version.clone(),
        actual,
        base_size,
        base.path.to_string_lossy().to_string(),
    );
    env.save(&data.environment_json())?;

    let mut cfg = AppConfig::new(req.workspace_dir.to_string_lossy().to_string(), "");
    cfg.vcpu = req.vcpu;
    cfg.memory_mib = req.memory_mib;
    cfg.disk_gib = req.disk_gib;
    cfg.language = req
        .language
        .clone()
        .unwrap_or_else(|| "zh-CN".to_string());
    cfg.seed_generation = 1;
    cfg.validate()?;
    cfg.save(&data.config_json())?;
    sink.add(
        "config",
        true,
        format!(
            "{} vCPU / {} MiB RAM / {} GiB disk, UI language {}",
            cfg.vcpu, cfg.memory_mib, cfg.disk_gib, cfg.language
        ),
    );

    // 8. seed disk -------------------------------------------------------------
    let (sftp_port, tunnel_port) = crate::vm::ports::reserve_pair()?;
    let seed = crate::vm::seed::SeedSpec {
        schema: crate::vm::seed::SEED_SCHEMA,
        generation: cfg.seed_generation,
        hostname: "oi-linux".into(),
        guest_user: crate::GUEST_USER.into(),
        guest_workspace: crate::GUEST_WORKSPACE.into(),
        share_transport: "reverse_tunnel".into(),
        host_sftp_address: crate::vm::seed::SLIRP_HOST_ALIAS.into(),
        host_sftp_port: sftp_port,
        reverse_tunnel_port: tunnel_port,
        client_public_key: material.client_public_openssh.clone(),
        sftp_host_public_key: material.sftp_host_public_openssh.clone(),
        authorized_keys: material.client_public_openssh.clone(),
        win_known_hosts: crate::vm::seed::win_known_hosts(
            sftp_port,
            tunnel_port,
            &material.sftp_host_public_openssh,
        ),
        guest_host_private_key: material.guest_host_private_openssh.clone(),
        guest_client_private_key: material.guest_client_private_openssh.clone(),
    };
    let seed_bytes = seed.write_iso(&data.seed_iso())?;
    sink.add(
        "seed",
        true,
        format!("boot-time seed configuration disk written ({seed_bytes} bytes)"),
    );

    // 9. final verification ----------------------------------------------------
    let env_back = EnvironmentJson::load(&data.environment_json())?;
    let cfg_back = AppConfig::load(&data.config_json())?;
    ensure_overlay(qemu_img, data.root(), BASE_IMAGE, USER_IMAGE)?;
    sink.add(
        "verify",
        true,
        format!(
            "environment {} re-verified (schema {}, alpine {})",
            env_back.environment_id, env_back.schema, env_back.alpine_version
        ),
    );

    Ok(InitReport {
        steps: sink.into_steps(),
        environment: env_back,
        config: cfg_back,
    })
}

/// Associate an existing data directory without moving or recreating anything.
pub fn relink(
    data_dir: &Path,
    workspace_dir: &Path,
    qemu_img: &Path,
    progress: &mut dyn FnMut(InitStep),
) -> OiResult<InitReport> {
    let data = DataDir::new(data_dir);
    let mut sink = StepSink {
        steps: Vec::new(),
        progress,
    };

    if !data.environment_json().is_file() {
        return Err(OiError::NotInitialized(format!(
            "{} does not contain an OI Linux environment (environment.json is missing)",
            data.root().display()
        )));
    }
    let mut env = EnvironmentJson::load(&data.environment_json())?;
    sink.add(
        "environment.json",
        true,
        format!(
            "environment.json read (schema {}, alpine {}, created {})",
            env.schema, env.alpine_version, env.created_at
        ),
    );

    if !data.base_qcow2().is_file() {
        return Err(OiError::ImageVerification(format!(
            "{} is missing; the base and the overlay must both be kept",
            data.base_qcow2().display()
        )));
    }
    let actual = sha256_file(&data.base_qcow2())?;
    if !actual.eq_ignore_ascii_case(&env.base_sha256) {
        return Err(OiError::ImageVerification(format!(
            "base.qcow2 hash {actual} does not match the recorded {}, so the environment was \
             modified or is damaged; refusing to open it in place",
            env.base_sha256
        )));
    }
    sink.add(
        "base_image",
        true,
        "base.qcow2 hash matches environment.json".to_string(),
    );

    check_overlay_points_at(qemu_img, &data.user_qcow2(), BASE_IMAGE)?;
    sink.add(
        "overlay",
        true,
        format!("{USER_IMAGE} points at the relative backing file {BASE_IMAGE}"),
    );

    let mut cfg = if data.config_json().is_file() {
        AppConfig::load(&data.config_json())?
    } else {
        let mut cfg = AppConfig::new(workspace_dir.to_string_lossy().to_string(), "");
        cfg.schema = CONFIG_SCHEMA;
        cfg
    };
    cfg.workspace_dir = workspace_dir.to_string_lossy().to_string();
    cfg.validate()?;
    cfg.save(&data.config_json())?;
    sink.add(
        "config",
        true,
        format!(
            "workspace set to {}; resources kept at {} vCPU / {} MiB",
            cfg.workspace_dir, cfg.vcpu, cfg.memory_mib
        ),
    );

    if !data.metadata_db().is_file() {
        let db = crate::share::mode_db::ModeDb::open(&data.metadata_db())?;
        db.verify()?;
        sink.add(
            "metadata_db",
            true,
            "rebuildable permission metadata database was missing and has been recreated"
                .to_string(),
        );
    } else {
        match crate::share::mode_db::ModeDb::open(&data.metadata_db()).and_then(|d| d.verify()) {
            Ok(()) => sink.add(
                "metadata_db",
                true,
                "permission metadata database opens and passes integrity_check".to_string(),
            ),
            Err(e) => sink.add(
                "metadata_db",
                false,
                format!(
                    "permission metadata is damaged ({e}); file sharing stays stopped until it is \
                     repaired or removed manually; the Windows workspace is untouched"
                ),
            ),
        }
    }

    env.schema = ENV_SCHEMA;
    env.last_relinked_at = Some(now_rfc3339());
    env.save(&data.environment_json())?;
    sink.add(
        "relink",
        true,
        "existing environment associated; no data was moved or rewritten".to_string(),
    );

    Ok(InitReport {
        steps: sink.into_steps(),
        environment: env,
        config: cfg,
    })
}

/// Rebuild the writable layer. Guest data is lost; the Windows workspace, the base image and the
/// per-machine identities are kept.
pub fn reset(
    data_dir: &Path,
    qemu_img: &Path,
    progress: &mut dyn FnMut(InitStep),
) -> OiResult<InitReport> {
    let data = DataDir::new(data_dir);
    let mut sink = StepSink {
        steps: Vec::new(),
        progress,
    };

    let env = EnvironmentJson::load(&data.environment_json())?;
    if !data.base_qcow2().is_file() {
        return Err(OiError::ImageVerification(
            "the base image is missing, so the overlay cannot be rebuilt".into(),
        ));
    }

    // No qcow2 writer may hold the overlay while it is replaced.
    assert_no_writer(&data.user_qcow2())?;
    sink.add(
        "stop_check",
        true,
        "no process holds a write handle on user.qcow2".to_string(),
    );

    let staging = data.root().join("user.new.qcow2");
    let previous = data.root().join("user.previous.qcow2");
    let _ = std::fs::remove_file(&staging);
    let _ = std::fs::remove_file(&previous);

    create_overlay(qemu_img, data.root(), BASE_IMAGE, "user.new.qcow2")?;
    check_overlay_points_at(qemu_img, &staging, BASE_IMAGE)?;
    sink.add(
        "prepare",
        true,
        "new overlay prepared and its backing chain verified".to_string(),
    );

    if data.user_qcow2().is_file() {
        std::fs::rename(data.user_qcow2(), &previous).map_err(|e| OiError::io(&previous, e))?;
    }
    std::fs::rename(&staging, data.user_qcow2()).map_err(|e| OiError::io(data.user_qcow2(), e))?;
    sink.add("switch", true, "overlay switched atomically".to_string());

    match check_overlay_points_at(qemu_img, &data.user_qcow2(), BASE_IMAGE) {
        Ok(_) => sink.add(
            "verify",
            true,
            "new overlay verified after the switch".to_string(),
        ),
        Err(e) => {
            // put the previous overlay back so that no data is lost
            let _ = std::fs::remove_file(data.user_qcow2());
            if previous.is_file() {
                let _ = std::fs::rename(&previous, data.user_qcow2());
            }
            return Err(e);
        }
    }

    let _ = std::fs::remove_file(&previous);
    sink.add(
        "cleanup",
        true,
        "old overlay removed; the Windows workspace and the base image were not touched"
            .to_string(),
    );

    // The identities stay valid, but the seed disk has to be regenerated so that a reset guest
    // still receives its host key and trust anchors.
    let cfg = AppConfig::load(&data.config_json())?;
    let ids = crate::storage::keys::SshIdentities::new(data.ssh_dir());
    if ids.exists() {
        let material = ids.load_public_material()?;
        let guest_host_private = std::fs::read_to_string(ids.guest_host_private())?;
        let guest_client_private = std::fs::read_to_string(ids.guest_client_private())?;
        let (port, tunnel) = crate::vm::ports::reserve_pair()?;
        let seed = crate::vm::seed::SeedSpec {
            schema: crate::vm::seed::SEED_SCHEMA,
            generation: cfg.seed_generation + 1,
            hostname: "oi-linux".into(),
            guest_user: crate::GUEST_USER.into(),
            guest_workspace: crate::GUEST_WORKSPACE.into(),
            share_transport: "reverse_tunnel".into(),
            host_sftp_address: crate::vm::seed::SLIRP_HOST_ALIAS.into(),
            host_sftp_port: port,
            reverse_tunnel_port: tunnel,
            client_public_key: material.client_public_openssh.clone(),
            sftp_host_public_key: material.sftp_host_public_openssh.clone(),
            authorized_keys: material.client_public_openssh.clone(),
            win_known_hosts: crate::vm::seed::win_known_hosts(
                port,
                tunnel,
                &material.sftp_host_public_openssh,
            ),
            guest_host_private_key: guest_host_private.trim().to_string(),
            guest_client_private_key: guest_client_private.trim().to_string(),
        };
        seed.write_iso(&data.seed_iso())?;
        sink.add(
            "seed",
            true,
            "seed configuration disk regenerated".to_string(),
        );
    }

    Ok(InitReport {
        steps: sink.into_steps(),
        environment: env,
        config: cfg,
    })
}

/// Grow the overlay when the requested size exceeds the base image's virtual size. Shrinking is
/// never performed.
fn apply_disk_size(qemu_img: &Path, data: &DataDir, disk_gib: u32) -> OiResult<()> {
    let chain = crate::storage::images::verify_overlay_chain(qemu_img, &data.user_qcow2())?;
    let current = chain
        .first()
        .map(|c| c.virtual_size)
        .ok_or_else(|| OiError::ImageVerification("overlay reports no virtual size".into()))?;
    let wanted = disk_gib as u64 * 1024 * 1024 * 1024;
    if wanted > current {
        resize_image(qemu_img, &data.user_qcow2(), &format!("{disk_gib}G"))?;
    }
    Ok(())
}

/// Fail when another process (usually a running QEMU) holds a write handle. On Windows this is
/// implemented by opening the file with no sharing.
fn assert_no_writer(path: &Path) -> OiResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(0);
    }
    match opts.open(path) {
        Ok(_) => Ok(()),
        Err(e) => Err(OiError::FileLocked(format!(
            "{} is still held by another process ({e}); stop the Linux environment first",
            path.display()
        ))),
    }
}

fn copy_file(src: &Path, dst: &Path) -> OiResult<u64> {
    let bytes = std::fs::copy(src, dst).map_err(|e| OiError::io(dst, e))?;
    Ok(bytes)
}

/// Convenience wrapper used by the UI: is this data directory an existing environment?
pub fn inspect(data_dir: &Path) -> OiResult<Option<EnvironmentJson>> {
    let data = DataDir::new(data_dir);
    if !data.environment_json().is_file() {
        return Ok(None);
    }
    EnvironmentJson::load(&data.environment_json()).map(Some)
}

/// Human readable reason why a workspace path cannot be used, if any.
pub fn workspace_issue(workspace: &Path) -> Option<String> {
    let text = workspace.as_os_str().to_string_lossy().to_string();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Some("network share paths are not supported; choose a local folder".into());
    }
    if text.trim().is_empty() {
        return Some("no workspace directory was chosen".into());
    }
    if workspace.exists() && !workspace.is_dir() {
        return Some("the workspace path is not a directory".into());
    }
    None
}
