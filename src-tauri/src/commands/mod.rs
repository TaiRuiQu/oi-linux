//! Tauri command surface: a thin, typed wrapper over [`oi_core::engine::Engine`].
//!
//! Every command that changes state publishes a fresh `oi://state` snapshot so the UI never has
//! to guess, and terminal traffic is forwarded as one event per session
//! (`oi://terminal/<id>` and `oi://terminal/<id>/exit`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, Mutex};

use oi_core::engine::{
    AppStateSnapshot, Engine, InitReportLite, PreflightReport, StartOutcome,
};
use oi_core::error::OiError;
use oi_core::logging::LogFileInfo;
use oi_core::ssh::client::TerminalEvent;
use oi_core::storage::init::{InitRequest, InitStep};
use oi_core::updater::{self, UpdateInfo};

pub const EVENT_STATE: &str = "oi://state";
pub const EVENT_INIT_PROGRESS: &str = "oi://init-progress";
pub const EVENT_UPDATE_PROGRESS: &str = "oi://update-progress";

/// Shared application context. The engine is behind one lock so that a command can never race
/// the recovery timer or the shutdown path.
pub struct AppCtx {
    pub engine: Mutex<Engine>,
    pub last_update: Mutex<Option<UpdateInfo>>,
}

impl AppCtx {
    pub fn new(engine: Engine) -> Self {
        AppCtx {
            engine: Mutex::new(engine),
            last_update: Mutex::new(None),
        }
    }
}

fn fail(e: OiError) -> String {
    e.to_string()
}

async fn publish(app: &AppHandle, snapshot: &AppStateSnapshot) {
    let _ = app.emit(EVENT_STATE, snapshot.clone());
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalOpenResult {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalDataPayload {
    pub id: String,
    pub data: String,
    pub encoding: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalExitPayload {
    pub id: String,
    pub code: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhpxResult {
    pub started: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitProgressPayload {
    pub step_key: String,
    pub detail: String,
    pub percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThirdPartyLicense {
    pub name: String,
    pub version: String,
    pub license: String,
    pub url: String,
}

/* -------------------------------------------------------------------------------------- *
 * State
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn get_state(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn preflight(ctx: State<'_, Arc<AppCtx>>) -> Result<PreflightReport, String> {
    let mut engine = ctx.engine.lock().await;
    Ok(engine.preflight())
}

/* -------------------------------------------------------------------------------------- *
 * Environment
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn init_environment(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    data_dir: String,
    workspace_dir: String,
    vcpu: u16,
    memory_mib: u32,
    disk_gib: u32,
    relink: bool,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    let request = InitRequest {
        data_dir: data_dir.into(),
        workspace_dir: workspace_dir.into(),
        vcpu,
        memory_mib,
        disk_gib,
        language: None,
        relink,
    };
    let total = 9.0f32;
    let mut done = 0.0f32;
    let app_for_progress = app.clone();
    let mut progress = move |step: InitStep| {
        done += 1.0;
        let _ = app_for_progress.emit(
            EVENT_INIT_PROGRESS,
            InitProgressPayload {
                step_key: format!("initStep.{}", step.key),
                detail: step.detail.clone(),
                percent: (done / total * 100.0).min(100.0),
            },
        );
    };
    let report: InitReportLite = engine.initialize(&request, &mut progress).map_err(fail)?;
    tracing::info!(all_ok = report.all_ok, relinked = report.relinked, "environment ready");
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn set_workspace_dir(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    path: String,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.set_workspace_dir(&path).map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn set_resources(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    vcpu: u16,
    memory_mib: u32,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.set_resources(vcpu, memory_mib).map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn resize_disk(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    gib: u32,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.resize_disk(gib).map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn set_language(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    language: String,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.set_language(&language).map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

/* -------------------------------------------------------------------------------------- *
 * Virtual machine and share
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn start_vm(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    // show "starting" immediately
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    drop(engine);

    let mut engine = ctx.engine.lock().await;
    let outcome: StartOutcome = engine.start_vm().await.map_err(fail)?;
    tracing::info!(
        accelerator = outcome.accelerator.label(),
        ssh_port = outcome.ssh_host_port,
        sftp_port = outcome.sftp_port,
        "Linux environment started"
    );
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn stop_vm(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    force: bool,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.stop_vm(force).await.map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn reset_environment(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    let mut progress = |_step: InitStep| {};
    engine.reset_environment(&mut progress).map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn remount_share(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.remount().await.map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

/* -------------------------------------------------------------------------------------- *
 * External tools
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn open_windows_terminal(
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<(), String> {
    let mut engine = ctx.engine.lock().await;
    engine
        .open_external_terminal()
        .map_err(|e| format!("{}", e))
}

#[tauri::command]
pub async fn open_vscode_workspace(
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<(), String> {
    let mut engine = ctx.engine.lock().await;
    engine.open_vscode().map_err(|e| format!("{}", e))
}

/* -------------------------------------------------------------------------------------- *
 * Terminals
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn terminal_open(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    cols: u32,
    rows: u32,
) -> Result<TerminalOpenResult, String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<TerminalEvent>();
    let mut engine = ctx.engine.lock().await;
    let id = engine
        .open_terminal(cols.max(2), rows.max(2), tx)
        .await
        .map_err(fail)?;
    drop(engine);

    let data_event = format!("oi://terminal/{id}");
    let exit_event = format!("oi://terminal/{id}/exit");
    let id_for_task = id.clone();
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        use base64::Engine as _;
        while let Some(event) = rx.recv().await {
            match event {
                TerminalEvent::Data(bytes) => {
                    let payload = TerminalDataPayload {
                        id: id_for_task.clone(),
                        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                        encoding: "base64".into(),
                    };
                    let _ = app_for_task.emit(&data_event, payload);
                }
                TerminalEvent::Exit(code) => {
                    let _ = app_for_task.emit(
                        &exit_event,
                        TerminalExitPayload {
                            id: id_for_task.clone(),
                            code,
                        },
                    );
                }
                TerminalEvent::Closed => break,
            }
        }
    });

    Ok(TerminalOpenResult { id })
}

#[tauri::command]
pub async fn terminal_write(
    ctx: State<'_, Arc<AppCtx>>,
    id: String,
    data: String,
) -> Result<(), String> {
    let mut engine = ctx.engine.lock().await;
    engine.terminal_write(&id, data.as_bytes()).await.map_err(fail)
}

#[tauri::command]
pub async fn terminal_resize(
    ctx: State<'_, Arc<AppCtx>>,
    id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let mut engine = ctx.engine.lock().await;
    engine
        .terminal_resize(&id, cols.max(2), rows.max(2))
        .await
        .map_err(fail)
}

#[tauri::command]
pub async fn terminal_close(
    ctx: State<'_, Arc<AppCtx>>,
    id: String,
) -> Result<(), String> {
    let mut engine = ctx.engine.lock().await;
    engine.terminal_close(&id).await.map_err(fail)
}

/* -------------------------------------------------------------------------------------- *
 * Hardware acceleration
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn enable_whpx(ctx: State<'_, Arc<AppCtx>>) -> Result<WhpxResult, String> {
    let (started, message) = oi_core::sys::host::enable_whpx_feature();
    // the feature state may have changed, so the cached host snapshot is no longer trustworthy
    {
        let mut engine = ctx.engine.lock().await;
        let _ = engine.refresh_host();
    }
    Ok(WhpxResult { started, message })
}

/* -------------------------------------------------------------------------------------- *
 * Updates
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn set_update_channel(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
    channel: String,
) -> Result<AppStateSnapshot, String> {
    let mut engine = ctx.engine.lock().await;
    engine.set_update_channel(&channel).map_err(fail)?;
    let snapshot = engine.snapshot();
    publish(&app, &snapshot).await;
    Ok(snapshot)
}

#[tauri::command]
pub async fn check_updates(ctx: State<'_, Arc<AppCtx>>) -> Result<Option<UpdateInfo>, String> {
    let channel = {
        let engine = ctx.engine.lock().await;
        engine
            .config()
            .map(|c| c.update_channel.clone())
            .unwrap_or_else(|| "stable".to_string())
    };
    let info = updater::fetch_manifest(&channel, oi_core::VERSION)
        .await
        .map_err(|e| e.to_string())?;
    *ctx.last_update.lock().await = info.clone();
    Ok(info)
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    ctx: State<'_, Arc<AppCtx>>,
) -> Result<(), String> {
    let info = ctx
        .last_update
        .lock()
        .await
        .clone()
        .ok_or_else(|| "check for updates first".to_string())?;
    // An update must never run while the writable overlay is attached.
    {
        let mut engine = ctx.engine.lock().await;
        engine.stop_vm(false).await.map_err(fail)?;
        let snapshot = engine.snapshot();
        publish(&app, &snapshot).await;
    }
    let app_for_progress = app.clone();
    crate::updater_install::download_and_launch(&info, move |downloaded, total| {
        let _ = app_for_progress.emit(
            EVENT_UPDATE_PROGRESS,
            serde_json::json!({ "downloaded": downloaded, "total": total }),
        );
    })
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/* -------------------------------------------------------------------------------------- *
 * Diagnostics
 * -------------------------------------------------------------------------------------- */

#[tauri::command]
pub async fn list_logs(ctx: State<'_, Arc<AppCtx>>) -> Result<Vec<LogFileInfo>, String> {
    let engine = ctx.engine.lock().await;
    engine.list_logs().map_err(fail)
}

#[tauri::command]
pub async fn export_logs(
    ctx: State<'_, Arc<AppCtx>>,
    dest_dir: String,
) -> Result<Vec<String>, String> {
    let engine = ctx.engine.lock().await;
    engine
        .export_logs(std::path::Path::new(&dest_dir))
        .map_err(fail)
}

#[tauri::command]
pub async fn third_party_licenses() -> Result<Vec<ThirdPartyLicense>, String> {
    let text = include_str!("../../resources/licenses/third-party.json");
    #[derive(Deserialize)]
    struct File {
        components: Vec<Component>,
    }
    #[derive(Deserialize)]
    struct Component {
        name: String,
        version: String,
        license: String,
        url: String,
    }
    let parsed: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(parsed
        .components
        .into_iter()
        .map(|c| ThirdPartyLicense {
            name: c.name,
            version: c.version,
            license: c.license,
            url: c.url,
        })
        .collect())
}
