//! OI Linux host core library.
//!
//! Module layout follows the architecture baseline in `OILinux_PRD.md` §13:
//!
//! * [`vm`]      – QEMU process control, WHPX/TCG selection, QMP, Job Objects, lifecycle FSM
//! * [`ssh`]     – guest SSH client, PTY sessions, share transport bridging
//! * [`share`]   – host side SFTP service, POSIX mode metadata, path safety
//! * [`storage`] – data directory layout, qcow2 images, initialisation/reset, config
//! * [`updater`] – Stable/Beta channel handling
//! * [`logging`] – tracing setup and local diagnostics
//!
//! The crate is deliberately GUI-free so that the same production code paths can be
//! exercised by `integration-tests/` without launching the Tauri shell.

pub mod engine;
pub mod error;
pub mod iso;
pub mod logging;
pub mod share;
pub mod ssh;
pub mod storage;
pub mod sys;
pub mod timeutil;
pub mod updater;
pub mod vm;

pub use error::{OiError, OiResult};

/// Product version string, taken from Cargo metadata.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Alpine release pinned by this build of OI Linux. The value is what the shipped
/// `base.qcow2` was built from; it is not used to reach the network.
pub const ALPINE_VERSION: &str = "3.24.2";

/// Guest account created by the base image.
pub const GUEST_USER: &str = "student";

/// Guest home directory.
pub const GUEST_HOME: &str = "/home/student";

/// Fixed mount point for the shared Windows workspace inside the guest.
pub const GUEST_WORKSPACE: &str = "/home/student/workspace";

/// Where the guest keeps the private key it uses towards the host share service.
pub const GUEST_SHARE_KEY: &str = "/home/student/.ssh/id_ed25519";
