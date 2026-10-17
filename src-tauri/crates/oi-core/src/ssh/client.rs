//! Guest SSH client: command execution and interactive PTY sessions.
//!
//! The Windows side is always the SSH *client* towards the guest. The guest's host key is pinned
//! to the per-machine key that was generated during initialisation and injected through the seed
//! disk, so the very first connection is already verified and there is never a "trust on first
//! use" step (`OILinux_PRD.md` §5.3).
//!
//! Session kinds:
//!
//! * one-shot commands (`exec`) used for share mounting, health probes and diagnostics,
//! * per-tab interactive shells on their own PTY, one SSH session each.

use std::path::PathBuf;
use std::sync::Arc;

use russh::client::{self, Handle};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};
use tokio::sync::{mpsc, Mutex};

use crate::error::{OiError, OiResult};

/// Events produced by an interactive terminal session.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    Data(Vec<u8>),
    Exit(Option<u32>),
    Closed,
}

#[derive(Debug, Clone)]
pub struct GuestConnectOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub client_key: PathBuf,
    /// Pinned guest sshd host key, as an `authorized_keys`/`known_hosts` style line.
    pub host_public_key: String,
}

impl GuestConnectOptions {
    pub fn localhost(port: u16, user: impl Into<String>, client_key: PathBuf, host_public_key: impl Into<String>) -> Self {
        GuestConnectOptions {
            host: "127.0.0.1".to_string(),
            port,
            user: user.into(),
            client_key,
            host_public_key: host_public_key.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SshCommandResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl SshCommandResult {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

struct ClientHandler {
    pinned: PublicKey,
    /// Where forwarded channels are routed: the loopback port of the host share listener.
    share_target: Arc<Mutex<Option<u16>>>,
}

impl client::Handler for ClientHandler {
    type Error = OiError;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let matches = server_public_key == &self.pinned;
        if !matches {
            tracing::error!(
                presented = %server_public_key.to_openssh().unwrap_or_default(),
                "the guest presented a host key that is not the pinned per-machine key"
            );
        }
        Ok(matches)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = *self.share_target.lock().await;
        let Some(port) = target else {
            tracing::warn!("a forwarded channel arrived before the share target was set");
            return Ok(());
        };
        tokio::spawn(async move {
            let mut remote = channel.into_stream();
            match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                Ok(mut local) => {
                    if let Err(e) = tokio::io::copy_bidirectional(&mut remote, &mut local).await {
                        tracing::debug!(error = %e, "forwarded share channel closed");
                    }
                }
                Err(e) => {
                    tracing::warn!(port, error = %e, "the share listener refused a forwarded channel");
                }
            }
        });
        Ok(())
    }
}

pub struct GuestClient {
    handle: Handle<ClientHandler>,
    share_target: Arc<Mutex<Option<u16>>>,
    pub options: GuestConnectOptions,
}

impl GuestClient {
    pub async fn connect(options: GuestConnectOptions) -> OiResult<Self> {
        let key = PrivateKey::read_openssh_file(&options.client_key).map_err(|e| {
            OiError::ssh(format!(
                "the client key {} could not be read: {e}",
                options.client_key.display()
            ))
        })?;
        let pinned = PublicKey::from_openssh(&options.host_public_key).map_err(|e| {
            OiError::ssh(format!("the pinned guest host key is unusable: {e}"))
        })?;

        let share_target = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            pinned,
            share_target: share_target.clone(),
        };

        let config = Arc::new(client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(24 * 3600)),
            nodelay: true,
            ..Default::default()
        });

        let mut handle = client::connect(config, (options.host.as_str(), options.port), handler)
            .await
            .map_err(|e| {
                OiError::ssh(format!(
                    "could not open an SSH connection to {}:{}: {e}",
                    options.host, options.port
                ))
            })?;

        let auth = handle
            .authenticate_publickey(
                options.user.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), None),
            )
            .await
            .map_err(|e| OiError::ssh(format!("SSH authentication failed: {e}")))?;

        if !matches!(auth, russh::client::AuthResult::Success) {
            return Err(OiError::ssh(
                "the guest refused the host's public key; the seed configuration disk has not been applied",
            ));
        }

        Ok(GuestClient {
            handle,
            share_target,
            options,
        })
    }

    pub fn set_share_target(&self, port: Option<u16>) {
        let target = self.share_target.clone();
        tokio::spawn(async move {
            *target.lock().await = port;
        });
    }

    pub async fn exec(&self, command: &str) -> OiResult<SshCommandResult> {
        self.exec_with_timeout(command, std::time::Duration::from_secs(120))
            .await
    }

    pub async fn exec_with_timeout(
        &self,
        command: &str,
        timeout: std::time::Duration,
    ) -> OiResult<SshCommandResult> {
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| OiError::ssh(format!("could not open an SSH session: {e}")))?;
        channel
            .exec(true, command.as_bytes().to_vec())
            .await
            .map_err(|e| OiError::ssh(format!("could not start {command:?}: {e}")))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut status: Option<u32> = None;

        let collect = async {
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, ext } => {
                        if ext == 1 {
                            stderr.extend_from_slice(&data);
                        } else {
                            stdout.extend_from_slice(&data);
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    ChannelMsg::Eof | ChannelMsg::Close => {}
                    _ => {}
                }
            }
        };

        tokio::time::timeout(timeout, collect)
            .await
            .map_err(|_| OiError::timeout(format!("the command {command:?} did not finish in time")))?;

        Ok(SshCommandResult {
            status: status.map(|s| s as i32).unwrap_or(0),
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        })
    }

    /// Open one interactive shell on its own PTY. Each terminal tab owns one of these.
    pub async fn open_terminal(
        &self,
        id: impl Into<String>,
        cols: u32,
        rows: u32,
        events: mpsc::UnboundedSender<TerminalEvent>,
    ) -> OiResult<TerminalSession> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| OiError::ssh(format!("could not open an SSH session: {e}")))?;
        channel
            .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
            .await
            .map_err(|e| OiError::ssh(format!("the guest refused a PTY request: {e}")))?;
        channel
            .request_shell(true)
            .await
            .map_err(|e| OiError::ssh(format!("the guest refused a shell request: {e}")))?;

        let (read_half, write_half) = channel.split();
        let id = id.into();
        let session_id = id.clone();
        tokio::spawn(async move {
            pump_terminal(read_half, events).await;
            tracing::debug!(session = %session_id, "terminal session finished");
        });

        Ok(TerminalSession { id, write_half })
    }

    /// Ask the guest to listen on `127.0.0.1:guest_port` and forward every connection back to the
    /// host's share listener. This is how the guest reaches the host SFTP service without the
    /// service ever being reachable from the network.
    pub async fn open_reverse_forward(&mut self, guest_port: u32, share_listen_port: u16) -> OiResult<u32> {
        *self.share_target.lock().await = Some(share_listen_port);
        let bound = self
            .handle
            .tcpip_forward("127.0.0.1", guest_port)
            .await
            .map_err(|e| OiError::Bridge(format!("the guest refused the reverse tunnel on port {guest_port}: {e}")))?;
        tracing::info!(guest_port = bound, share_port = share_listen_port, "reverse tunnel established");
        Ok(bound)
    }

    pub async fn cancel_reverse_forward(&mut self, guest_port: u32) -> OiResult<()> {
        self.handle
            .cancel_tcpip_forward("127.0.0.1", guest_port)
            .await
            .map_err(|e| OiError::Bridge(format!("could not close the reverse tunnel: {e}")))
    }

    pub async fn is_alive(&self) -> bool {
        self.handle
            .channel_open_session()
            .await
            .map(|c| {
                tokio::spawn(async move {
                    let _ = c.eof().await;
                });
                true
            })
            .unwrap_or(false)
    }

    pub async fn close(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    }
}

pub struct TerminalSession {
    pub id: String,
    write_half: ChannelWriteHalf<client::Msg>,
}

impl TerminalSession {
    pub async fn write(&self, data: &[u8]) -> OiResult<()> {
        self.write_half
            .data(data)
            .await
            .map_err(|e| OiError::ssh(format!("terminal write failed: {e}")))
    }

    pub async fn resize(&self, cols: u32, rows: u32) -> OiResult<()> {
        self.write_half
            .window_change(cols, rows, 0, 0)
            .await
            .map_err(|e| OiError::ssh(format!("terminal resize failed: {e}")))
    }

    pub async fn eof(&self) {
        let _ = self.write_half.eof().await;
    }

    pub async fn close(self) {
        let _ = self.write_half.close().await;
    }
}

async fn pump_terminal(mut read_half: ChannelReadHalf, events: mpsc::UnboundedSender<TerminalEvent>) {
    let mut exit: Option<u32> = None;
    while let Some(msg) = read_half.wait().await {
        match msg {
            ChannelMsg::Data { data } => {
                if events.send(TerminalEvent::Data(data.to_vec())).is_err() {
                    return;
                }
            }
            ChannelMsg::ExtendedData { data, .. } => {
                if events.send(TerminalEvent::Data(data.to_vec())).is_err() {
                    return;
                }
            }
            ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    let _ = events.send(TerminalEvent::Exit(exit));
    let _ = events.send(TerminalEvent::Closed);
}

/// Retry until the guest accepts an SSH connection, or until the deadline passes. Used right
/// after a launch and again after a manual remount request.
pub async fn wait_for_guest(
    options: &GuestConnectOptions,
    timeout: std::time::Duration,
) -> OiResult<GuestClient> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_error;
    loop {
        match GuestClient::connect(options.clone()).await {
            Ok(client) => return Ok(client),
            Err(e) => last_error = e.to_string(),
        }
        if std::time::Instant::now() >= deadline {
            return Err(OiError::timeout(format!(
                "the guest did not become reachable over SSH within {}s ({last_error})",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }
}
