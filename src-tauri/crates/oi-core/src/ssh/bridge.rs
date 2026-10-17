//! Share transport between the guest and the host SFTP service.
//!
//! Two transports are implemented, and the one that is used is decided by measurement, not by
//! assumption (`OILinux_PRD.md` §7.2):
//!
//! * **direct** – the guest connects to `10.0.2.2:<port>`, QEMU's user-mode alias for the host.
//!   Whether a host listener bound to `127.0.0.1` is reachable this way depends on the platform,
//!   so it is probed by actually authenticating over SSH.
//! * **reverse tunnel** – the host opens a `tcpip-forward` on the guest over the SSH connection
//!   it already owns, so the guest reaches the SFTP service on `127.0.0.1:<guest port>` and every
//!   byte travels inside that already authenticated SSH session. Nothing is ever exposed to the
//!   local network in either mode.
//!
//! Mounting is performed from the host over SSH, so the guest needs no resident agent.

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};

use super::client::{GuestClient, SshCommandResult};

/// Host alias seen from inside QEMU user-mode networking.
pub const SLIRP_HOST: &str = "10.0.2.2";
pub const GUEST_LOOPBACK: &str = "127.0.0.1";

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeState {
    pub transport: ShareTransport,
    pub guest_port: u16,
    pub mounted: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectProbe {
    pub reachable: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub mounted: bool,
    pub write_ok: bool,
    pub read_back_ok: bool,
    pub chmod_exec_ok: bool,
    pub chmod_noexec_ok: bool,
    pub detail: String,
}

impl HealthReport {
    pub fn healthy(&self) -> bool {
        self.mounted && self.write_ok && self.read_back_ok && self.chmod_exec_ok && self.chmod_noexec_ok
    }
}

/// Single-quote a value for POSIX shells.
pub fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:@+=,".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The `ssh` invocation the guest uses against the host share service.
pub fn guest_ssh_options(host: &str, port: u16) -> String {
    let key = shell_quote(crate::GUEST_SHARE_KEY);
    format!(
        "-o BatchMode=yes -o StrictHostKeyChecking=yes \
         -o UserKnownHostsFile=/home/student/.ssh/known_hosts \
         -o IdentitiesOnly=yes -o IdentityFile={key} \
         -o ConnectTimeout=6 -p {port} {}@{}",
        crate::GUEST_USER, host
    )
}

/// Probe whether the guest can reach the host share listener directly through the SLIRP alias.
pub async fn probe_direct(client: &GuestClient, port: u16) -> DirectProbe {
    let command = format!(
        "timeout 12 ssh {} true 2>&1; echo \"exit=$?\"",
        guest_ssh_options(SLIRP_HOST, port)
    );
    match client.exec_with_timeout(&command, std::time::Duration::from_secs(25)).await {
        Ok(res) => {
            let reachable = res.stdout.contains("exit=0");
            DirectProbe {
                reachable,
                detail: format!(
                    "direct SSH to {SLIRP_HOST}:{port} {}: {}",
                    if reachable { "succeeded" } else { "failed" },
                    first_nonempty(&res.stdout, &res.stderr)
                ),
            }
        }
        Err(e) => DirectProbe {
            reachable: false,
            detail: format!("the direct probe could not be run: {e}"),
        },
    }
}

/// A short explanation of a probe result: the `exit=` marker plus the last lines the client
/// printed, which is where `ssh` puts the actual reason.
fn first_nonempty(a: &str, b: &str) -> String {
    let text = format!("{a}\n{b}");
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail: Vec<&str> = lines.iter().rev().take(3).rev().copied().collect();
    let summary = tail.join(" | ");
    if summary.chars().count() > 300 {
        format!("{}…", summary.chars().take(300).collect::<String>())
    } else {
        summary
    }
}

fn mount_options() -> &'static str {
    // Correctness first (`OILinux_PRD.md` §7.2/§7.4): no permanent content cache, no attribute or
    // entry cache that could hide a file another program just replaced, and permissions enforced
    // by the kernel.
    "default_permissions,idmap=user,noauto_cache,dir_cache=no,attr_timeout=0,\
     entry_timeout=0,negative_timeout=0,reconnect,\
     ServerAliveInterval=15,ServerAliveCountMax=4,StrictHostKeyChecking=yes,\
     UserKnownHostsFile=/home/student/.ssh/known_hosts,IdentitiesOnly=yes,\
     IdentityFile=/home/student/.ssh/id_ed25519,BatchMode=yes"
}

fn mount_options_minimal() -> &'static str {
    "default_permissions,StrictHostKeyChecking=yes,\
     UserKnownHostsFile=/home/student/.ssh/known_hosts,IdentitiesOnly=yes,\
     IdentityFile=/home/student/.ssh/id_ed25519,BatchMode=yes"
}

/// Shell snippet that unmounts the workspace, using whichever FUSE helper the image provides.
/// Alpine's fuse3 package installs `fusermount3`; older layouts only have `fusermount`.
fn unmount_snippet(ws: &str) -> String {
    format!(
        "if mountpoint -q {ws}; then \
           if command -v fusermount3 >/dev/null 2>&1; then fusermount3 -u {ws} || true; \
           elif command -v fusermount >/dev/null 2>&1; then fusermount -u {ws} || true; \
           else umount {ws} || true; fi; \
         fi",
        ws = shell_quote(ws)
    )
}

/// Mount the host share at `/home/student/workspace`.
///
/// The mount point is never allowed to silently degrade into a local empty directory: it is
/// removed first, so a failed mount cannot be mistaken for a working share.
pub async fn mount(
    client: &GuestClient,
    transport: ShareTransport,
    port: u16,
) -> OiResult<SshCommandResult> {
    let (host, user_at_host) = match transport {
        ShareTransport::Direct => (SLIRP_HOST, format!("{}@{SLIRP_HOST}", crate::GUEST_USER)),
        ShareTransport::ReverseTunnel => (GUEST_LOOPBACK, format!("{}@{GUEST_LOOPBACK}", crate::GUEST_USER)),
    };
    let workspace = crate::GUEST_WORKSPACE;

    let build = |options: &str| {
        format!(
            "set -e; \
             {unmount}; \
             if [ -d {ws} ] && [ -n \"$(ls -A {ws} 2>/dev/null)\" ]; then \
               echo 'refusing to mount over a non-empty local directory' >&2; exit 9; fi; \
             rm -rf {ws}; mkdir -p {ws}; \
             chmod 755 {ws}; \
             if sshfs -p {port} {target}:/ {ws} -o {options} && mountpoint -q {ws}; then \
               echo mounted-transport={transport}; \
             else \
               chmod 555 {ws}; \
               echo 'the shared workspace is not mounted; the mount point is read-only so that \
files cannot silently be written to the guest disk instead' >&2; \
               exit 1; \
             fi",
            unmount = unmount_snippet(workspace),
            ws = shell_quote(workspace),
            port = port,
            target = shell_quote(&user_at_host),
            options = options,
            transport = transport.as_str(),
        )
    };

    let first = client
        .exec_with_timeout(&build(mount_options()), std::time::Duration::from_secs(60))
        .await?;
    if first.ok() {
        return Ok(first);
    }
    tracing::warn!(
        stderr = %first.stderr.trim(),
        "the preferred mount options were refused; retrying with the minimal set"
    );
    let second = client
        .exec_with_timeout(&build(mount_options_minimal()), std::time::Duration::from_secs(60))
        .await?;
    if second.ok() {
        return Ok(second);
    }
    let _ = host;
    Err(OiError::Bridge(format!(
        "sshfs could not mount the share (first attempt: {}; second attempt: {})",
        first.stderr.trim(),
        second.stderr.trim()
    )))
}

pub async fn unmount(client: &GuestClient) -> OiResult<SshCommandResult> {
    let workspace = crate::GUEST_WORKSPACE;
    let command = format!(
        "{}; echo unmounted",
        unmount_snippet(workspace)
    );
    client.exec_with_timeout(&command, std::time::Duration::from_secs(30)).await
}

/// Put the mount point into a state where a failed share cannot silently absorb writes: the mount
/// is removed and the directory is left read-only, so a student sees a clear error instead of
/// compiling into a guest-local copy (`OILinux_PRD.md` §7.4).
pub async fn quarantine(client: &GuestClient) -> OiResult<SshCommandResult> {
    let workspace = crate::GUEST_WORKSPACE;
    let command = format!(
        "{unmount}; rm -rf {ws}; mkdir -p {ws}; chmod 555 {ws}; \
         echo 'the shared workspace is unavailable; the mount point is read-only'",
        unmount = unmount_snippet(workspace),
        ws = shell_quote(workspace)
    );
    client.exec_with_timeout(&command, std::time::Duration::from_secs(30)).await
}

pub async fn is_mounted(client: &GuestClient) -> bool {
    let workspace = crate::GUEST_WORKSPACE;
    let command = format!("mountpoint -q {ws} && echo yes || echo no", ws = shell_quote(workspace));
    match client.exec_with_timeout(&command, std::time::Duration::from_secs(20)).await {
        Ok(res) => res.stdout.trim().ends_with("yes"),
        Err(_) => false,
    }
}

/// A real, controlled read/write/execute test against the mounted share.
///
/// This is what "recovered" means: not a TCP handshake, but a file written on one side being
/// readable with the right permissions on the other (`OILinux_PRD.md` §7.4).
pub async fn health_check(client: &GuestClient, nonce: &str) -> OiResult<HealthReport> {
    let script = format!(
        r#"ws={ws}
if ! mountpoint -q "$ws"; then echo "mounted=no"; exit 3; fi
echo "mounted=yes"
d="$ws/.oi-health"
mkdir -p "$d"
printf '%s' {nonce} > "$d/probe.txt"
got=$(cat "$d/probe.txt")
if [ "$got" = {nonce} ]; then echo "read_back=yes"; else echo "read_back=no"; fi
printf '#!/bin/sh\necho oi-exec-ok\n' > "$d/exec.sh"
chmod +x "$d/exec.sh"
if out=$("$d/exec.sh" 2>&1) && [ "$out" = "oi-exec-ok" ]; then echo "chmod_exec=yes"; else echo "chmod_exec=no"; fi
chmod -x "$d/exec.sh"
if "$d/exec.sh" >/dev/null 2>&1; then echo "chmod_noexec=no"; else echo "chmod_noexec=yes"; fi
chmod +x "$d/exec.sh"
rm -rf "$d"
"#,
        ws = shell_quote(crate::GUEST_WORKSPACE),
        nonce = shell_quote(nonce),
    );

    let res = client
        .exec_with_timeout(&script, std::time::Duration::from_secs(60))
        .await?;
    let has = |needle: &str| res.stdout.contains(needle);
    Ok(HealthReport {
        mounted: has("mounted=yes"),
        write_ok: !has("read_back=no") && has("read_back=yes"),
        read_back_ok: has("read_back=yes"),
        chmod_exec_ok: has("chmod_exec=yes"),
        chmod_noexec_ok: has("chmod_noexec=yes"),
        detail: format!(
            "{} {}",
            res.stdout.replace('\n', " ").trim(),
            res.stderr.replace('\n', " ").trim()
        )
        .trim()
        .to_string(),
    })
}

/// Random probe token so a stale file cannot produce a false pass.
pub fn new_nonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| {
            let n: u8 = rng.gen_range(0..36);
            if n < 10 {
                (b'0' + n) as char
            } else {
                (b'a' + n - 10) as char
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_protects_special_characters() {
        assert_eq!(shell_quote("main.cpp"), "main.cpp");
        assert_eq!(shell_quote("/home/student/workspace"), "/home/student/workspace");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
    }

    #[test]
    fn ssh_options_pin_the_host_key_and_the_identity() {
        let opts = guest_ssh_options("10.0.2.2", 55123);
        assert!(opts.contains("StrictHostKeyChecking=yes"), "{opts}");
        assert!(opts.contains("known_hosts"), "{opts}");
        assert!(opts.contains("IdentityFile=/home/student/.ssh/id_ed25519"), "{opts}");
        assert!(opts.contains("-p 55123"), "{opts}");
        assert!(opts.contains("student@10.0.2.2"), "{opts}");
        assert!(!opts.contains("StrictHostKeyChecking=no"), "{opts}");
    }

    #[test]
    fn the_default_mount_disables_content_caching() {
        let o = mount_options();
        assert!(o.contains("default_permissions"));
        assert!(o.contains("noauto_cache"));
        assert!(o.contains("dir_cache=no"));
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(new_nonce(), new_nonce());
        assert_eq!(new_nonce().len(), 16);
    }
}
