//! SSH server side of the share service: authentication, subsystem dispatch and refusal of
//! everything that is not the SFTP subsystem.
//!
//! The service offers exactly one operation. Shell, exec, PTY and port forwarding requests are
//! answered with a channel failure so that a compromised guest cannot use the host share as a
//! general purpose remote execution facility (`OILinux_PRD.md` §8.2 assumes guest programs are
//! untrusted).

use std::net::SocketAddr;
use std::sync::Arc;

use russh::keys::ssh_key::PublicKey;
use russh::keys::PrivateKey;
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::net::TcpStream;

use crate::error::{OiError, OiResult};
use crate::share::mode_db::ModeDb;
use crate::share::path_guard::ShareRoot;

pub const SFTP_SUBSYSTEM: &str = "sftp";

/// A single authorised identity, compared by key material rather than by name.
#[derive(Clone)]
pub struct Authorized {
    pub key: PublicKey,
}

impl Authorized {
    pub fn matches(&self, presented: &PublicKey) -> bool {
        self.key == *presented
    }
}

/// The service's shared state. `russh` hands each accepted connection its own handler, which
/// [`new_client`](russh::server::Server::new_client) builds from this value.
pub struct ShareService {
    pub root: ShareRoot,
    pub db: Arc<ModeDb>,
    pub authorized: Authorized,
    pub guest_workspace: String,
}

impl russh::server::Server for ShareService {
    type Handler = ShareConnection;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> ShareConnection {
        tracing::info!(?peer, "share connection accepted");
        ShareConnection::new(
            self.root.clone(),
            self.db.clone(),
            self.authorized.clone(),
            self.guest_workspace.clone(),
        )
    }
}

pub struct ShareConnection {
    root: ShareRoot,
    db: Arc<ModeDb>,
    authorized: Authorized,
    guest_workspace: String,
    /// The session channel, kept so that the SFTP subsystem can take it over as a byte stream.
    channel: Option<Channel<Msg>>,
    authenticated: bool,
}

impl ShareConnection {
    pub fn new(
        root: ShareRoot,
        db: Arc<ModeDb>,
        authorized: Authorized,
        guest_workspace: String,
    ) -> Self {
        ShareConnection {
            root,
            db,
            authorized,
            guest_workspace,
            channel: None,
            authenticated: false,
        }
    }
}

impl russh::server::Handler for ShareConnection {
    type Error = OiError;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        if user != crate::GUEST_USER {
            tracing::warn!(user, "share service refused an unknown account");
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }
        if self.authorized.matches(key) {
            self.authenticated = true;
            Ok(Auth::Accept)
        } else {
            tracing::warn!("share service refused a key that is not the configured identity");
            Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        // The share service never accepts passwords.
        Ok(Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])),
            partial_success: false,
        })
    }

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])),
            partial_success: false,
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        if !self.authenticated {
            return Ok(false);
        }
        self.channel = Some(channel);
        Ok(true)
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != SFTP_SUBSYSTEM {
            tracing::warn!(name, "share service refused a non-sftp subsystem");
            session.channel_failure(channel_id)?;
            return Ok(());
        }
        let Some(channel) = self.channel.take() else {
            session.channel_failure(channel_id)?;
            return Ok(());
        };
        session.channel_success(channel_id)?;

        let root = self.root.clone();
        let db = self.db.clone();
        let workspace = self.guest_workspace.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::share::sftp::run_sftp(channel.into_stream(), root, db, workspace).await {
                tracing::warn!(error = %e, "sftp session ended with an error");
            }
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::warn!("share service refused an exec request");
        session.channel_failure(channel_id)?;
        session.close(channel_id)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel_id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::warn!("share service refused a shell request");
        session.channel_failure(channel_id)?;
        session.close(channel_id)?;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel_id: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel_id)?;
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        _address: &str,
        _port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        // The share service never forwards anything.
        Ok(false)
    }
}

/// Load the host key of the share service from the environment's `ssh` directory.
pub fn load_host_key(path: &std::path::Path) -> OiResult<(PrivateKey, String)> {
    let key = PrivateKey::read_openssh_file(path).map_err(|e| {
        OiError::config(format!(
            "the share service host key {} could not be read: {e}",
            path.display()
        ))
    })?;
    let public_openssh = key
        .public_key()
        .to_openssh()
        .map_err(|e| OiError::config(format!("the share service host key is unusable: {e}")))?;
    Ok((key, public_openssh))
}

/// Parse an `authorized_keys` style line.
pub fn parse_authorized_key(line: &str) -> OiResult<Authorized> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(OiError::config("the authorised key is empty"));
    }
    let key = PublicKey::from_openssh(trimmed)
        .map_err(|e| OiError::config(format!("the authorised public key is unusable: {e}")))?;
    Ok(Authorized { key })
}

/// Serve one accepted TCP connection. Authentication is mandatory; there is no anonymous path.
pub async fn serve_connection(
    stream: TcpStream,
    peer: SocketAddr,
    root: ShareRoot,
    db: Arc<ModeDb>,
    host_key: PrivateKey,
    authorized: Authorized,
    guest_workspace: String,
) -> OiResult<()> {
    let _ = peer;
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        auth_rejection_time: std::time::Duration::from_millis(200),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(50)),
        inactivity_timeout: Some(std::time::Duration::from_secs(3600)),
        nodelay: true,
        ..Default::default()
    });

    let handler = ShareConnection::new(root, db, authorized, guest_workspace);
    let running = russh::server::run_stream(config, stream, handler)
        .await
        .map_err(|e| OiError::ssh(format!("share ssh session failed: {e}")))?;
    if let Err(e) = running.await {
        tracing::debug!(error = %e, "share ssh session ended");
    }
    Ok(())
}
