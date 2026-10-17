//! Boot-time seed configuration disk.
//!
//! `OILinux_PRD.md` §5.3: the first boot delivers the guest's SSH public key material, the
//! Windows SFTP endpoint and the trust anchors through a read-only seed disk instead of baking
//! keys into the public base image. This is *not* a guest agent – the guest side is a one-shot
//! OpenRC script that consumes the disk and exits.
//!
//! The disk is a small ISO9660 image with level-1 names because both sides of the contract are
//! ours:
//!
//! | file          | content                                                |
//! |---------------|--------------------------------------------------------|
//! | `SEED.JSON`   | endpoint, generation, hostname, public keys (no secrets) |
//! | `CLIENT.PUB`  | host client public key, appended to `authorized_keys`  |
//! | `GUESTKEY`    | guest sshd ed25519 host *private* key (per machine)    |
//! | `SFTPKEY`     | guest's own private key for the host share service     |
//! | `WINHOSTS`    | pinned `known_hosts` lines for the host SFTP endpoint  |

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{OiError, OiResult};
use crate::iso::{build_iso9660, IsoFile};

pub const SEED_VOLUME_ID: &str = "OI_SEED";
pub const FILE_SEED_JSON: &str = "SEED.JSON";
pub const FILE_CLIENT_PUB: &str = "CLIENT.PUB";
pub const FILE_GUEST_KEY: &str = "GUESTKEY";
pub const FILE_GUEST_CLIENT_KEY: &str = "SFTPKEY";
pub const FILE_WIN_HOSTS: &str = "WINHOSTS";
pub const SEED_SCHEMA: u32 = 1;

/// Guest-visible address of the host under QEMU user networking.
pub const SLIRP_HOST_ALIAS: &str = "10.0.2.2";
/// Address the reverse tunnel listens on inside the guest.
pub const GUEST_LOOPBACK: &str = "127.0.0.1";

/// The pinned `known_hosts` lines for both possible transports. The ports change on every launch,
/// which is why the seed disk is regenerated whenever they do.
pub fn win_known_hosts(direct_port: u16, tunnel_port: u16, public_key: &str) -> String {
    let key = public_key.trim();
    format!(
        "{}\n{}\n",
        crate::storage::keys::known_hosts_line(SLIRP_HOST_ALIAS, direct_port, key).trim_end(),
        crate::storage::keys::known_hosts_line(GUEST_LOOPBACK, tunnel_port, key).trim_end(),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedSpec {
    pub schema: u32,
    pub generation: u64,
    pub hostname: String,
    pub guest_user: String,
    pub guest_workspace: String,
    pub share_transport: String,
    pub host_sftp_address: String,
    pub host_sftp_port: u16,
    pub reverse_tunnel_port: u16,
    pub client_public_key: String,
    pub sftp_host_public_key: String,
    pub authorized_keys: String,
    pub win_known_hosts: String,
    pub guest_host_private_key: String,
    pub guest_client_private_key: String,
}

impl SeedSpec {
    /// Metadata-only view written as `SEED.JSON`; never contains the host private key.
    pub fn metadata_json(&self) -> OiResult<String> {
        #[derive(Serialize)]
        struct Meta<'a> {
            schema: u32,
            generation: u64,
            hostname: &'a str,
            guest_user: &'a str,
            guest_workspace: &'a str,
            share_transport: &'a str,
            host_sftp_address: &'a str,
            host_sftp_port: u16,
            reverse_tunnel_port: u16,
            client_public_key: &'a str,
            sftp_host_public_key: &'a str,
            applied_at: Option<String>,
        }
        let meta = Meta {
            schema: self.schema,
            generation: self.generation,
            hostname: &self.hostname,
            guest_user: &self.guest_user,
            guest_workspace: &self.guest_workspace,
            share_transport: &self.share_transport,
            host_sftp_address: &self.host_sftp_address,
            host_sftp_port: self.host_sftp_port,
            reverse_tunnel_port: self.reverse_tunnel_port,
            client_public_key: &self.client_public_key,
            sftp_host_public_key: &self.sftp_host_public_key,
            applied_at: None,
        };
        Ok(serde_json::to_string_pretty(&meta)?)
    }

    pub fn build_iso(&self) -> OiResult<Vec<u8>> {
        if self.client_public_key.trim().is_empty() {
            return Err(OiError::config(
                "the seed disk needs the client public key; generate identities first",
            ));
        }
        if self.guest_host_private_key.trim().is_empty() {
            return Err(OiError::config(
                "the seed disk needs the per-machine guest host private key",
            ));
        }
        let files = vec![
            IsoFile::new(FILE_SEED_JSON, self.metadata_json()?.into_bytes()),
            IsoFile::new(
                FILE_CLIENT_PUB,
                format!("{} {}\n", self.client_public_key.trim(), "oi-linux-host").into_bytes(),
            ),
            IsoFile::new(
                FILE_GUEST_KEY,
                format!("{}\n", self.guest_host_private_key.trim()).into_bytes(),
            ),
            IsoFile::new(
                FILE_GUEST_CLIENT_KEY,
                format!("{}\n", self.guest_client_private_key.trim()).into_bytes(),
            ),
            IsoFile::new(FILE_WIN_HOSTS, self.win_known_hosts.clone().into_bytes()),
        ];
        build_iso9660(SEED_VOLUME_ID, &files)
    }

    pub fn write_iso(&self, path: &Path) -> OiResult<u64> {
        let img = self.build_iso()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OiError::io(parent, e))?;
        }
        std::fs::write(path, &img).map_err(|e| OiError::io(path, e))?;
        // The seed carries a private key; keep it owner-only.
        crate::sys::acl::restrict_to_current_user(path)?;
        Ok(img.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SeedSpec {
        SeedSpec {
            schema: SEED_SCHEMA,
            generation: 1,
            hostname: "oi-linux".into(),
            guest_user: "student".into(),
            guest_workspace: "/home/student/workspace".into(),
            share_transport: "reverse_tunnel".into(),
            host_sftp_address: SLIRP_HOST_ALIAS.into(),
            host_sftp_port: 55123,
            reverse_tunnel_port: 55124,
            client_public_key: "ssh-ed25519 AAAACLIENT oi-linux".into(),
            sftp_host_public_key: "ssh-ed25519 AAAASFTP host".into(),
            authorized_keys: "ssh-ed25519 AAAACLIENT oi-linux\n".into(),
            win_known_hosts: win_known_hosts(55123, 55124, "ssh-ed25519 AAAASFTP host"),
            guest_host_private_key: "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----".into(),
            guest_client_private_key: "-----BEGIN OPENSSH PRIVATE KEY-----\nBBBB\n-----END OPENSSH PRIVATE KEY-----".into(),
        }
    }

    #[test]
    fn metadata_json_never_contains_the_private_key() {
        let json = spec().metadata_json().unwrap();
        assert!(json.contains("reverse_tunnel_port"));
        assert!(!json.contains("PRIVATE KEY"));
        assert!(!json.contains("AAAA\n"));
    }

    #[test]
    fn seed_iso_contains_all_five_files() {
        let img = spec().build_iso().unwrap();
        assert_eq!(&img[16 * 2048 + 40..16 * 2048 + 47], b"OI_SEED");
        for name in [
            "SEED.JSON;1",
            "CLIENT.PUB;1",
            "GUESTKEY;1",
            "SFTPKEY;1",
            "WINHOSTS;1",
        ] {
            assert!(
                img.windows(name.len())
                    .any(|w| w == name.as_bytes()),
                "seed image is missing {name}"
            );
        }
    }

    #[test]
    fn known_hosts_pins_both_transports() {
        let text = win_known_hosts(55123, 55124, "ssh-ed25519 AAAASFTP host");
        assert!(text.contains("[10.0.2.2]:55123 ssh-ed25519 AAAASFTP host"));
        assert!(text.contains("[127.0.0.1]:55124 ssh-ed25519 AAAASFTP host"));
    }

    #[test]
    fn seed_without_keys_is_refused() {
        let mut s = spec();
        s.guest_host_private_key = String::new();
        assert!(s.build_iso().is_err());
        let mut s = spec();
        s.client_public_key = "  ".into();
        assert!(s.build_iso().is_err());
    }
}
