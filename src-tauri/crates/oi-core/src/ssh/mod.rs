//! Guest SSH connectivity: command execution, interactive PTY sessions and the share transport.

pub mod bridge;
pub mod client;

pub use bridge::{
    guest_ssh_options, health_check, is_mounted, mount, new_nonce, probe_direct, shell_quote,
    unmount, BridgeState, DirectProbe, HealthReport, ShareTransport, GUEST_LOOPBACK, SLIRP_HOST,
};
pub use client::{
    wait_for_guest, GuestClient, GuestConnectOptions, SshCommandResult, TerminalEvent,
    TerminalSession,
};
