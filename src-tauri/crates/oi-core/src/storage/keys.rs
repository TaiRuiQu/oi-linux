//! Per-machine SSH identity material.
//!
//! Four independent ed25519 identities are created at initialisation time, so that neither
//! direction of the connection ever has to trust an unverified key
//! (`OILinux_PRD.md` §5.3):
//!
//! * `client_ed25519` – Windows uses it to authenticate to the guest; the guest receives the
//!   public half through the seed disk.
//! * `guest_host_ed25519` – the guest sshd host key. It is generated on Windows and injected
//!   through the seed disk, so the Windows client can pin it from the very first connection
//!   instead of trusting whatever answers first.
//! * `guest_client_ed25519` – the guest's own identity towards the host share service. The
//!   public half authorises it inside the share service; the private half is delivered to the
//!   guest's own system directory through the seed disk, which §5.3 explicitly permits.
//! * `sftp_host_ed25519` – the host SFTP/SSH server's host key. The guest receives the public
//!   half through the seed disk and pins it in `known_hosts`.

use std::path::{Path, PathBuf};

use ssh_key::rand_core::OsRng;
use ssh_key::{Algorithm, LineEnding, PrivateKey};

use crate::error::{OiError, OiResult};

pub const CLIENT_KEY: &str = "client_ed25519";
pub const GUEST_HOST_KEY: &str = "guest_host_ed25519";
pub const GUEST_CLIENT_KEY: &str = "guest_client_ed25519";
pub const SFTP_HOST_KEY: &str = "sftp_host_ed25519";

#[derive(Debug, Clone)]
pub struct SshIdentities {
    dir: PathBuf,
}

/// Public halves plus the private halves that have to travel to the guest.
#[derive(Debug, Clone)]
pub struct KeyMaterial {
    pub client_public_openssh: String,
    pub guest_host_public_openssh: String,
    /// Guest sshd host private key, injected through the seed disk on first boot.
    pub guest_host_private_openssh: String,
    /// The guest's private key for the host share service.
    pub guest_client_public_openssh: String,
    pub guest_client_private_openssh: String,
    pub sftp_host_public_openssh: String,
    pub sftp_host_private_openssh: String,
}

impl SshIdentities {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        SshIdentities { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn private_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn public_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.pub"))
    }

    pub fn client_private(&self) -> PathBuf {
        self.private_path(CLIENT_KEY)
    }

    pub fn client_public(&self) -> PathBuf {
        self.public_path(CLIENT_KEY)
    }

    pub fn guest_host_private(&self) -> PathBuf {
        self.private_path(GUEST_HOST_KEY)
    }

    pub fn guest_host_public(&self) -> PathBuf {
        self.public_path(GUEST_HOST_KEY)
    }

    pub fn guest_client_private(&self) -> PathBuf {
        self.private_path(GUEST_CLIENT_KEY)
    }

    pub fn guest_client_public(&self) -> PathBuf {
        self.public_path(GUEST_CLIENT_KEY)
    }

    pub fn sftp_host_private(&self) -> PathBuf {
        self.private_path(SFTP_HOST_KEY)
    }

    pub fn sftp_host_public(&self) -> PathBuf {
        self.public_path(SFTP_HOST_KEY)
    }

    pub fn exists(&self) -> bool {
        self.client_private().is_file()
            && self.guest_host_private().is_file()
            && self.guest_client_private().is_file()
            && self.sftp_host_private().is_file()
    }

    /// Generate all four identities and write them with a restrictive ACL. Returns the material
    /// needed to build the seed disk.
    pub fn generate_all(&self) -> OiResult<KeyMaterial> {
        std::fs::create_dir_all(&self.dir).map_err(|e| OiError::io(&self.dir, e))?;
        let client = generate_keypair(&self.dir, CLIENT_KEY)?;
        let guest_host = generate_keypair(&self.dir, GUEST_HOST_KEY)?;
        let guest_client = generate_keypair(&self.dir, GUEST_CLIENT_KEY)?;
        let sftp_host = generate_keypair(&self.dir, SFTP_HOST_KEY)?;

        crate::sys::acl::restrict_tree_to_current_user(&self.dir)?;

        Ok(KeyMaterial {
            client_public_openssh: client.public_openssh,
            guest_host_public_openssh: guest_host.public_openssh,
            guest_host_private_openssh: guest_host.private_openssh,
            guest_client_public_openssh: guest_client.public_openssh,
            guest_client_private_openssh: guest_client.private_openssh,
            sftp_host_public_openssh: sftp_host.public_openssh,
            sftp_host_private_openssh: sftp_host.private_openssh,
        })
    }

    /// Load the public halves that already exist on disk.
    pub fn load_public_material(&self) -> OiResult<KeyMaterial> {
        Ok(KeyMaterial {
            client_public_openssh: read_trimmed(&self.client_public())?,
            guest_host_public_openssh: read_trimmed(&self.guest_host_public())?,
            guest_host_private_openssh: String::new(),
            guest_client_public_openssh: read_trimmed(&self.guest_client_public())?,
            guest_client_private_openssh: String::new(),
            sftp_host_public_openssh: read_trimmed(&self.sftp_host_public())?,
            sftp_host_private_openssh: String::new(),
        })
    }
}

struct GeneratedKey {
    public_openssh: String,
    private_openssh: String,
}

fn generate_keypair(dir: &Path, name: &str) -> OiResult<GeneratedKey> {
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .map_err(|e| OiError::Other(format!("ed25519 key generation failed: {e}")))?;
    let private = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| OiError::Other(format!("could not serialise private key: {e}")))?
        .to_string();
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|e| OiError::Other(format!("could not serialise public key: {e}")))?;

    let priv_path = dir.join(name);
    std::fs::write(&priv_path, private.as_bytes()).map_err(|e| OiError::io(&priv_path, e))?;
    crate::sys::acl::restrict_to_current_user(&priv_path)?;
    let pub_path = dir.join(format!("{name}.pub"));
    std::fs::write(&pub_path, format!("{public}\n").as_bytes())
        .map_err(|e| OiError::io(&pub_path, e))?;
    crate::sys::acl::restrict_to_current_user(&pub_path)?;

    Ok(GeneratedKey {
        public_openssh: public,
        private_openssh: private,
    })
}

fn read_trimmed(path: &Path) -> OiResult<String> {
    let s = std::fs::read_to_string(path).map_err(|e| OiError::io(path, e))?;
    Ok(s.trim_end().to_string())
}

/// `known_hosts` style line for a key, as used when pinning the host SFTP endpoint inside the
/// guest. The bracket form is required for non-default ports.
pub fn known_hosts_line(host: &str, port: u16, public_openssh: &str) -> String {
    format!("[{host}]:{port} {public_openssh}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_generated_and_reloadable() {
        let dir = tempfile::tempdir().unwrap();
        let ids = SshIdentities::new(dir.path().join("ssh"));
        let material = ids.generate_all().unwrap();
        assert!(material.client_public_openssh.starts_with("ssh-ed25519 "));
        assert!(material
            .guest_host_private_openssh
            .contains("BEGIN OPENSSH PRIVATE KEY"));
        assert!(ids.exists());
        let reloaded = ids.load_public_material().unwrap();
        assert_eq!(reloaded.client_public_openssh, material.client_public_openssh);
        assert_eq!(
            reloaded.sftp_host_public_openssh,
            material.sftp_host_public_openssh
        );
    }

    #[test]
    fn generated_keys_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let ids = SshIdentities::new(dir.path());
        let m = ids.generate_all().unwrap();
        assert_ne!(m.client_public_openssh, m.guest_host_public_openssh);
        assert_ne!(m.guest_host_public_openssh, m.sftp_host_public_openssh);
    }

    #[test]
    fn known_hosts_uses_bracket_form_for_custom_ports() {
        assert_eq!(
            known_hosts_line("10.0.2.2", 55123, "ssh-ed25519 AAA"),
            "[10.0.2.2]:55123 ssh-ed25519 AAA\n"
        );
    }
}
