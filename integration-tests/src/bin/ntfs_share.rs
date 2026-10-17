//! P0 file sharing acceptance tests (`OILinux_PRD.md` §11.2).
//!
//! Two parts:
//!
//! * **Protocol level** – the production share service is started in-process against a scratch
//!   root and driven with a real SSH/SFTP client. These checks need no virtual machine, so they
//!   also run on a build agent without QEMU.
//! * **End to end** – the real Alpine environment is started through the production engine and
//!   the same criteria are verified across the guest, the Windows workspace and the share.

use std::path::{Path, PathBuf};

use oi_integration_tests::*;
use oi_core::share::ShareStatus;
use oi_core::storage::keys::SshIdentities;

#[tokio::main]
async fn main() {
    init_tracing();
    let mut report = Report::new("§11.2 P0 file sharing");

    protocol_suite(&mut report).await;

    match TestEnv::prepare("ntfs-share").await {
        Ok(mut env) => end_to_end_suite(&mut report, &mut env).await,
        Err(e) => report.fail("environment", e),
    }

    std::process::exit(report.finish());
}

/* ------------------------------------------------------------------------------------------ *
 * Part 1: the share service on its own
 * ------------------------------------------------------------------------------------------ */

async fn protocol_suite(report: &mut Report) {
    let root = workspace_dir().join("ntfs-share-protocol");
    let _ = std::fs::remove_dir_all(&root);
    let share_root = root.join("share-root");
    let outside = root.join("outside");
    std::fs::create_dir_all(&share_root).expect("share root");
    std::fs::create_dir_all(&outside).expect("outside directory");
    std::fs::write(outside.join("secret.txt"), b"TOP-SECRET").expect("secret");

    let identities = SshIdentities::new(root.join("ssh"));
    identities.generate_all().expect("identities");
    let (server, _db, _guest_public) = start_share(&share_root, &identities).await;
    let port = server.port;

    // A positive control first: a legitimate request must work, otherwise a refusal means
    // nothing.
    let client = SftpTest::open(port, &identities.guest_client_private()).await;
    let positive = sftp_write(&client, "/main.cpp", b"int main() { return 0; }\n", None).await;
    report.check(
        "§11.2 control: a legitimate create/read round trip works",
        positive.is_ok() && std::fs::read(share_root.join("main.cpp")).is_ok(),
        match &positive {
            Ok(()) => "main.cpp created through the share and present on NTFS".to_string(),
            Err(e) => format!("the control request failed: {e}"),
        },
    );

    // 8: every escape spelling must be refused.
    let hostile: &[(&str, &str)] = &[
        ("../../windows/win.ini", "relative escape"),
        ("/../../secret.txt", "absolute-style escape"),
        ("a/../../secret.txt", "escape in the middle"),
        ("..\\..\\secret.txt", "backslash escape"),
        ("C:/Windows/win.ini", "drive letter"),
        (r"\\?\C:\Windows\win.ini", "Windows special path"),
        (r"\\server\share\x", "UNC path"),
        ("//server/share/x", "UNC with forward slashes"),
        ("file.txt:stream", "alternate data stream"),
        ("/x:$DATA", "named stream"),
        ("CON", "reserved device name"),
        ("sub/NUL.txt", "reserved device name in a subdirectory"),
        ("name.", "trailing dot"),
        ("name ", "trailing space"),
    ];
    let mut refused = Vec::new();
    let mut leaked = Vec::new();
    for (path, kind) in hostile {
        match client.sftp.metadata(*path).await {
            Ok(_) => leaked.push(format!("{kind} ({path}) was resolved")),
            Err(_) => refused.push(*kind),
        }
    }
    report.check(
        "§11.2.8 hostile SFTP paths are refused",
        leaked.is_empty(),
        if leaked.is_empty() {
            format!(
                "{} request spellings refused: {}",
                refused.len(),
                refused.join(", ")
            )
        } else {
            format!("requests escaped the root: {}", leaked.join("; "))
        },
    );

    // A junction inside the root must not become a way out.
    let junction = share_root.join("escape-junction");
    let junction_created = std::process::Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &junction.to_string_lossy(),
            &outside.to_string_lossy(),
        ])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if junction_created {
        let through = client.sftp.metadata("/escape-junction/secret.txt").await;
        let listing = client.sftp.read_dir("/escape-junction").await;
        report.check(
            "§11.2.8 a junction inside the root cannot reach outside",
            through.is_err() && listing.is_err(),
            match &through {
                Err(e) => format!("reading through the junction was refused: {e}"),
                Ok(_) => "a file outside the shared root was reachable through a junction"
                    .to_string(),
            },
        );
        let _ = std::process::Command::new("cmd")
            .args(["/C", "rmdir", &junction.to_string_lossy()])
            .output();
    } else {
        report.fail(
            "§11.2.8 a junction inside the root cannot reach outside",
            "the test could not create a junction to try",
        );
    }

    // Symbolic links are not part of the supported file set and must be refused.
    let symlink_result = client
        .sftp
        .symlink("/link.txt", outside.join("secret.txt").to_string_lossy())
        .await;
    report.check(
        "§11.2.8 symbolic links are refused rather than followed",
        symlink_result.is_err(),
        match symlink_result {
            Err(e) => format!("symlink creation refused: {e}"),
            Ok(()) => "the share created a symbolic link".to_string(),
        },
    );

    // The service always requires authentication.
    let other = SshIdentities::new(root.join("other"));
    other.generate_all().expect("second identity");
    let unauthorised = try_open_sftp(port, &other.guest_client_private(), "student").await;
    let anonymous = try_open_sftp(port, &identities.guest_client_private(), "root").await;
    report.check(
        "§8.2 the share service always requires authentication",
        unauthorised.is_err() && anonymous.is_err(),
        format!(
            "wrong key: {}; wrong account: {}",
            unauthorised.err().unwrap_or_else(|| "accepted!".into()),
            anonymous.err().unwrap_or_else(|| "accepted!".into())
        ),
    );

    // Only the SFTP subsystem is offered.
    let exec_refused = try_exec_over_share(port, &identities.guest_client_private()).await;
    report.check(
        "§8.2 the share service offers only the sftp subsystem",
        exec_refused.is_ok(),
        match &exec_refused {
            Ok(detail) => detail.clone(),
            Err(detail) => detail.clone(),
        },
    );

    // 10: the listener is on loopback only.
    let loopback = listener_is_loopback_only(port).unwrap_or(false);
    report.check(
        "§11.2.10 the share listener is bound to loopback only",
        loopback && port > 1024 && port != 2222,
        format!("port {port} is loopback-only: {loopback}"),
    );

    // Extensions the guest's sshfs relies on.
    match RawSftpTest::open(port, &identities.guest_client_private()).await {
        Ok(raw) => {
            let has_rename = raw
                .version
                .extensions
                .contains_key("posix-rename@openssh.com");
            let has_fsync = raw.version.extensions.contains_key("fsync@openssh.com");
            let version = raw.version.version;
            report.check(
                "§7.2 protocol v3 with posix-rename and fsync extensions",
                version == 3 && has_rename && has_fsync,
                format!(
                    "version {version}, extensions: {}",
                    raw.version
                        .extensions
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );

            // posix-rename replaces the destination; the plain rename refuses to.
            let _ = raw
                .session
                .open(
                    "/replace.txt",
                    russh_sftp::protocol::OpenFlags::CREATE
                        | russh_sftp::protocol::OpenFlags::WRITE,
                    russh_sftp::protocol::FileAttributes::default(),
                )
                .await;
            let plain = raw.session.rename("/main.cpp", "/replace.txt").await;
            let replaced = raw
                .session
                .extended(
                    "posix-rename@openssh.com",
                    two_strings("/main.cpp", "/replace.txt"),
                )
                .await;
            let replaced_ok = matches!(
                replaced,
                Ok(russh_sftp::protocol::Packet::Status(ref s))
                    if s.status_code == russh_sftp::protocol::StatusCode::Ok
            );
            report.check(
                "§7.2 posix-rename replaces an existing file, plain rename refuses to",
                plain.is_err() && replaced_ok,
                format!(
                    "plain rename: {:?}; posix-rename: {:?}",
                    plain.err().map(|e| e.to_string()),
                    replaced
                        .map(|_| "ok".to_string())
                        .map_err(|e| e.to_string())
                ),
            );
        }
        Err(e) => report.fail(
            "§7.2 protocol v3 with posix-rename and fsync extensions",
            e,
        ),
    }

    // 4/5: the simulated POSIX modes.
    mode_suite(report, &client, &share_root).await;

    // §11.4 requires directories with thousands of files to work. This also caught a real bug
    // once: the service sent the whole listing as one SFTP packet, which clients dropped, so a
    // 5000-file directory appeared empty.
    let big = share_root.join("big");
    std::fs::create_dir_all(&big).expect("create the big directory");
    for i in 0..3000 {
        std::fs::write(big.join(format!("f{i:04}.cpp")), b"x").expect("write a file");
    }
    let listed = client
        .sftp
        .read_dir("/big")
        .await
        .map(|dir| dir.count())
        .unwrap_or(0);
    report.check(
        "§11.4 a directory with 3000 entries is listed completely",
        listed == 3000,
        format!("{listed} of 3000 entries returned through the share"),
    );

    server.stop().await;
    let _ = std::fs::remove_dir_all(&root);
}

async fn mode_suite(report: &mut Report, client: &SftpTest, share_root: &PathBuf) {
    use russh_sftp::protocol::{FileAttributes, FileMode};

    let new_file = sftp_write(client, "/fresh.cpp", b"// fresh\n", None).await;
    let mode = client
        .sftp
        .metadata("/fresh.cpp")
        .await
        .map(|m| m.permissions.unwrap_or(0) & 0o7777)
        .unwrap_or(0);
    report.check(
        "§11.2.5 a new source file defaults to 0644",
        new_file.is_ok() && mode == 0o644,
        format!("mode {:o}", mode),
    );

    let created_dir = client.sftp.create_dir("/probe-dir").await;
    let dir_mode = client
        .sftp
        .metadata("/probe-dir")
        .await
        .map(|m| m.permissions.unwrap_or(0) & 0o7777)
        .unwrap_or(0);
    report.check(
        "§11.2.5 a directory is traversable (0755)",
        created_dir.is_ok() && dir_mode & 0o111 == 0o111,
        format!("mode {:o}", dir_mode),
    );

    // chmod +x through setstat, then read it back
    let exec_attr = FileAttributes {
        permissions: Some(0o755 | FileMode::REG.bits()),
        ..Default::default()
    };
    let set_exec = client.sftp.set_metadata("/fresh.cpp", exec_attr).await;
    let after = client
        .sftp
        .metadata("/fresh.cpp")
        .await
        .map(|m| m.permissions.unwrap_or(0) & 0o111)
        .unwrap_or(0);
    report.check(
        "§11.2.4 chmod +x through the share is visible immediately",
        set_exec.is_ok() && after == 0o111,
        format!("setstat {:?}, execute bits {:o}", set_exec.err(), after),
    );

    // rename follows the entity: the exec bit survives
    let renamed = client.sftp.rename("/fresh.cpp", "/renamed.cpp").await;
    let still_exec = client
        .sftp
        .metadata("/renamed.cpp")
        .await
        .map(|m| m.permissions.unwrap_or(0) & 0o111)
        .unwrap_or(0);
    report.check(
        "§11.2.5 renaming keeps the permission of the file entity",
        renamed.is_ok() && still_exec == 0o111,
        format!("rename {:?}, execute bits {:o}", renamed.err(), still_exec),
    );

    // A replacement is a new entity and gets the default mode again. Windows editors save by
    // writing a temporary file and renaming it over the original, which is exactly this.
    let tmp = share_root.join("replacement.txt");
    std::fs::write(&tmp, b"// replacement\n").expect("write the replacement");
    let replaced = std::fs::rename(&tmp, share_root.join("renamed.cpp"));
    let replacement_mode = client
        .sftp
        .metadata("/renamed.cpp")
        .await
        .map(|m| m.permissions.unwrap_or(0))
        .unwrap_or(0);
    report.check(
        "§11.2.5 replacing the file resets it to the default mode",
        replaced.is_ok() && (replacement_mode & 0o111) == 0,
        format!(
            "after the replace the execute bits are {:o}",
            replacement_mode & 0o111
        ),
    );
}

async fn try_exec_over_share(port: u16, identity: &Path) -> Result<String, String> {
    use std::sync::Arc;
    use std::time::Duration;

    let key = russh::keys::PrivateKey::read_openssh_file(identity)
        .map_err(|e| format!("read key: {e}"))?;
    let config = Arc::new(russh::client::Config::default());
    let mut handle = russh::client::connect(config, ("127.0.0.1", port), TestClientHandler)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let auth = handle
        .authenticate_publickey(
            oi_core::GUEST_USER.to_string(),
            russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None),
        )
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    if !matches!(auth, russh::client::AuthResult::Success) {
        return Err("authentication was refused".into());
    }
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel: {e}"))?;
    // The client only *sends* the request here; whether the service accepted it shows up in the
    // messages that follow.
    if channel.exec(true, b"id".to_vec()).await.is_err() {
        return Ok("the exec request was refused immediately".into());
    }

    let mut saw_output = false;
    let mut saw_refusal = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, channel.wait()).await {
            Ok(Some(message)) => match message {
                russh::ChannelMsg::Data { .. } | russh::ChannelMsg::ExtendedData { .. } => {
                    saw_output = true
                }
                russh::ChannelMsg::Failure => saw_refusal = true,
                russh::ChannelMsg::Close | russh::ChannelMsg::Eof => break,
                _ => {}
            },
            Ok(None) => break,
            Err(_) => break,
        }
    }

    if saw_output {
        Err("the share service produced command output for an exec request".to_string())
    } else if saw_refusal {
        Ok("the exec request was refused with an SSH channel failure".to_string())
    } else {
        Ok("the exec request produced no output and opened no session".to_string())
    }
}

struct TestClientHandler;

impl russh::client::Handler for TestClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/* ------------------------------------------------------------------------------------------ *
 * Part 2: the real environment
 * ------------------------------------------------------------------------------------------ */

async fn end_to_end_suite(report: &mut Report, env: &mut TestEnv) {
    if let Err(e) = env.start().await {
        report.fail("VM start", e);
        return;
    }
    report.pass(
        "VM start",
        format!(
            "the Alpine environment started; guest ssh port {:?}",
            env.engine.ssh_port()
        ),
    );

    let status = env.share_status();
    report.check(
        "the shared workspace is mounted after start",
        status == ShareStatus::Mounted,
        format!("share status is {status:?}"),
    );
    if status != ShareStatus::Mounted {
        return;
    }

    // ---- 1: Windows writes, Linux reads ------------------------------------------------
    env.write_workspace(
        "main.cpp",
        "#include <cstdio>\nint main(){ std::printf(\"v1\\n\"); }\n",
    );
    let read_back = env
        .guest(&format!("cat {}", env.guest_path("main.cpp")))
        .await;
    report.check(
        "§11.2.1 Windows-created file is immediately readable in Alpine",
        read_back.stdout.contains("v1"),
        summarize(&read_back),
    );

    env.replace_workspace(
        "main.cpp",
        "#include <cstdio>\nint main(){ std::printf(\"v2\\n\"); }\n",
    );
    let after_replace = env
        .guest(&format!("cat {}", env.guest_path("main.cpp")))
        .await;
    report.check(
        "§11.2.1 an editor-style atomic replace is visible on the next Linux read",
        after_replace.stdout.contains("v2") && !after_replace.stdout.contains("v1"),
        summarize(&after_replace),
    );

    // ---- 2: Linux writes, Windows sees -------------------------------------------------
    let created = env
        .guest(&format!(
            "printf 'from-linux\\n' > {} && mkdir -p {} && printf 'x' > {}",
            env.guest_path("created-by-linux.txt"),
            env.guest_path("linux-dir"),
            env.guest_path("linux-dir/inner.txt"),
        ))
        .await;
    let visible = std::fs::read_to_string(env.workspace.join("created-by-linux.txt"))
        .unwrap_or_default();
    let dir_visible = env.workspace.join("linux-dir/inner.txt").is_file();
    report.check(
        "§11.2.2 files and directories created in Alpine appear on Windows immediately",
        created.ok() && visible.contains("from-linux") && dir_visible,
        format!(
            "{} (directory visible: {dir_visible})",
            summarize(&created)
        ),
    );

    let renamed = env
        .guest(&format!(
            "mv {} {} && rm -f {}",
            env.guest_path("created-by-linux.txt"),
            env.guest_path("renamed-by-linux.txt"),
            env.guest_path("linux-dir/inner.txt"),
        ))
        .await;
    let rename_visible = env.workspace.join("renamed-by-linux.txt").is_file()
        && !env.workspace.join("created-by-linux.txt").exists()
        && !env.workspace.join("linux-dir/inner.txt").exists();
    report.check(
        "§11.2.2 rename and delete from Alpine are reflected on Windows immediately",
        renamed.ok() && rename_visible,
        summarize(&renamed),
    );

    // ---- 7: a Windows exclusive lock must not lose data --------------------------------
    let locked_path = env.write_workspace("locked.txt", "original\n");
    let lock_metadata_before = std::fs::metadata(&locked_path).expect("metadata before locking");
    let lock = open_exclusive(&locked_path).expect("hold an exclusive lock");
    let write_while_locked = env
        .guest(&format!(
            "printf 'guest\\n' > {} ; echo exit=$?",
            env.guest_path("locked.txt")
        ))
        .await;
    // The lock also blocks the test's own read, so compare the file's identity and size instead
    // of its content: a truncated or replaced file would differ in both.
    let lock_metadata_after = std::fs::metadata(&locked_path).expect("metadata after the attempt");
    let unchanged = lock_metadata_before.len() == lock_metadata_after.len()
        && lock_metadata_before
            .modified()
            .ok()
            .zip(lock_metadata_after.modified().ok())
            .map(|(a, b)| a == b)
            .unwrap_or(true);
    let refused = write_while_locked.stdout.contains("exit=0") == false;
    report.check(
        "§11.2.7 a Windows lock produces a clear failure instead of silent corruption",
        refused && unchanged,
        format!(
            "guest: {} | file unchanged ({} bytes, same mtime): {unchanged}",
            summarize(&write_while_locked),
            lock_metadata_after.len()
        ),
    );

    drop(lock);
    let content_after_unlock = std::fs::read_to_string(&locked_path).unwrap_or_default();
    report.check(
        "§11.2.7 the original content is intact once the lock is released",
        content_after_unlock.contains("original"),
        format!("content is {content_after_unlock:?}"),
    );

    let write_after_unlock = env
        .guest(&format!(
            "printf 'guest\\n' > {} && cat {}",
            env.guest_path("locked.txt"),
            env.guest_path("locked.txt")
        ))
        .await;
    report.check(
        "§11.2.7 the same write succeeds once the lock is released",
        write_after_unlock.ok() && write_after_unlock.stdout.contains("guest"),
        summarize(&write_after_unlock),
    );

    // ---- 7: Linux stays usable while the share service is down -------------------------
    env.engine.stop_share_service().await;
    let independent = env
        .guest("g++ --version >/dev/null && echo compiler-ok")
        .await;
    let degraded = env.share_status();
    report.check(
        "§7.4 Linux remains fully usable when the share service is stopped",
        independent.stdout.contains("compiler-ok"),
        format!("{} | share status {degraded:?}", summarize(&independent)),
    );
    report.check(
        "§7.4 the share state is reported as degraded, not as a VM failure",
        degraded != ShareStatus::Mounted,
        format!("share status {degraded:?}"),
    );

    let restored = env.engine.restart_share_service_and_remount().await;
    let verify_after = env
        .guest(&format!(
            "printf 'restored\\n' > {} && cat {}",
            env.guest_path("after-restore.txt"),
            env.guest_path("after-restore.txt")
        ))
        .await;
    report.check(
        "§11.2.7 after the share service is restored, read/write is verified again",
        restored.is_ok() && verify_after.ok() && env.workspace.join("after-restore.txt").is_file(),
        format!(
            "remount: {:?}; round trip: {}",
            restored.as_ref().err().map(|e| e.to_string()),
            summarize(&verify_after)
        ),
    );

    // ---- 9: names, Unicode and case behaviour -----------------------------------------
    let unicode_ok = env
        .guest(&format!(
            "mkdir -p \"{}\" && printf 'unicode\\n' > \"{}\" && cat \"{}\"",
            env.guest_path("中文 目录"),
            env.guest_path("中文 目录/源文件 名.cpp"),
            env.guest_path("中文 目录/源文件 名.cpp")
        ))
        .await;
    let unicode_visible = env.workspace.join("中文 目录/源文件 名.cpp").is_file();
    report.check(
        "§11.2.9 Chinese names and spaces work in both directions",
        unicode_ok.ok() && unicode_visible,
        format!(
            "{} (visible on Windows: {unicode_visible})",
            summarize(&unicode_ok)
        ),
    );

    env.write_workspace("CaseTest.txt", "first\n");
    let case_write = env
        .guest(&format!(
            "printf 'second\\n' > {} && ls {} >/dev/null && echo written",
            env.guest_path("casetest.txt"),
            env.guest_path(".")
        ))
        .await;
    let case_entries = std::fs::read_dir(&env.workspace)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("casetest.txt")
                })
                .count()
        })
        .unwrap_or(0);
    report.check(
        "§11.2.9 names that differ only by case address the same NTFS file",
        case_write.ok() && case_entries == 1,
        format!(
            "{case_entries} entry matching 'casetest.txt' (NTFS is case-insensitive by design)"
        ),
    );

    // ---- 10: the port is dynamic, the listener is loopback-only ------------------------
    let port = env.engine.share_port().expect("a share port");
    let loopback = listener_is_loopback_only(port).unwrap_or(false);
    report.check(
        "§11.2.10 the share service listens on a random loopback port only",
        loopback && port > 1024 && port != 2222,
        format!("port {port}, loopback only: {loopback}"),
    );

    // ---- permissions survive a remount -------------------------------------------------
    env.guest(&format!(
        "chmod 755 {}",
        env.guest_path("renamed-by-linux.txt")
    ))
    .await;
    let _ = env.remount().await;
    let mode_after_remount = env
        .guest(&format!(
            "stat -c %a {}",
            env.guest_path("renamed-by-linux.txt")
        ))
        .await;
    report.check(
        "§11.2.4 the execute bit survives a remount",
        mode_after_remount.stdout.trim() == "755",
        summarize(&mode_after_remount),
    );

    env.stop().await;
}
