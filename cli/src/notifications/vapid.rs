//! VAPID key generation for Web Push (RFC 8292).
//!
//! Generates P-256 ECDSA keypairs. Keys are stored encrypted at the
//! device level (shared across all hubs) via `persistence.rs`.

// Rust guideline compliant 2026-02

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL, Engine};
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::rand_core::OsRng;
use serde::{Deserialize, Serialize};

/// VAPID keypair for web push authentication.
///
/// The private key is a P-256 ECDSA signing key stored as the raw 32-byte
/// scalar (base64url). The public key is the uncompressed SEC1 point (65 bytes).
///
/// We store the raw scalar (not SEC1 DER, not PKCS8 DER) because the web-push
/// crate's `VapidSignatureBuilder::from_base64()` expects exactly this format,
/// and `from_der()` panics on SEC1 DER from p256 due to a bug in sec1_decode.
#[derive(Debug, Serialize, Deserialize)]
pub struct VapidKeys {
    /// Raw 32-byte P-256 private key scalar (base64url).
    private_key_b64: String,
    /// Uncompressed public key bytes (base64url, 65 bytes decoded).
    public_key_b64: String,
}

impl VapidKeys {
    /// Generate a fresh VAPID keypair.
    pub fn generate() -> Result<Self> {
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        // SEC1 uncompressed public key (65 bytes: 0x04 || x || y)
        let public_bytes = verifying_key.to_encoded_point(false);
        let public_key_b64 = BASE64URL.encode(public_bytes.as_bytes());

        // Raw 32-byte private key scalar for web-push from_base64()
        let private_key_b64 = BASE64URL.encode(signing_key.to_bytes().as_slice());

        Ok(Self {
            private_key_b64,
            public_key_b64,
        })
    }

    /// Base64url-encoded uncompressed public key (65 bytes decoded).
    ///
    /// This is sent to browsers as the VAPID `applicationServerKey`.
    pub fn public_key_base64url(&self) -> &str {
        &self.public_key_b64
    }

    /// Base64url-encoded raw 32-byte private key scalar.
    ///
    /// Used by `web-push` crate's `VapidSignatureBuilder::from_base64()` and
    /// in the copy flow (Device A → browser → Device B via `vapid_key_set`).
    pub fn private_key_base64url(&self) -> &str {
        &self.private_key_b64
    }

    /// Reconstruct from base64url-encoded strings (copy flow).
    ///
    /// Device B receives pre-existing keys from Device A via the browser.
    /// Validates both the public key format and the private key scalar.
    pub fn from_base64url(public_key_b64: &str, private_key_b64: &str) -> Result<Self> {
        // Validate public key: must be 65-byte uncompressed P-256 point
        let pub_bytes = BASE64URL
            .decode(public_key_b64)
            .context("Invalid base64url for VAPID public key")?;
        anyhow::ensure!(
            pub_bytes.len() == 65 && pub_bytes[0] == 0x04,
            "VAPID public key must be 65-byte uncompressed P-256 point"
        );

        // Validate private key: must be 32-byte P-256 scalar
        let priv_bytes = BASE64URL
            .decode(private_key_b64)
            .context("Invalid base64url for VAPID private key")?;
        anyhow::ensure!(
            priv_bytes.len() == 32,
            "VAPID private key must be 32-byte P-256 scalar, got {} bytes",
            priv_bytes.len()
        );
        SigningKey::from_bytes(priv_bytes.as_slice().into())
            .context("VAPID private key is not a valid P-256 scalar")?;

        Ok(Self {
            private_key_b64: private_key_b64.to_string(),
            public_key_b64: public_key_b64.to_string(),
        })
    }

    /// Migrate legacy key formats (SEC1 DER, PKCS8 DER) to raw 32-byte scalar.
    ///
    /// Early versions stored the private key as SEC1 DER (~109 bytes) or
    /// PKCS8 DER (~138 bytes). The web-push crate needs the raw 32-byte scalar.
    /// If the key is already 32 bytes, this is a no-op.
    pub fn migrate_if_needed(self) -> Result<Self> {
        let priv_bytes = BASE64URL
            .decode(&self.private_key_b64)
            .context("Failed to decode VAPID private key")?;

        if priv_bytes.len() == 32 {
            return Ok(self);
        }

        // Try SEC1 DER first (109 bytes typically), then PKCS8 DER (~138 bytes)
        let signing_key = if let Ok(sk) = p256::SecretKey::from_sec1_der(&priv_bytes) {
            SigningKey::from(sk)
        } else {
            use p256::pkcs8::DecodePrivateKey;
            let sk = SigningKey::from_pkcs8_der(&priv_bytes)
                .context("VAPID private key is not valid 32-byte scalar, SEC1 DER, or PKCS8 DER")?;
            sk
        };

        log::info!("[WebPush] Migrated VAPID key from legacy DER ({} bytes) to raw scalar", priv_bytes.len());

        Ok(Self {
            private_key_b64: BASE64URL.encode(signing_key.to_bytes().as_slice()),
            public_key_b64: self.public_key_b64,
        })
    }

    /// Uncompressed public key bytes (65 bytes).
    pub fn public_key_bytes(&self) -> Result<Vec<u8>> {
        BASE64URL
            .decode(&self.public_key_b64)
            .context("Failed to decode VAPID public key")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_vapid_keys() {
        let keys = VapidKeys::generate().expect("should generate keys");

        // Public key should be 65 bytes (uncompressed P-256 point)
        let pub_bytes = keys.public_key_bytes().expect("decode public key");
        assert_eq!(pub_bytes.len(), 65, "uncompressed P-256 public key is 65 bytes");
        assert_eq!(pub_bytes[0], 0x04, "uncompressed point starts with 0x04");

        // Private key should be raw 32-byte scalar
        let priv_bytes = BASE64URL
            .decode(keys.private_key_base64url())
            .expect("decode private key");
        assert_eq!(priv_bytes.len(), 32, "raw P-256 scalar is 32 bytes");
    }

    #[test]
    fn test_from_base64url_roundtrip() {
        let keys = VapidKeys::generate().expect("should generate keys");
        let reconstructed = VapidKeys::from_base64url(
            keys.public_key_base64url(),
            keys.private_key_base64url(),
        )
        .expect("should reconstruct from base64url");

        assert_eq!(
            keys.public_key_base64url(),
            reconstructed.public_key_base64url()
        );
        assert_eq!(
            keys.private_key_base64url(),
            reconstructed.private_key_base64url(),
        );
    }

    #[test]
    fn test_vapid_key_works_with_web_push_from_base64() {
        // Verify our key format is accepted by web-push crate's from_base64
        use web_push::{SubscriptionInfo, VapidSignatureBuilder};

        let keys = VapidKeys::generate().expect("generate keys");
        let sub = SubscriptionInfo::new(
            "https://push.example.com/test",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAA",
        );
        let builder = VapidSignatureBuilder::from_base64(
            keys.private_key_base64url(),
            &sub,
        );
        assert!(builder.is_ok(), "from_base64 should accept our raw key scalar");
    }

    #[test]
    fn test_legacy_der_keys_migrate_to_raw_scalar() {
        // Reproduces: old encrypted VAPID keys on disk have SEC1 DER (109 bytes)
        // or PKCS8 DER (~138 bytes). Loading them and passing to web-push
        // from_base64 panics with assertion `left == right` (109 != 32).
        use p256::SecretKey;
        use web_push::{SubscriptionInfo, VapidSignatureBuilder};

        // Generate a key and store it as SEC1 DER (the old format)
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let public_bytes = verifying_key.to_encoded_point(false);
        let public_key_b64 = BASE64URL.encode(public_bytes.as_bytes());

        let secret_key: SecretKey = signing_key.into();
        let der = secret_key.to_sec1_der().expect("SEC1 DER");
        let old_private_b64 = BASE64URL.encode(&*der);

        // This is what was on disk — 109 bytes, not 32
        let old_keys = VapidKeys {
            private_key_b64: old_private_b64,
            public_key_b64: public_key_b64,
        };

        // Migrate should convert to 32-byte scalar
        let migrated = old_keys.migrate_if_needed().expect("migration should succeed");
        let priv_bytes = BASE64URL.decode(migrated.private_key_base64url()).unwrap();
        assert_eq!(priv_bytes.len(), 32, "migrated key should be 32 bytes");

        // And it should work with web-push
        let sub = SubscriptionInfo::new(
            "https://push.example.com/test",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAA",
        );
        let builder = VapidSignatureBuilder::from_base64(
            migrated.private_key_base64url(),
            &sub,
        );
        assert!(builder.is_ok(), "migrated key should work with from_base64");
    }

    #[test]
    fn test_from_base64url_rejects_invalid() {
        assert!(VapidKeys::from_base64url("not-valid-key", "also-bad").is_err());
    }

    #[test]
    fn test_vapid_keys_roundtrip_serde() {
        let keys = VapidKeys::generate().expect("should generate keys");
        let json = serde_json::to_string(&keys).expect("serialize");
        let loaded: VapidKeys = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(keys.public_key_base64url(), loaded.public_key_base64url());
        assert_eq!(
            keys.private_key_base64url(),
            loaded.private_key_base64url(),
        );
    }
}
