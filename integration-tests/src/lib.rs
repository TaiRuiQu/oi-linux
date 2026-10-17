//! Shared harness for the blocking acceptance tests.
//!
//! The tests exercise the *production* code paths (`oi_core`), not a reimplementation: the same
//! QEMU argument builder, the same host SFTP service, the same seed disk, the same engine the
//! Tauri shell drives, and the same storage routines. Results are printed as a
//! machine-checkable report so that a reviewer can see exactly which criterion was verified and
//! how.
//!
//! One environment is created per test binary and then reused on subsequent runs: the data
//! directory is kept, so a re-run only pays for the overlay and the boot instead of copying and
//! hashing the base image again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use oi_core::engine::Engine;
use oi_core::ssh::client::SshCommandResult;
use oi_core::storage::init::InitRequest;

pub struct Evidence {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Default)]
pub struct Report {
    pub suite: String,
    pub results: Vec<Evidence>,
}

impl Report {
    pub fn new(suite: impl Into<String>) -> Self {
        Report {
            suite: suite.into(),
            results: Vec::new(),
        }
    }

    pub fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.results.push(Evidence {
            name: name.into(),
            passed: true,
            detail: detail.into(),
        });
    }

    pub fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.results.push(Evidence {
            name: name.into(),
            passed: false,
            detail: detail.into(),
        });
    }

    pub fn check(&mut self, name: impl Into<String>, passed: bool, detail: impl Into<String>) {
        if passed {
            self.pass(name, detail)
        } else {
            self.fail(name, detail)
        }
    }

    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.passed)
    }

    pub fn finish(&self) -> i32 {
        println!("\n=== {} ===", self.suite);
        for r in &self.results {
            println!(
                "[{}] {} -- {}",
                if r.passed { "PASS" } else { "FAIL" },
                r.name,
                r.detail
            );
        }
        let failed = self.results.iter().filter(|r| !r.passed).count();
        println!(
            "\n{} of {} criteria passed",
            self.results.len() - failed,
            self.results.len()
        );
        if failed == 0 {
            0
        } else {
            1
        }
    }
}

/// Working directory for test artefacts. Everything a test creates lives here.
pub fn workspace_dir() -> PathBuf {
    let base = std::env::var("OI_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // keep the (large) test images off the system drive when a data drive exists
            if Path::new(r"D:\").is_dir() {
                PathBuf::from(r"D:\oi-linux-tests")
            } else {
                std::env::temp_dir().join("oi-linux-tests")
            }
        });
    std::fs::create_dir_all(&base).expect("create the test root");
    base
}

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}

/// Repository root, derived from this crate's manifest directory.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf())
        .expect("the integration tests live inside the repository")
}

/// The shippable base image and QEMU, or a clear reason why the tests cannot run.
pub struct Machine {
    pub qemu: Option<PathBuf>,
    pub base_image: Option<PathBuf>,
}

impl Machine {
    pub fn detect() -> Machine {
        Machine {
            qemu: oi_core::vm::accelerator::locate_qemu(None)
                .ok()
                .map(|q| q.path),
            base_image: oi_core::storage::images::locate_base_image(Some(
                &repo_root().join("vm").join("out"),
            ))
            .ok()
            .map(|(p, _)| p),
        }
    }

    pub fn ready(&self) -> Result<(), String> {
        if self.qemu.is_none() {
            return Err(
                "QEMU was not found; install it or set OI_LINUX_QEMU before running the \
                 blocking acceptance tests"
                    .into(),
            );
        }
        if self.base_image.is_none() {
            return Err(format!(
                "the base image {} was not found; build it with \
                 `powershell -File vm/build-base-image.ps1` (or the matching CI job)",
                repo_root().join("vm").join("out").join("base.qcow2").display()
            ));
        }
        Ok(())
    }
}

/// A complete, real environment: data directory, Windows workspace and the engine that owns the
/// virtual machine and the share service.
pub struct TestEnv {
    pub engine: Engine,
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub workspace: PathBuf,
    pub machine: Machine,
    settings: PathBuf,
    started: bool,
}

impl TestEnv {
    /// Path of the application settings file this environment was opened with.
    pub fn settings_path(&self) -> &Path {
        &self.settings
    }

    /// Create or re-associate the environment for one test binary.
    pub async fn prepare(name: &str) -> Result<TestEnv, String> {
        let machine = Machine::detect();
        machine.ready()?;
        let root = workspace_dir().join(name);
        let data_dir = root.join("data");
        let workspace = root.join("workspace");
        // A previous suite may still be releasing its handles for a moment, so the directory
        // creation is retried instead of failing the whole run with a transient sharing error.
        let mut last_error = String::new();
        for attempt in 0..10 {
            match std::fs::create_dir_all(&workspace) {
                Ok(()) => {
                    last_error.clear();
                    break;
                }
                Err(e) => {
                    last_error = format!("{}: {e}", workspace.display());
                    if attempt == 0 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    } else {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        }
        if !last_error.is_empty() {
            return Err(format!(
                "could not create the test workspace {last_error}"
            ));
        }

        let settings_path = root.join("settings.json");
        let mut engine = Engine::new(&settings_path).with_overrides(
            machine.qemu.clone(),
            Some(repo_root().join("vm").join("out")),
        );

        let fresh = !data_dir.join("environment.json").is_file();
        let request = InitRequest {
            data_dir: data_dir.clone(),
            workspace_dir: workspace.clone(),
            vcpu: 2,
            memory_mib: 1024,
            disk_gib: 8,
            language: Some("zh-CN".into()),
            relink: !fresh,
        };
        let mut progress = |step: oi_core::storage::init::InitStep| {
            println!("  init: {} {}", step.key, step.detail);
        };
        engine
            .initialize(&request, &mut progress)
            .map_err(|e| format!("could not prepare the environment: {e}"))?;
        println!(
            "environment {} at {}",
            if fresh { "created" } else { "re-associated" },
            data_dir.display()
        );

        Ok(TestEnv {
            engine,
            root,
            data_dir,
            workspace,
            machine,
            settings: settings_path,
            started: false,
        })
    }

    pub async fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }
        let outcome = self
            .engine
            .start_vm()
            .await
            .map_err(|e| format!("the Linux environment did not start: {e}"))?;
        println!(
            "started with {} ({}), ssh port {}",
            outcome.accelerator.label(),
            outcome.accelerator_reason,
            outcome.ssh_host_port
        );
        self.started = true;
        Ok(())
    }

    pub async fn stop(&mut self) {
        if !self.started {
            return;
        }
        // Stop the way the application does: ask the guest to power off and wait for it, and only
        // force the process when that times out. A forced stop discards the guest's page cache,
        // so data written shortly before it can be lost - which is why the persistence test needs
        // the graceful path.
        if self.engine.stop_vm(false).await.is_err() {
            let _ = self.engine.stop_vm(true).await;
        }
        self.started = false;
    }

    /// Run a command inside the guest over the production SSH client.
    pub async fn guest(&self, command: &str) -> SshCommandResult {
        self.guest_timeout(command, Duration::from_secs(180)).await
    }

    pub async fn guest_timeout(&self, command: &str, timeout: Duration) -> SshCommandResult {
        let client = self
            .engine
            .guest_client()
            .expect("the guest session must be open");
        client
            .exec_with_timeout(command, timeout)
            .await
            .unwrap_or_else(|e| panic!("command {command:?} failed: {e}"))
    }

    /// Path of a workspace-relative file as the guest sees it.
    pub fn guest_path(&self, relative: &str) -> String {
        format!("/home/student/workspace/{relative}")
    }

    pub fn write_workspace(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.workspace.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create workspace subdirectory");
        }
        std::fs::write(&path, content).expect("write workspace file");
        path
    }

    pub fn read_workspace(&self, relative: &str) -> String {
        std::fs::read_to_string(self.workspace.join(relative)).expect("read workspace file")
    }

    /// Windows-side "editor save": write a temporary file and rename it over the original.
    pub fn replace_workspace(&self, relative: &str, content: &str) {
        let path = self.workspace.join(relative);
        let tmp = self.workspace.join(format!("{relative}.tmp-save"));
        std::fs::write(&tmp, content).expect("write temporary file");
        std::fs::rename(&tmp, &path).expect("rename over the original");
    }

    pub fn share_status(&mut self) -> oi_core::share::ShareStatus {
        self.engine.snapshot().share.status
    }

    pub async fn remount(&mut self) -> oi_core::OiResult<()> {
        self.engine.remount().await
    }

    /// A short, self-contained program that proves execve, dynamic linking and mmap work from
    /// the shared directory.
    pub fn mmap_probe_source() -> &'static str {
        r#"#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

int main() {
    const char *path = "probe-data.bin";
    int fd = ::open(path, O_RDONLY);
    if (fd < 0) { std::perror("open"); return 2; }
    struct stat st{};
    if (::fstat(fd, &st) != 0) { std::perror("fstat"); return 2; }
    // shared map: writes must be visible to a second mapping
    void *shared = ::mmap(nullptr, (size_t)st.st_size, PROT_READ, MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED) { std::perror("mmap shared"); return 3; }
    // private map: copy-on-write must not touch the file
    void *priv = ::mmap(nullptr, (size_t)st.st_size, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (priv == MAP_FAILED) { std::perror("mmap private"); return 4; }
    char *p = static_cast<char *>(priv);
    p[0] = 'X';
    const char *s = static_cast<const char *>(shared);
    if (s[0] == 'X') { std::fprintf(stderr, "private map leaked into the file\n"); return 5; }
    if (std::memcmp(s, "OI-PROBE-DATA", 13) != 0) {
        std::fprintf(stderr, "shared map content mismatch: %.13s\n", s);
        return 6;
    }
    ::munmap(shared, (size_t)st.st_size);
    ::munmap(priv, (size_t)st.st_size);
    ::close(fd);
    std::printf("mmap-ok\n");
    return 0;
}
"#
    }

    pub fn hello_source() -> &'static str {
        "#include <cstdio>\nint main() { std::printf(\"hello-oi\\n\"); return 0; }\n"
    }
}

/// Open a file on the Windows side with no sharing at all, which is what a Windows application
/// holding a file open for writing looks like to the guest.
pub fn open_exclusive(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path)
}

/// Is another process holding a write handle on this file? Used to prove that nothing keeps the
/// qcow2 overlay open after a crash.
pub fn is_exclusively_openable(path: &Path) -> bool {
    match open_exclusive(path) {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Short human readable form of a command result, used in evidence strings.
pub fn summarize(res: &SshCommandResult) -> String {
    let text = format!("{} {}", res.stdout.trim(), res.stderr.trim());
    let trimmed = text.trim();
    if trimmed.chars().count() > 160 {
        format!("{}…", trimmed.chars().take(160).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

/* ------------------------------------------------------------------------------------------ *
 * A real SFTP client for the host share service
 *
 * The hostile-path and permission tests talk to the *production* share service the same way the
 * guest's `sshfs` does: over SSH, authenticating with the per-machine guest identity. Wrapping
 * it here keeps the tests free of any reimplementation.
 * ------------------------------------------------------------------------------------------ */

struct AcceptPinned;

impl russh::client::Handler for AcceptPinned {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // The tests hold the private half of the host key, so pinning it here would only test
        // the test. What matters is that the service *requires* authentication, which is checked
        // separately by attempting an unauthenticated session.
        Ok(true)
    }
}

pub struct SftpTest {
    pub sftp: russh_sftp::client::SftpSession,
    _handle: russh::client::Handle<AcceptPinned>,
}

impl SftpTest {
    pub async fn open(port: u16, identity: &Path) -> SftpTest {
        open_sftp(port, identity, oi_core::GUEST_USER)
            .await
            .expect("open an sftp session")
    }
}

/// Try to open an SFTP session, returning the error instead of panicking.
pub async fn try_open_sftp(
    port: u16,
    identity: &Path,
    user: &str,
) -> Result<SftpTest, String> {
    open_sftp(port, identity, user).await
}

async fn open_sftp(port: u16, identity: &Path, user: &str) -> Result<SftpTest, String> {
    use std::sync::Arc;

    let key = russh::keys::PrivateKey::read_openssh_file(identity)
        .map_err(|e| format!("read {}: {e}", identity.display()))?;
    let config = Arc::new(russh::client::Config {
        inactivity_timeout: Some(Duration::from_secs(120)),
        ..Default::default()
    });
    let mut handle = russh::client::connect(config, ("127.0.0.1", port), AcceptPinned)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let auth = handle
        .authenticate_publickey(
            user.to_string(),
            russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None),
        )
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    if !matches!(auth, russh::client::AuthResult::Success) {
        return Err("the share service refused the identity".to_string());
    }
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("subsystem: {e}"))?;
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("sftp handshake: {e}"))?;
    Ok(SftpTest {
        sftp,
        _handle: handle,
    })
}

/* ------------------------------------------------------------------------------------------ *
 * A raw SFTP session
 *
 * `sshfs` depends on protocol extensions (`posix-rename@openssh.com`, `fsync@openssh.com`), and
 * the guest never uses the version-3 rename when it can avoid it. The high level client does not
 * expose extensions, so the tests that verify them use the raw session.
 * ------------------------------------------------------------------------------------------ */

pub struct RawSftpTest {
    pub session: russh_sftp::client::RawSftpSession,
    pub version: russh_sftp::protocol::Version,
    _handle: russh::client::Handle<AcceptPinned>,
}

impl RawSftpTest {
    pub async fn open(port: u16, identity: &Path) -> Result<RawSftpTest, String> {
        use std::sync::Arc;

        let key = russh::keys::PrivateKey::read_openssh_file(identity)
            .map_err(|e| format!("read {}: {e}", identity.display()))?;
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(Duration::from_secs(120)),
            ..Default::default()
        });
        let mut handle = russh::client::connect(config, ("127.0.0.1", port), AcceptPinned)
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
            return Err("the share service refused the identity".to_string());
        }
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| format!("channel: {e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| format!("subsystem: {e}"))?;
        let session = russh_sftp::client::RawSftpSession::new(channel.into_stream());
        let version = session
            .init()
            .await
            .map_err(|e| format!("sftp init: {e}"))?;
        Ok(RawSftpTest {
            session,
            version,
            _handle: handle,
        })
    }
}

/// Encode the two-string payload the `posix-rename@openssh.com` extension expects.
pub fn two_strings(a: &str, b: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len() + b.len() + 8);
    for value in [a, b] {
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    out
}

/// Create or truncate a file through the share and write its content, honouring an optional
/// permission word exactly like `sshfs` does when the guest creates a file.
pub async fn sftp_write(
    client: &SftpTest,
    path: &str,
    data: &[u8],
    mode: Option<u32>,
) -> Result<(), String> {
    use russh_sftp::protocol::{FileAttributes, OpenFlags};
    use tokio::io::AsyncWriteExt;

    let attrs = FileAttributes {
        permissions: mode,
        ..Default::default()
    };
    let mut file = client
        .sftp
        .open_with_flags_and_attributes(
            path,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            attrs,
        )
        .await
        .map_err(|e| format!("open {path}: {e}"))?;
    file.write_all(data)
        .await
        .map_err(|e| format!("write {path}: {e}"))?;
    file.close().await.map_err(|e| format!("close {path}: {e}"))?;
    Ok(())
}

/// Start the host share service on a scratch root, exactly as the application does.
pub async fn start_share(
    root: &Path,
    identities: &oi_core::storage::keys::SshIdentities,
) -> (oi_core::share::ShareServer, std::sync::Arc<oi_core::share::ModeDb>, String) {
    use std::sync::Arc;

    let material = identities
        .load_public_material()
        .expect("public key material");
    let db = Arc::new(
        oi_core::share::ModeDb::open(&root.join("..").join("metadata.db")).expect("open mode db"),
    );
    let config = oi_core::share::ShareConfig {
        root: root.to_path_buf(),
        ssh_host_key: identities.sftp_host_private(),
        authorized_public_key: material.guest_client_public_openssh.clone(),
        port: 0,
    };
    let (server, mut events) = oi_core::share::ShareServer::start(config, db.clone())
        .await
        .expect("start the share service");
    tokio::spawn(async move {
        while events.recv().await.is_ok() {}
    });
    (server, db, material.guest_client_public_openssh)
}

/// Bind a TCP listener on a random loopback port and return it, for tests that need a free port.
pub fn free_loopback_port() -> u16 {
    oi_core::vm::ports::allocate_loopback_port().expect("allocate a loopback port")
}

/// Does `netstat` show this port listening only on loopback?
pub fn listener_is_loopback_only(port: u16) -> Result<bool, String> {
    let out = std::process::Command::new("netstat")
        .args(["-ano"])
        .output()
        .map_err(|e| format!("netstat failed: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut loopback = false;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        if !fields[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        let local = fields[1];
        let Some(port_text) = local.rsplit(':').next() else {
            continue;
        };
        if port_text != port.to_string() {
            continue;
        }
        if !fields[3].eq_ignore_ascii_case("LISTENING") && !local.contains("ESTABLISHED") {
            // only listening sockets matter
        }
        if local.starts_with("127.0.0.1:") {
            loopback = true;
        } else if local.starts_with("0.0.0.0:") || local.starts_with("[::]:") {
            return Ok(false);
        }
    }
    Ok(loopback)
}
