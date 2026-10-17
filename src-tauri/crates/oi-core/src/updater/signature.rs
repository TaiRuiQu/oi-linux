//! Update package signature verification.
//!
//! The Windows installer itself is not Authenticode signed in V1, so the update package is what
//! carries cryptographic trust: the manifest signature is verified against a public key compiled
//! into the binary before anything is downloaded or installed.
//!
//! The format is the one `minisign`/Tauri use: base64 of
//!
//! ```text
//! public key : "Ed" | key id (8) | ed25519 public key (32)   = 42 bytes
//! signature  : "Ed" | key id (8) | ed25519 signature (64)    = 74 bytes
//! ```
//!
//! A signature whose key id or algorithm does not match the pinned public key is rejected.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::error::{OiError, OiResult};

const ALGO_ED: &[u8; 2] = b"Ed";

#[derive(Debug, Clone)]
pub struct UpdatePublicKey {
    pub key_id: [u8; 8],
    pub key: VerifyingKey,
}

pub fn decode_public_key(encoded: &str) -> OiResult<UpdatePublicKey> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|e| OiError::other(format!("the update public key is not base64: {e}")))?;
    if raw.len() != 42 {
        return Err(OiError::other(format!(
            "the update public key has {} bytes, expected 42",
            raw.len()
        )));
    }
    if &raw[0..2] != ALGO_ED {
        return Err(OiError::other(
            "the update public key does not use the expected Ed algorithm",
        ));
    }
    let mut key_id = [0u8; 8];
    key_id.copy_from_slice(&raw[2..10]);
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&raw[10..42]);
    let key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| OiError::other(format!("the update public key is invalid: {e}")))?;
    Ok(UpdatePublicKey { key_id, key })
}

/// Verify `signature` over `data`. `signature` may be the raw base64 blob or a whole minisign
/// signature file, in which case the base64 line is picked out.
pub fn verify(public_key: &UpdatePublicKey, data: &[u8], signature: &str) -> OiResult<()> {
    let encoded = signature
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment"))
        .ok_or_else(|| OiError::other("the update signature file is empty"))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| OiError::other(format!("the update signature is not base64: {e}")))?;
    if raw.len() < 74 {
        return Err(OiError::other(format!(
            "the update signature has {} bytes, expected at least 74",
            raw.len()
        )));
    }
    if &raw[0..2] != ALGO_ED {
        return Err(OiError::other(
            "the update signature does not use the expected Ed algorithm",
        ));
    }
    if raw[2..10] != public_key.key_id {
        return Err(OiError::other(
            "the update signature was produced by a different key than the one pinned in this build",
        ));
    }
    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(&raw[10..74]);
    let sig = Signature::from_bytes(&sig_bytes);
    public_key
        .key
        .verify(data, &sig)
        .map_err(|e| OiError::other(format!("the update package signature is not valid: {e}")))
}

/// Encode a public key in the format the updater configuration expects (used by the release job).
pub fn encode_public_key(key_id: [u8; 8], key: &VerifyingKey) -> String {
    let mut raw = Vec::with_capacity(42);
    raw.extend_from_slice(ALGO_ED);
    raw.extend_from_slice(&key_id);
    raw.extend_from_slice(key.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (SigningKey, UpdatePublicKey, [u8; 8]) {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let key_id = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let pubkey = UpdatePublicKey {
            key_id,
            key: signing.verifying_key(),
        };
        (signing, pubkey, key_id)
    }

    fn sign(signing: &SigningKey, key_id: [u8; 8], data: &[u8]) -> String {
        let sig = signing.sign(data);
        let mut raw = Vec::with_capacity(74);
        raw.extend_from_slice(ALGO_ED);
        raw.extend_from_slice(&key_id);
        raw.extend_from_slice(&sig.to_bytes());
        base64::engine::general_purpose::STANDARD.encode(raw)
    }

    #[test]
    fn a_valid_signature_is_accepted() {
        let (signing, pubkey, key_id) = keypair();
        let data = b"installer bytes";
        let sig = sign(&signing, key_id, data);
        verify(&pubkey, data, &sig).unwrap();
    }

    #[test]
    fn tampered_bytes_are_rejected() {
        let (signing, pubkey, key_id) = keypair();
        let sig = sign(&signing, key_id, b"installer bytes");
        let err = verify(&pubkey, b"installer bytez", &sig).unwrap_err();
        assert!(err.to_string().contains("not valid"), "{err}");
    }

    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let (_signing, pubkey, _key_id) = keypair();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let sig = sign(&other, pubkey.key_id, b"payload");
        let err = verify(&pubkey, b"payload", &sig).unwrap_err();
        assert!(err.to_string().contains("not valid"), "{err}");
    }

    #[test]
    fn a_signature_with_a_mismatched_key_id_is_rejected_before_verification() {
        let (signing, pubkey, _key_id) = keypair();
        let sig = sign(&signing, [9u8; 8], b"payload");
        let err = verify(&pubkey, b"payload", &sig).unwrap_err();
        assert!(err.to_string().contains("different key"), "{err}");
    }

    #[test]
    fn public_keys_round_trip() {
        let (_signing, pubkey, key_id) = keypair();
        let encoded = encode_public_key(key_id, &pubkey.key);
        let decoded = decode_public_key(&encoded).unwrap();
        assert_eq!(decoded.key_id, key_id);
        assert_eq!(decoded.key, pubkey.key);
    }

    #[test]
    fn malformed_public_keys_are_rejected() {
        assert!(decode_public_key("not base64!!").is_err());
        assert!(decode_public_key(&base64::engine::general_purpose::STANDARD.encode([0u8; 10])).is_err());
    }
}
