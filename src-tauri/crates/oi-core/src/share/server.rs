//! Host side SFTP service.
//!
//! The service is a small SSH server that offers exactly one thing: the `sftp` subsystem rooted
//! at the configured NTFS workspace directory. Shell, exec and port forwarding requests are all
//! refused. Authentication is always required: the guest presents the private half of the
//! identity generated per machine (`OILinux_PRD.md` §7.2, §8.2).
//!
//! The listener binds to loopback only, so nothing on a school network can reach it. Inside the
//! guest the endpoint is reached either directly through QEMU's user-mode host alias or through
//! an SSH reverse tunnel opened by the host over the existing guest connection.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::share::mode_db::ModeDb;
use crate::share::path_guard::ShareRoot;

#[derive(Debug, Clone)]
pub struct ShareConfig {
    pub root: PathBuf,
    pub ssh_host_key: PathBuf,
    pub authorized_public_key: String,
    /// 0 means "let the OS choose", which is what the launcher uses for a random port.
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareStatus {
    NotMounted,
    Mounting,
    Mounted,
    Degraded,
    Recovering,
    Error,
}

/// A running share service: the listener plus the metadata database behind it.
pub struct ShareServer {
    pub port: u16,
    handle: Option<tokio::task::JoinHandle<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ShareServer {
    /// Bind the listener before returning, so the caller knows the real port immediately.
    pub async fn start(
        config: ShareConfig,
        db: Arc<ModeDb>,
    ) -> OiResult<(ShareServer, tokio::sync::broadcast::Receiver<()>)> {
        let root = ShareRoot::new(&config.root)?;
        if config.authorized_public_key.trim().is_empty() {
            return Err(OiError::config(
                "the share service refuses to run without an authorised key",
            ));
        }
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
            .await
            .map_err(|e| {
                OiError::ShareUnavailable(format!(
                    "could not bind the share listener on loopback port {}: {e}",
                    config.port
                ))
            })?;
        let port = listener
            .local_addr()
            .map_err(|e| OiError::ShareUnavailable(format!("listener address unavailable: {e}")))?
            .port();

        let (host_key, host_key_openssh) = crate::share::protocol::load_host_key(&config.ssh_host_key)?;
        let authorized = crate::share::protocol::parse_authorized_key(&config.authorized_public_key)?;
        tracing::info!(
            port,
            root = %root.configured().display(),
            host_key = %host_key_openssh.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
            "share service listening on loopback"
        );

        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let stop = tx.clone();

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        let _ = stop.send(());
                        break;
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, peer)) => {
                                let root = root.clone();
                                let db = db.clone();
                                let host_key = host_key.clone();
                                let authorized = authorized.clone();
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        crate::share::protocol::serve_connection(stream, peer, root, db, host_key, authorized, crate::GUEST_WORKSPACE.to_string()).await
                                    {
                                        tracing::warn!(error = %e, "share connection ended with an error");
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "share listener accept failed");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok((
            ShareServer {
                port,
                handle: Some(handle),
                shutdown: Some(shutdown_tx),
            },
            rx,
        ))
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
        }
    }
}
