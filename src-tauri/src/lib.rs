//! OI Linux desktop shell: the Tauri window, the shared engine and the command surface.
//!
//! Lifecycle rules implemented here (`OILinux_PRD.md` §6.3, §14 decisions 15/42):
//!
//! * opening the window never starts Linux – the engine is created stopped,
//! * a periodic timer publishes state changes (a guest `poweroff`, a share that needs recovering),
//! * closing the window stops Linux normally first and only then lets the process exit; if that
//!   fails, the Job Object that owns QEMU kills it as the process goes away.

pub mod commands;
pub mod updater_install;

use std::sync::Arc;
use std::time::Duration;

use tauri::{Emitter, Manager, WindowEvent};

use oi_core::engine::Engine;
use oi_core::storage::app_settings::AppSettings;

use commands::{AppCtx, EVENT_STATE};

/// How often the UI timer checks for a guest that stopped on its own and retries a degraded
/// share.
const TICK: Duration = Duration::from_secs(2);

pub fn run() {
    let settings_path = AppSettings::default_path();
    let engine = Engine::new(&settings_path);
    let log_dir = engine.logs_dir();
    let _logging = oi_core::logging::init(&log_dir, "info");
    tracing::info!(
        version = oi_core::VERSION,
        alpine = oi_core::ALPINE_VERSION,
        settings = %settings_path.display(),
        logs = %log_dir.display(),
        "OI Linux starting"
    );

    let ctx = Arc::new(AppCtx::new(engine));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(ctx.clone())
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::preflight,
            commands::init_environment,
            commands::set_workspace_dir,
            commands::set_resources,
            commands::resize_disk,
            commands::set_language,
            commands::start_vm,
            commands::stop_vm,
            commands::reset_environment,
            commands::remount_share,
            commands::open_windows_terminal,
            commands::open_vscode_workspace,
            commands::terminal_open,
            commands::terminal_write,
            commands::terminal_resize,
            commands::terminal_close,
            commands::enable_whpx,
            commands::set_update_channel,
            commands::check_updates,
            commands::install_update,
            commands::list_logs,
            commands::export_logs,
            commands::third_party_licenses,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let ctx = ctx.clone();
            tauri::async_runtime::spawn(async move {
                // The UI asks for the first snapshot itself; this timer only reports changes that
                // happen without user action.
                loop {
                    tokio::time::sleep(TICK).await;
                    let Ok(mut engine) = ctx.engine.try_lock() else {
                        continue;
                    };
                    let before = engine.snapshot();
                    engine.tick();
                    let _ = engine.recover_share().await;
                    let after = engine.snapshot();
                    if changed(&before, &after) {
                        let _ = handle.emit(EVENT_STATE, after);
                    }
                }
            });
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let window = window.clone();
                let app_handle = window.app_handle().clone();
                let ctx = app_handle.state::<Arc<AppCtx>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    {
                        let mut engine = ctx.engine.lock().await;
                        tracing::info!("the window is closing; stopping Linux");
                        // a bounded graceful stop, then the Job Object is the last resort
                        let _ = tokio::time::timeout(Duration::from_secs(30), engine.stop_vm(false))
                            .await;
                        let _ = engine.shutdown().await;
                    }
                    window.destroy().ok();
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("the OI Linux window could not be created");
}

/// Cheap comparison used to avoid flooding the UI with identical snapshots.
fn changed(a: &oi_core::engine::AppStateSnapshot, b: &oi_core::engine::AppStateSnapshot) -> bool {
    a.vm.status != b.vm.status
        || a.vm.pid != b.vm.pid
        || a.share.status != b.share.status
        || a.share.transport != b.share.transport
        || a.share.last_error != b.share.last_error
        || a.ssh.ready != b.ssh.ready
        || a.ssh.host_port != b.ssh.host_port
        || a.environment.is_none() != b.environment.is_none()
        || a.config.is_none() != b.config.is_none()
}
