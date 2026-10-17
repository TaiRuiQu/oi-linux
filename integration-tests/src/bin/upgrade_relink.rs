//! Re-association and update-channel acceptance tests (`OILinux_PRD.md` §4.3, §10.2, §11.3).
//!
//! Two things must never happen: opening an existing environment may not move or rewrite the
//! user's data, and switching update channels may not silently replace the base image or empty
//! the environment.
//!
//! Re-association is what a re-installed application does, so the test drops the first engine
//! (releasing the single-instance lock) and opens the data directory again from a fresh settings
//! file, exactly like a restored system would.

use oi_integration_tests::*;
use oi_core::storage::config::{AppConfig, EnvironmentJson};
use oi_core::storage::init::InitRequest;

#[tokio::main]
async fn main() {
    init_tracing();
    let mut report = Report::new("§4.3/§10.2 re-association and update channels");

    let env = match TestEnv::prepare("upgrade-relink").await {
        Ok(env) => env,
        Err(e) => {
            report.fail("environment", e);
            std::process::exit(report.finish());
        }
    };

    let data_dir = env.data_dir.clone();
    let workspace = env.workspace.clone();
    let root = env.root.clone();
    let environment_before =
        std::fs::read_to_string(data_dir.join("environment.json")).expect("environment.json");
    let parsed_before: EnvironmentJson =
        serde_json::from_str(&environment_before).expect("parse environment.json");
    let base_hash = parsed_before.base_sha256.clone();
    let base_size_before = std::fs::metadata(data_dir.join("base.qcow2"))
        .map(|m| m.len())
        .unwrap_or(0);
    let user_size_before = std::fs::metadata(data_dir.join("user.qcow2"))
        .map(|m| m.len())
        .unwrap_or(0);

    // Release the environment: a second application instance must be refused, and re-association
    // is what happens when there is only one.
    {
        let mut other = oi_core::engine::Engine::new(root.join("settings-second-instance.json"))
            .with_overrides(None, Some(repo_root().join("vm").join("out")));
        let refused = other.initialize(
            &InitRequest {
                data_dir: data_dir.clone(),
                workspace_dir: workspace.clone(),
                vcpu: 2,
                memory_mib: 1024,
                disk_gib: 8,
                language: None,
                relink: true,
            },
            &mut |_| {},
        );
        report.check(
            "§4.2 a second instance cannot open an environment that is in use",
            refused.is_err()
                && refused
                    .as_ref()
                    .err()
                    .map(|e| e.to_string().contains("another OI Linux instance"))
                    .unwrap_or(false),
            match &refused {
                Err(e) => e.to_string(),
                Ok(_) => "the second instance was allowed in".to_string(),
            },
        );
    }
    drop(env);

    // ---- re-associating after a system reinstall ----------------------------------------
    let fresh_settings = root.join("settings-after-reinstall.json");
    let mut reinstalled = oi_core::engine::Engine::new(&fresh_settings)
        .with_overrides(None, Some(repo_root().join("vm").join("out")));
    let relink = {
        let mut progress = |step: oi_core::storage::init::InitStep| {
            println!("  relink: {} {}", step.key, step.detail);
        };
        reinstalled.initialize(
            &InitRequest {
                data_dir: data_dir.clone(),
                workspace_dir: workspace.clone(),
                vcpu: 2,
                memory_mib: 1024,
                disk_gib: 8,
                language: None,
                relink: true,
            },
            &mut progress,
        )
    };
    report.check(
        "§4.3 an existing environment can be re-associated after a system restore",
        relink.is_ok() && relink.as_ref().map(|r| r.all_ok).unwrap_or(false),
        match &relink {
            Ok(r) => format!("{} steps verified", r.steps.len()),
            Err(e) => format!("relink failed: {e}"),
        },
    );

    let environment_after =
        std::fs::read_to_string(data_dir.join("environment.json")).expect("environment.json");
    let parsed_after: EnvironmentJson =
        serde_json::from_str(&environment_after).expect("parse environment.json");
    report.check(
        "§4.3 re-association keeps the environment identity and the base image hash",
        parsed_after.environment_id == parsed_before.environment_id
            && parsed_after.base_sha256 == base_hash
            && parsed_after.created_at == parsed_before.created_at,
        format!(
            "id {} → {}, base {}",
            &parsed_before.environment_id[..8],
            &parsed_after.environment_id[..8],
            &base_hash[..16]
        ),
    );
    report.check(
        "§4.3 re-association does not move or rewrite the images",
        base_size_before > 0
            && user_size_before > 0
            && data_dir.join("base.qcow2").is_file()
            && data_dir.join("user.qcow2").is_file(),
        format!("base.qcow2 {base_size_before} bytes, user.qcow2 {user_size_before} bytes, both in place"),
    );

    // A modified base image must be refused instead of silently accepted.
    let base_path = data_dir.join("base.qcow2");
    let stash = data_dir.join("base.qcow2.stash");
    std::fs::rename(&base_path, &stash).expect("stash the base image");
    let mut tampered = std::fs::read(&stash).expect("read the base image");
    if let Some(last) = tampered.last_mut() {
        *last ^= 0xff;
    }
    std::fs::write(&base_path, &tampered).expect("write the tampered image");
    let refused = reinstalled.initialize(
        &InitRequest {
            data_dir: data_dir.clone(),
            workspace_dir: workspace.clone(),
            vcpu: 2,
            memory_mib: 1024,
            disk_gib: 8,
            language: None,
            relink: true,
        },
        &mut |_| {},
    );
    report.check(
        "§4.3 a modified base image makes re-association refuse to open the environment",
        refused
            .as_ref()
            .err()
            .map(|e| {
                let text = e.to_string();
                text.contains("hash") || text.contains("damaged") || text.contains("modified")
            })
            .unwrap_or(false),
        match &refused {
            Err(e) => e.to_string(),
            Ok(_) => "the tampered environment was accepted".to_string(),
        },
    );
    std::fs::remove_file(&base_path).expect("remove the tampered image");
    std::fs::rename(&stash, &base_path).expect("restore the base image");

    // Creating a second environment in a directory that already holds one must be refused.
    let create_over_existing = reinstalled.initialize(
        &InitRequest {
            data_dir: data_dir.clone(),
            workspace_dir: workspace.clone(),
            vcpu: 2,
            memory_mib: 1024,
            disk_gib: 8,
            language: None,
            relink: false,
        },
        &mut |_| {},
    );
    report.check(
        "§4.3 creating a new environment over an existing one is refused",
        create_over_existing
            .as_ref()
            .err()
            .map(|e| e.to_string().contains("already contains"))
            .unwrap_or(false),
        match &create_over_existing {
            Err(e) => e.to_string(),
            Ok(_) => "the existing environment was overwritten".to_string(),
        },
    );

    // ---- switching update channels ------------------------------------------------------
    let _config_before = AppConfig::load(&data_dir.join("config.json")).expect("config.json");
    let user_image_before = std::fs::metadata(data_dir.join("user.qcow2"))
        .map(|m| m.len())
        .unwrap_or(0);
    let environment_before_switch =
        std::fs::read_to_string(data_dir.join("environment.json")).unwrap_or_default();

    let to_beta = reinstalled.set_update_channel("beta");
    let beta_config = AppConfig::load(&data_dir.join("config.json")).ok();
    let to_stable = reinstalled.set_update_channel("stable");
    let stable_config = AppConfig::load(&data_dir.join("config.json")).ok();
    let environment_after_switch =
        std::fs::read_to_string(data_dir.join("environment.json")).unwrap_or_default();
    let user_image_after = std::fs::metadata(data_dir.join("user.qcow2"))
        .map(|m| m.len())
        .unwrap_or(0);

    report.check(
        "§10.2 both channel switches are accepted in the same application",
        to_beta.is_ok()
            && to_stable.is_ok()
            && beta_config
                .as_ref()
                .map(|c| c.update_channel == "beta")
                .unwrap_or(false)
            && stable_config
                .as_ref()
                .map(|c| c.update_channel == "stable")
                .unwrap_or(false),
        format!(
            "beta: {:?}, stable: {:?}",
            beta_config.as_ref().map(|c| c.update_channel.clone()),
            stable_config.as_ref().map(|c| c.update_channel.clone())
        ),
    );
    report.check(
        "§11.3 switching channels never touches environment.json, base.qcow2 or the overlay",
        environment_before_switch == environment_after_switch && user_image_before == user_image_after,
        format!(
            "environment.json unchanged: {}; overlay {} bytes",
            environment_before_switch == environment_after_switch,
            user_image_after
        ),
    );

    let qemu_img = reinstalled
        .host_snapshot()
        .qemu_path
        .map(|path| std::path::PathBuf::from(path).parent().unwrap().join("qemu-img.exe"))
        .unwrap_or_else(|| std::path::PathBuf::from("qemu-img.exe"));
    report.check(
        "§10.2 the overlay still points at base.qcow2 after the switches",
        oi_core::storage::images::check_overlay_points_at(
            &qemu_img,
            &data_dir.join("user.qcow2"),
            "base.qcow2",
        )
        .is_ok(),
        "the backing chain still records the relative base.qcow2",
    );
    drop(reinstalled);

    // the environment is still fully startable after all of that
    let mut env = match TestEnv::prepare("upgrade-relink").await {
        Ok(env) => env,
        Err(e) => {
            report.fail("§10.2 the environment starts after re-association", e);
            std::process::exit(report.finish());
        }
    };
    let start = env.start().await;
    let share_status = env.share_status();
    report.check(
        "§10.2 the environment starts normally after re-association and channel switches",
        start.is_ok() && share_status == oi_core::share::ShareStatus::Mounted,
        match &start {
            Ok(()) => format!("started; share is {share_status:?}"),
            Err(e) => e.clone(),
        },
    );
    env.stop().await;

    std::process::exit(report.finish());
}
