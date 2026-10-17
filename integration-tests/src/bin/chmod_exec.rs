//! P0 execution and permission acceptance tests (`OILinux_PRD.md` §11.2 items 3–6).
//!
//! Everything here happens on the *shared* directory: the compiler runs inside Alpine, its output
//! is an NTFS file, and the resulting binary is executed directly from the share.

use oi_integration_tests::*;

#[tokio::main]
async fn main() {
    init_tracing();
    let mut report = Report::new("§11.2 P0 execution and permissions");

    let mut env = match TestEnv::prepare("chmod-exec").await {
        Ok(env) => env,
        Err(e) => {
            report.fail("environment", e);
            std::process::exit(report.finish());
        }
    };
    if let Err(e) = env.start().await {
        report.fail("VM start", e);
        std::process::exit(report.finish());
    }

    // ---- 3: compile and run, twice, over the same output name --------------------------
    env.write_workspace("main.cpp", TestEnv::hello_source());
    let build = env
        .guest_timeout(
            "cd /home/student/workspace && g++ -O2 -std=c++17 main.cpp -o main 2>&1 && ./main",
            std::time::Duration::from_secs(900),
        )
        .await;
    report.check(
        "§11.2.3 g++ main.cpp -o main && ./main runs from the shared directory",
        build.ok() && build.stdout.contains("hello-oi"),
        summarize(&build),
    );

    env.replace_workspace(
        "main.cpp",
        "#include <cstdio>\nint main(){ std::printf(\"hello-oi-v2\\n\"); return 7; }\n",
    );
    let rebuild = env
        .guest_timeout(
            "cd /home/student/workspace && g++ -O2 -std=c++17 main.cpp -o main 2>&1 && ./main; echo exit=$?",
            std::time::Duration::from_secs(900),
        )
        .await;
    report.check(
        "§11.2.3 recompiling over the existing ELF works and the new binary runs",
        rebuild.stdout.contains("hello-oi-v2") && rebuild.stdout.contains("exit=7"),
        summarize(&rebuild),
    );

    let kind = env
        .guest("cd /home/student/workspace && file main && readelf -l main | grep -c INTERP")
        .await;
    report.check(
        "§11.2.6 the produced binary is a dynamically linked ELF",
        kind.stdout.contains("ELF 64-bit") && kind.stdout.contains('1'),
        summarize(&kind),
    );

    let compiled_mode = env
        .guest("stat -c %a /home/student/workspace/main")
        .await;
    report.check(
        "§7.3 a file produced by the compiler is created executable",
        compiled_mode.stdout.trim() == "755",
        format!("mode {}", compiled_mode.stdout.trim()),
    );

    // ---- 4: chmod -x really denies execution, chmod +x restores it ----------------------
    let noexec = env
        .guest("cd /home/student/workspace && chmod -x main && ./main; echo exit=$?")
        .await;
    let text = format!(
        "{}{}",
        noexec.stdout.to_lowercase(),
        noexec.stderr.to_lowercase()
    );
    report.check(
        "§11.2.4 chmod -x makes ./main fail with a permission error",
        text.contains("permission denied") || text.contains("exit=126") || text.contains("exit=127"),
        summarize(&noexec),
    );

    let reexec = env
        .guest("cd /home/student/workspace && chmod +x main && ./main; echo exit=$?")
        .await;
    report.check(
        "§11.2.4 chmod +x restores execution",
        reexec.stdout.contains("hello-oi-v2"),
        summarize(&reexec),
    );

    // ---- 6: execve, private and shared mmap from the share ------------------------------
    env.write_workspace("probe-data.bin", "OI-PROBE-DATA");
    env.write_workspace("probe.cpp", TestEnv::mmap_probe_source());
    let mmap = env
        .guest_timeout(
            "cd /home/student/workspace && g++ -O1 -std=c++17 probe.cpp -o probe 2>&1 && ./probe",
            std::time::Duration::from_secs(900),
        )
        .await;
    report.check(
        "§11.2.6 dynamic linking, execve, MAP_SHARED and MAP_PRIVATE work from the share",
        mmap.ok() && mmap.stdout.contains("mmap-ok"),
        summarize(&mmap),
    );

    // ---- 5: the mode follows the file entity -------------------------------------------
    env.guest("chmod 755 /home/student/workspace/main").await;
    let renamed_external = std::fs::rename(
        env.workspace.join("main"),
        env.workspace.join("main-renamed"),
    );
    let mode_after_rename = env
        .guest("stat -c %a /home/student/workspace/main-renamed")
        .await;
    report.check(
        "§11.2.5 a Windows rename keeps the execute permission of the file entity",
        renamed_external.is_ok() && mode_after_rename.stdout.trim() == "755",
        format!(
            "rename {:?}, mode {}",
            renamed_external.err().map(|e| e.to_string()),
            mode_after_rename.stdout.trim()
        ),
    );

    env.write_workspace(
        "replaced.cpp",
        "#include <cstdio>\nint main(){ std::printf(\"replaced\\n\"); }\n",
    );
    let compile_replaced = env
        .guest_timeout(
            "cd /home/student/workspace && g++ -O1 replaced.cpp -o replaced && stat -c %a replaced",
            std::time::Duration::from_secs(900),
        )
        .await;
    // a Windows-style replacement: write a temporary file and rename it over the binary
    std::fs::write(env.workspace.join("replaced.new"), b"not a program\n").unwrap();
    std::fs::rename(
        env.workspace.join("replaced.new"),
        env.workspace.join("replaced"),
    )
    .expect("replace the binary from Windows");
    let mode_after_replace = env
        .guest("stat -c %a /home/student/workspace/replaced")
        .await;
    let detail = format!(
        "compiled mode {} then replaced mode {}",
        compile_replaced.stdout.trim(),
        mode_after_replace.stdout.trim()
    );
    report.check(
        "§11.2.5 a Windows replacement is a new entity with the default mode",
        compile_replaced.stdout.trim() == "755" && mode_after_replace.stdout.trim() == "644",
        detail,
    );

    // ---- 4 (persistence): the execute bit survives a full restart -----------------------
    // `main` was renamed to `main-renamed` above; that is the 0755 file now.
    let before_stop = env
        .guest("stat -c %a /home/student/workspace/main-renamed")
        .await;
    env.stop().await;
    if let Err(e) = env.start().await {
        report.fail("restart", e);
        std::process::exit(report.finish());
    }
    let after_start = env
        .guest(
            "stat -c %a /home/student/workspace/main-renamed && \
             cd /home/student/workspace && ./main-renamed",
        )
        .await;
    report.check(
        "§11.2.4 the execute permission persists across a virtual machine restart",
        before_stop.stdout.trim() == "755"
            && after_start.stdout.contains("755")
            && after_start.stdout.contains("hello-oi-v2"),
        format!(
            "before {:?}, after {:?}",
            before_stop.stdout.trim(),
            summarize(&after_start)
        ),
    );

    env.stop().await;
    std::process::exit(report.finish());
}
