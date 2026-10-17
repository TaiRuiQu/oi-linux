//! Performance baseline (`OILinux_PRD.md` §11.4).
//!
//! These are *measurements*, not thresholds: the plan asks for a baseline that later work can be
//! compared against, on the machine and image that are actually shipped. Everything is measured
//! twice where it matters — once through the shared Windows workspace and once inside the guest's
//! ext4 — so the cost of the share is visible instead of guessed.
//!
//! Every sample records what the command actually saw (how many files were created, listed, found
//! or read), so a suspiciously fast number can be recognised as an empty directory rather than a
//! fast share.
//!
//! The report is printed and written to `integration-tests/artifacts/perf-baseline.json`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use oi_integration_tests::*;
use serde_json::{json, Value};

struct Sample {
    secs: f64,
    ok: bool,
    output: String,
}

impl Sample {
    /// The last number the command printed, which is the count it reported.
    fn count(&self) -> Option<i64> {
        self.output
            .split_whitespace()
            .filter_map(|word| word.parse::<i64>().ok())
            .next_back()
    }
}

#[tokio::main]
async fn main() {
    init_tracing();
    let sizes: Vec<usize> = std::env::var("OI_PERF_SIZES")
        .ok()
        .map(|text| {
            text.split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![100, 1000, 5000]);

    let mut env = match TestEnv::prepare("perf-baseline").await {
        Ok(env) => env,
        Err(e) => {
            eprintln!("environment: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = env.start().await {
        eprintln!("VM start: {e}");
        std::process::exit(1);
    }

    let mut report = serde_json::Map::new();
    let mut problems: Vec<String> = Vec::new();

    // ---------------------------------------------------------------- directory scaling
    let mut directories = serde_json::Map::new();
    for size in &sizes {
        let size = *size;
        let shared = format!("/home/student/workspace/perf-{size}");
        let guest_only = format!("/home/student/perf-{size}");

        let create = format!(
            "rm -rf {{dir}}; mkdir -p {{dir}}; cd {{dir}} && i=1; \
             while [ $i -le {size} ]; do printf 'x' > f$i.cpp; i=$((i+1)); done; \
             ls | wc -l"
        );
        let create_shared = sample(
            &env,
            &create.replace("{dir}", &shared),
            Duration::from_secs(1800),
        )
        .await;
        let create_guest = sample(
            &env,
            &create.replace("{dir}", &guest_only),
            Duration::from_secs(900),
        )
        .await;

        let ls_shared =
            sample(&env, &format!("ls {shared} | wc -l"), Duration::from_secs(900)).await;
        let ls_guest = sample(
            &env,
            &format!("ls {guest_only} | wc -l"),
            Duration::from_secs(900),
        )
        .await;

        let find_shared = sample(
            &env,
            &format!("find {shared} -type f | wc -l"),
            Duration::from_secs(1800),
        )
        .await;
        let find_guest = sample(
            &env,
            &format!("find {guest_only} -type f | wc -l"),
            Duration::from_secs(900),
        )
        .await;

        // Only successful reads are counted, so the number proves the reads really happened.
        let read = "cd {dir} && n=0; for f in *.cpp; do cat \"$f\" > /dev/null && n=$((n+1)); done; echo $n";
        let read_shared = sample(
            &env,
            &read.replace("{dir}", &shared),
            Duration::from_secs(1800),
        )
        .await;
        let read_guest = sample(
            &env,
            &read.replace("{dir}", &guest_only),
            Duration::from_secs(900),
        )
        .await;

        directories.insert(
            size.to_string(),
            json!({
                "create_shared_secs": create_shared.secs,
                "create_shared_files": create_shared.count(),
                "create_ext4_secs": create_guest.secs,
                "create_ext4_files": create_guest.count(),
                "ls_shared_secs": ls_shared.secs,
                "ls_shared_entries": ls_shared.count(),
                "ls_ext4_secs": ls_guest.secs,
                "ls_ext4_entries": ls_guest.count(),
                "find_shared_secs": find_shared.secs,
                "find_shared_files": find_shared.count(),
                "find_ext4_secs": find_guest.secs,
                "find_ext4_files": find_guest.count(),
                "read_all_shared_secs": read_shared.secs,
                "read_all_shared_files": read_shared.count(),
                "read_all_ext4_secs": read_guest.secs,
                "read_all_ext4_files": read_guest.count(),
                "all_commands_ok": create_shared.ok && create_guest.ok && ls_shared.ok
                    && ls_guest.ok && find_shared.ok && find_guest.ok && read_shared.ok
                    && read_guest.ok,
            }),
        );
        println!(
            "  {size:>5} files: create {:.2}s (ext4 {:.2}s), ls {:.2}s/{} entries, \
             find {:.2}s/{} files, read all {:.2}s/{} files",
            create_shared.secs,
            create_guest.secs,
            ls_shared.secs,
            ls_shared.count().unwrap_or(-1),
            find_shared.secs,
            find_shared.count().unwrap_or(-1),
            read_shared.secs,
            read_shared.count().unwrap_or(-1),
        );

        // A measurement is only meaningful when the commands actually saw the files: the whole
        // point of tracking the counts is that an empty directory looks like a very fast share.
        let expected = size as i64;
        for (label, sample) in [
            ("create_shared", &create_shared),
            ("create_ext4", &create_guest),
            ("ls_shared", &ls_shared),
            ("ls_ext4", &ls_guest),
            ("find_shared", &find_shared),
            ("find_ext4", &find_guest),
            ("read_all_shared", &read_shared),
            ("read_all_ext4", &read_guest),
        ] {
            match sample.count() {
                Some(count) if count == expected => {}
                Some(count) => problems.push(format!(
                    "size {size}: {label} saw {count} entries instead of {expected}"
                )),
                None => problems.push(format!(
                    "size {size}: {label} did not report a count ({})",
                    sample.output.trim()
                )),
            }
        }
    }
    report.insert("directories".into(), Value::Object(directories));

    // ---------------------------------------------------------------- compile latency
    env.write_workspace(
        "perf/main.cpp",
        "#include <bits/stdc++.h>\nusing namespace std;\nint main(){ios::sync_with_stdio(false);cin.tie(nullptr);int n=0;for(int i=0;i<1000000;i++)n+=i%7;cout<<n<<\"\\n\";return 0;}\n",
    );
    let mut compile_samples = Vec::new();
    let mut run_samples = Vec::new();
    for attempt in 0..3 {
        let compile = sample(
            &env,
            &format!(
                "cd /home/student/workspace/perf && g++ -O2 -std=c++17 main.cpp -o main 2>&1 && echo built-{attempt}"
            ),
            Duration::from_secs(1200),
        )
        .await;
        compile_samples.push(compile.secs);
        let run = sample(
            &env,
            "cd /home/student/workspace/perf && ./main",
            Duration::from_secs(600),
        )
        .await;
        run_samples.push(run.secs);
    }
    let ext4_compile = sample(
        &env,
        "mkdir -p /home/student/perf-compile && cp /home/student/workspace/perf/main.cpp /home/student/perf-compile/ && cd /home/student/perf-compile && g++ -O2 -std=c++17 main.cpp -o main 2>&1 && echo built",
        Duration::from_secs(1200),
    )
    .await;
    report.insert(
        "compile_single_cpp17".into(),
        json!({
            "shared_compile_secs": compile_samples,
            "shared_run_secs": run_samples,
            "ext4_compile_secs": ext4_compile.secs,
            "ext4_compile_ok": ext4_compile.ok,
        }),
    );

    // ---------------------------------------------------------------- throughput
    let write_throughput = sample(
        &env,
        "cd /home/student/workspace/perf && dd if=/dev/zero of=payload.bin bs=1M count=64 2>&1 | tail -1",
        Duration::from_secs(1800),
    )
    .await;
    let read_throughput = sample(
        &env,
        "cd /home/student/workspace/perf && dd if=payload.bin of=/dev/null bs=1M 2>&1 | tail -1",
        Duration::from_secs(1800),
    )
    .await;
    report.insert(
        "throughput_64mib".into(),
        json!({
            "write_secs": write_throughput.secs,
            "read_secs": read_throughput.secs,
            "write_ok": write_throughput.ok,
            "read_ok": read_throughput.ok,
        }),
    );

    // ---------------------------------------------------------------- host footprint
    let snapshot = env.engine.snapshot();
    report.insert(
        "host".into(),
        json!({
            "qemu_working_set_mib": qemu_working_set_mib(),
            "total_memory_mib": snapshot.host.total_memory_mib,
            "free_disk_mib": snapshot.host.free_disk_mib,
            "windows_build": snapshot.host.windows_build,
            "qemu_version": snapshot.host.qemu_version,
            "accelerator": format!("{:?}", snapshot.vm.accelerator),
            "vcpu": env.engine.config().map(|c| c.vcpu),
            "memory_mib": env.engine.config().map(|c| c.memory_mib),
            "mount_options": "default_permissions,noauto_cache,attr_timeout=0,entry_timeout=0",
        }),
    );

    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("artifacts");
    std::fs::create_dir_all(&artifacts).expect("create the artifacts directory");
    let path = artifacts.join("perf-baseline.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&Value::Object(report.clone())).expect("serialise"),
    )
    .expect("write the report");

    println!("\n=== performance baseline ===");
    println!("{}", serde_json::to_string_pretty(&Value::Object(report)).unwrap());
    if problems.is_empty() {
        println!("\nevery sample saw the expected number of files");
    } else {
        eprintln!(
            "\n{} measurement(s) did not see the expected number of files:",
            problems.len()
        );
        for problem in &problems {
            eprintln!("  - {problem}");
        }
    }
    println!("\nwritten to {}", path.display());

    env.stop().await;
    if !problems.is_empty() {
        std::process::exit(1);
    }
}

async fn sample(env: &TestEnv, command: &str, timeout: Duration) -> Sample {
    let started = Instant::now();
    let result = env.guest_timeout(command, timeout).await;
    let secs = started.elapsed().as_secs_f64();
    if !result.ok() {
        eprintln!("  (command reported a failure: {})", summarize(&result));
    }
    Sample {
        secs,
        ok: result.ok(),
        output: result.stdout.clone(),
    }
}

/// Working set of the QEMU process, in MiB, from `tasklist`'s CSV output.
fn qemu_working_set_mib() -> u64 {
    let out = std::process::Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq qemu-system-x86_64.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .output();
    let Ok(out) = out else { return 0 };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .next()
        .and_then(|line| {
            line.split("\",\"")
                .find(|field| field.trim_end_matches('"').ends_with(" K"))
                .map(|value| value.replace(['"', ',', 'K', ' '], ""))
        })
        .and_then(|value| value.parse::<u64>().ok())
        .map(|kilobytes| kilobytes / 1024)
        .unwrap_or(0)
}
