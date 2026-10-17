//! P0 lifecycle and data-safety acceptance tests (`OILinux_PRD.md` §11.3).
//!
//! The single rule these tests enforce: the Windows workspace holds the user's real files, the
//! Linux disk is a rebuildable training environment, and no operation may ever destroy the
//! former while rebuilding the latter.

use std::path::PathBuf;
use std::time::Duration;

use oi_integration_tests::*;

#[tokio::main]
async fn main() {
    init_tracing();
    let mut report = Report::new("§11.3 P0 lifecycle and data safety");

    // A worker mode used by the crash test below: it starts QEMU and dies without cleaning up.
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() == 3 && argv[1] == "--crash-worker" {
        crash_worker(PathBuf::from(&argv[2])).await;
    }

    let mut env = match TestEnv::prepare("boot-reset").await {
        Ok(env) => env,
        Err(e) => {
            report.fail("environment", e);
            std::process::exit(report.finish());
        }
    };

    if let Err(e) = env.start().await {
        report.fail("§11.3 the environment starts on this machine", e);
        std::process::exit(report.finish());
    }
    report.pass(
        "§11.3 a real Alpine Linux starts (no hardware virtualisation required)",
        "the engine started QEMU and the guest answered over SSH",
    );

    // ---- guest state survives a restart -------------------------------------------------
    env.guest("printf 'guest-marker\\n' > /home/student/guest-only.txt && mkdir -p /home/student/guest-dir && printf 'x' > /home/student/guest-dir/x")
        .await;
    env.write_workspace("workspace-marker.txt", "windows-side\n");
    env.stop().await;
    report.pass("§11.3 the environment stops cleanly", "guest powered off");

    if let Err(e) = env.start().await {
        report.fail("§11.3 the environment restarts", e);
        std::process::exit(report.finish());
    }
    let persisted = env
        .guest("cat /home/student/guest-only.txt && cat /home/student/guest-dir/x && echo ---")
        .await;
    report.check(
        "§11.3 data written inside the Linux disk survives a stop/start cycle",
        persisted.stdout.contains("guest-marker") && persisted.stdout.contains("---"),
        summarize(&persisted),
    );
    report.check(
        "§11.3 the Windows workspace is untouched by the restart",
        env.workspace.join("workspace-marker.txt").is_file(),
        "workspace-marker.txt still present",
    );

    // ---- changing the workspace is only possible while stopped --------------------------
    let while_running = env.engine.set_workspace_dir(&env.workspace.to_string_lossy());
    report.check(
        "§11.3 the workspace directory can only be changed while Linux is stopped",
        while_running.is_err(),
        match while_running {
            Err(e) => format!("refused while running: {e}"),
            Ok(()) => "the change was accepted while the environment was running".to_string(),
        },
    );

    // ---- the fully offline core flow ----------------------------------------------------
    let internet_before = env
        .guest_timeout(
            "wget -T 5 -q -O /dev/null https://mirrors.tuna.tsinghua.edu.cn/ ; echo exit=$?",
            Duration::from_secs(60),
        )
        .await;
    env.stop().await;
    std::env::set_var("OI_LINUX_QEMU_RESTRICT_NET", "1");
    let offline_start = env.start().await;
    let offline_guest = if offline_start.is_ok() {
        Some(
            env.guest_timeout(
                "cd /home/student/workspace && printf 'int main(){return 0;}' > off.cpp && g++ off.cpp -o off && ./off && echo offline-build-ok; wget -T 5 -q -O /dev/null https://mirrors.tuna.tsinghua.edu.cn/ ; echo wget-exit=$?",
                Duration::from_secs(600),
            )
            .await,
        )
    } else {
        None
    };
    std::env::remove_var("OI_LINUX_QEMU_RESTRICT_NET");
    let share_offline = env.share_status();
    report.check(
        "§11.3 the complete core flow works with the guest isolated from every network",
        offline_start.is_ok()
            && offline_guest
                .as_ref()
                .map(|r| r.stdout.contains("offline-build-ok") && r.stdout.contains("wget-exit=1"))
                .unwrap_or(false)
            && share_offline == oi_core::share::ShareStatus::Mounted,
        match (&offline_start, &offline_guest) {
            (Err(e), _) => format!("the environment did not start offline: {e}"),
            (Ok(()), Some(r)) => format!(
                "internet reachable normally: {} (exit in {}) | offline: {}",
                internet_before.stdout.trim(),
                internet_before.stderr.trim(),
                summarize(r)
            ),
            (Ok(()), None) => "the offline guest session was never established".to_string(),
        },
    );

    // The marker that a reset must remove is captured while the guest is still running.
    let guest_dir_before = env
        .guest("test -d /home/student/guest-dir && echo present")
        .await;

    // ---- refusing to reset a running environment ----------------------------------------
    let reset_while_running = {
        let mut progress = |_step: oi_core::storage::init::InitStep| {};
        env.engine.reset_environment(&mut progress)
    };
    report.check(
        "§6.4 reset is refused while Linux is running",
        reset_while_running.is_err(),
        match reset_while_running {
            Err(e) => format!("refused: {e}"),
            Ok(()) => "the reset was allowed while running".to_string(),
        },
    );

    env.stop().await;

    // ---- reset only rebuilds the Linux disk ---------------------------------------------
    let base_before = std::fs::read(env.data_dir.join("base.qcow2")).map(|b| b.len()).unwrap_or(0);
    let base_hash_before = std::fs::read_to_string(env.data_dir.join("environment.json"))
        .ok()
        .and_then(|t| {
            t.split("\"base_sha256\":")
                .nth(1)
                .map(|s| s.trim().trim_start_matches('"').chars().take(64).collect::<String>())
        })
        .unwrap_or_default();

    let reset = {
        let mut progress = |step: oi_core::storage::init::InitStep| {
            println!("  reset: {} {}", step.key, step.detail);
        };
        env.engine.reset_environment(&mut progress)
    };
    report.check(
        "§11.3 reset replaces the writable layer",
        reset.is_ok(),
        match &reset {
            Ok(()) => "the overlay was rebuilt and verified".to_string(),
            Err(e) => format!("reset failed: {e}"),
        },
    );
    let base_after = std::fs::read(env.data_dir.join("base.qcow2")).map(|b| b.len()).unwrap_or(0);
    let base_hash_after = std::fs::read_to_string(env.data_dir.join("environment.json"))
        .ok()
        .and_then(|t| {
            t.split("\"base_sha256\":")
                .nth(1)
                .map(|s| s.trim().trim_start_matches('"').chars().take(64).collect::<String>())
        })
        .unwrap_or_default();
    report.check(
        "§11.3 reset never replaces or rewrites base.qcow2",
        base_before == base_after && !base_hash_before.is_empty() && base_hash_before == base_hash_after,
        format!("base image {base_before} bytes, hash {base_hash_before}"),
    );
    report.check(
        "§11.3 reset does not delete the Windows workspace",
        env.workspace.join("workspace-marker.txt").is_file(),
        "workspace-marker.txt survived the reset",
    );

    if let Err(e) = env.start().await {
        report.fail("§11.3 the environment starts after a reset", e);
        std::process::exit(report.finish());
    }
    let guest_after_reset = env
        .guest("ls -d /home/student/guest-dir 2>/dev/null; echo exit=$?")
        .await;
    report.check(
        "§11.3 guest-only data inside the Linux disk is gone after a reset",
        !guest_after_reset.stdout.contains("guest-dir"),
        format!("before reset: {} | after: {}", summarize(&guest_dir_before), summarize(&guest_after_reset)),
    );
    // and the share still works against the same Windows files
    let share_after_reset = env
        .guest("cd /home/student/workspace && cat workspace-marker.txt")
        .await;
    report.check(
        "§11.3 the shared workspace is fully usable again after a reset",
        share_after_reset.stdout.contains("windows-side"),
        summarize(&share_after_reset),
    );

    // ---- three real start failures suggest a reset but never perform one ----------------
    env.stop().await;
    let user_image = env.data_dir.join("user.qcow2");
    let backup = env.data_dir.join("user.qcow2.backup");
    std::fs::rename(&user_image, &backup).expect("move the overlay aside");
    std::fs::write(&user_image, b"this is not a qcow2 image").expect("corrupt the overlay");

    let mut failures = 0;
    for _ in 0..3 {
        if env.start().await.is_err() {
            failures += 1;
        } else {
            let _ = env.stop().await;
        }
    }
    let snapshot = env.engine.snapshot();
    report.check(
        "§11.3 three genuine start failures are counted",
        failures == 3 && snapshot.vm.consecutive_start_failures >= 3,
        format!(
            "{failures} failed attempts, counter {}",
            snapshot.vm.consecutive_start_failures
        ),
    );
    report.check(
        "§11.3 a failing start never cleans anything up by itself",
        env.workspace.join("workspace-marker.txt").is_file()
            && env.data_dir.join("base.qcow2").is_file()
            && env.data_dir.join("environment.json").is_file(),
        "workspace, base image and environment.json are all still present",
    );

    std::fs::remove_file(&user_image).expect("remove the corrupt overlay");
    std::fs::rename(&backup, &user_image).expect("restore the overlay");
    let recovered = env.start().await;
    report.check(
        "§11.3 the environment starts again once the fault is repaired",
        recovered.is_ok(),
        match &recovered {
            Ok(()) => "started after restoring the overlay".to_string(),
            Err(e) => e.clone(),
        },
    );
    env.stop().await;

    // ---- a crashed application leaves no QEMU behind (Job Object) -----------------------
    let settings = env.settings_path().to_path_buf();
    let data_dir = env.data_dir.clone();
    drop(env);
    let exe = std::env::current_exe().expect("the test binary path");
    let mut worker = std::process::Command::new(&exe)
        .arg("--crash-worker")
        .arg(&settings)
        .spawn()
        .expect("spawn the crash worker");
    let status = worker.wait().expect("wait for the crash worker");
    let mut released = false;
    let mut remaining_qemu = 0usize;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        released = is_exclusively_openable(&data_dir.join("user.qcow2"));
        remaining_qemu = count_qemu_processes();
        if released && remaining_qemu == 0 {
            break;
        }
    }
    report.check(
        "§11.3 a crashed application leaves no QEMU writing to the overlay",
        status.code() == Some(9) && released,
        format!(
            "worker exit {:?}; overlay free: {released}; qemu processes left: {remaining_qemu}",
            status.code()
        ),
    );

    std::process::exit(report.finish());
}

/// Start the environment and die without any cleanup, the way a crashed UI process would.
async fn crash_worker(settings: PathBuf) -> ! {
    let mut engine = oi_core::engine::Engine::new(&settings)
        .with_overrides(None, Some(repo_root().join("vm").join("out")));
    let starter = tokio::spawn(async move {
        let _ = engine.start_vm().await;
        // keep the engine (and therefore the QEMU child) alive inside the task
        tokio::time::sleep(Duration::from_secs(600)).await;
    });
    // give QEMU time to be spawned and the boot to begin, then vanish without stopping anything
    tokio::time::sleep(Duration::from_secs(20)).await;
    let _ = starter;
    std::process::exit(9);
}

fn count_qemu_processes() -> usize {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq qemu-system-x86_64.exe", "/NH"])
        .output();
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.to_lowercase().contains("qemu-system-x86_64"))
            .count(),
        Err(_) => 0,
    }
}
