//! Browser pairing and admission authority.
//!
//! WebRTC DTLS encrypts transport bytes. This module authenticates each browser
//! inside the DataChannel before the hub accepts application traffic.

#![allow(missing_docs)]

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

pub const PAIRING_PROTOCOL_VERSION: u8 = 7;
pub const PAIRING_SECRET_TTL_SECS: u64 = 10 * 60;
const CHALLENGE_SIZE: usize = 32;
const PUBLIC_KEY_SIZE: usize = 32;
const SECRET_SIZE: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingBundle {
    pub version: u8,
    pub route: String,
    pub hub_public_key: [u8; PUBLIC_KEY_SIZE],
    pub pairing_secret: [u8; SECRET_SIZE],
    pub expires_at: u64,
}

impl PairingBundle {
    pub fn is_expired(&self) -> bool {
        self.expires_at < now_epoch_secs()
    }

    /// Binary format: version, expiry, route length, route, hub key, secret.
    pub fn to_binary(&self) -> Result<Vec<u8>> {
        let route = self.route.as_bytes();
        let route_len = u16::try_from(route.len()).context("pairing route is too long")?;
        let mut bytes = Vec::with_capacity(1 + 8 + 2 + route.len() + 32 + 32);
        bytes.push(self.version);
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes.extend_from_slice(&route_len.to_be_bytes());
        bytes.extend_from_slice(route);
        bytes.extend_from_slice(&self.hub_public_key);
        bytes.extend_from_slice(&self.pairing_secret);
        Ok(bytes)
    }

    pub fn from_binary(bytes: &[u8]) -> Result<Self> {
        anyhow::ensure!(bytes.len() >= 75, "pairing bundle is too short");
        let version = bytes[0];
        anyhow::ensure!(
            version == PAIRING_PROTOCOL_VERSION,
            "unsupported pairing protocol version {version}"
        );
        let expires_at = u64::from_be_bytes(bytes[1..9].try_into().expect("fixed expiry slice"));
        let route_len =
            u16::from_be_bytes(bytes[9..11].try_into().expect("fixed route slice")) as usize;
        let expected = 11 + route_len + PUBLIC_KEY_SIZE + SECRET_SIZE;
        anyhow::ensure!(bytes.len() == expected, "invalid pairing bundle size");
        let route = std::str::from_utf8(&bytes[11..11 + route_len])
            .context("pairing route is not UTF-8")?
            .to_string();
        anyhow::ensure!(route.starts_with("/hubs/"), "invalid pairing route");
        let key_start = 11 + route_len;
        let hub_public_key = bytes[key_start..key_start + PUBLIC_KEY_SIZE]
            .try_into()
            .expect("fixed hub key slice");
        let pairing_secret = bytes[key_start + PUBLIC_KEY_SIZE..expected]
            .try_into()
            .expect("fixed secret slice");
        Ok(Self {
            version,
            route,
            hub_public_key,
            pairing_secret,
            expires_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserCredential {
    pub id: String,
    pub public_key: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
}

impl BrowserCredential {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

#[derive(Debug, Clone)]
struct PairingSecret {
    value: [u8; SECRET_SIZE],
    route: String,
    expires_at: u64,
}

#[derive(Debug, Clone)]
struct PendingChallenge {
    value: String,
    route: String,
    dtls_binding: DtlsBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtlsBinding {
    pub browser_fingerprint: String,
    pub hub_fingerprint: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserAuthRequest {
    #[serde(rename = "pair_request")]
    Pair {
        protocol: u8,
        challenge: String,
        browser_public_key: String,
        expires_at: u64,
        proof: String,
    },
    #[serde(rename = "auth_response")]
    Authenticate {
        protocol: u8,
        challenge: String,
        browser_public_key: String,
        signature: String,
    },
}

#[derive(Debug, Serialize)]
pub struct AuthChallenge {
    #[serde(rename = "type")]
    kind: &'static str,
    pub protocol: u8,
    pub challenge: String,
    pub route: String,
    pub hub_public_key: String,
    pub browser_fingerprint: String,
    pub hub_fingerprint: String,
}

#[derive(Debug, Serialize)]
pub struct AuthAccepted {
    #[serde(rename = "type")]
    kind: &'static str,
    pub protocol: u8,
    pub challenge: String,
    pub credential_id: String,
    pub browser_public_key: String,
    pub hub_public_key: String,
    pub signature: String,
    pub paired: bool,
}

#[derive(Debug)]
pub struct BrowserAdmission {
    pub credential_id: String,
    pub response: AuthAccepted,
}

pub struct PairingAuthority {
    signing_key: SigningKey,
    route: String,
    pairing_secret: Option<PairingSecret>,
    challenges: HashMap<String, PendingChallenge>,
    credentials: HashMap<String, BrowserCredential>,
    credentials_path: Option<PathBuf>,
}

impl std::fmt::Debug for PairingAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingAuthority")
            .field("route", &self.route)
            .field("pairing_secret_active", &self.pairing_secret.is_some())
            .field("credential_count", &self.credentials.len())
            .finish_non_exhaustive()
    }
}

impl PairingAuthority {
    pub fn load(signing_key: SigningKey, hub_id: &str, route: String) -> Result<Self> {
        let path = crate::device::Device::config_dir()?
            .join("hubs")
            .join(hub_id)
            .join("browser_credentials.json");
        let credentials = load_credentials(&path)?;
        Ok(Self {
            signing_key,
            route,
            pairing_secret: None,
            challenges: HashMap::new(),
            credentials,
            credentials_path: Some(path),
        })
    }

    #[cfg(test)]
    pub fn in_memory(signing_key: SigningKey, route: String) -> Self {
        Self {
            signing_key,
            route,
            pairing_secret: None,
            challenges: HashMap::new(),
            credentials: HashMap::new(),
            credentials_path: None,
        }
    }

    pub fn hub_public_key(&self) -> String {
        encode(self.signing_key.verifying_key().as_bytes())
    }

    pub fn create_pairing_bundle(&mut self) -> PairingBundle {
        let mut secret = [0u8; SECRET_SIZE];
        rand::rng().fill_bytes(&mut secret);
        let expires_at = now_epoch_secs().saturating_add(PAIRING_SECRET_TTL_SECS);
        self.pairing_secret = Some(PairingSecret {
            value: secret,
            route: self.route.clone(),
            expires_at,
        });
        PairingBundle {
            version: PAIRING_PROTOCOL_VERSION,
            route: self.route.clone(),
            hub_public_key: self.signing_key.verifying_key().to_bytes(),
            pairing_secret: secret,
            expires_at,
        }
    }

    pub fn begin_challenge(&mut self, peer_id: &str, dtls_binding: DtlsBinding) -> AuthChallenge {
        let mut challenge = [0u8; CHALLENGE_SIZE];
        rand::rng().fill_bytes(&mut challenge);
        let challenge = encode(&challenge);
        self.challenges.insert(
            peer_id.to_string(),
            PendingChallenge {
                value: challenge.clone(),
                route: self.route.clone(),
                dtls_binding: dtls_binding.clone(),
            },
        );
        AuthChallenge {
            kind: "auth_challenge",
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge,
            route: self.route.clone(),
            hub_public_key: self.hub_public_key(),
            browser_fingerprint: dtls_binding.browser_fingerprint,
            hub_fingerprint: dtls_binding.hub_fingerprint,
        }
    }

    pub fn verify(
        &mut self,
        peer_id: &str,
        request: BrowserAuthRequest,
    ) -> Result<BrowserAdmission> {
        let pending = self
            .challenges
            .remove(peer_id)
            .context("missing or replayed authentication challenge")?;

        let (challenge, browser_public_key, paired) = match request {
            BrowserAuthRequest::Pair {
                protocol,
                challenge,
                browser_public_key,
                expires_at,
                proof,
            } => {
                validate_protocol(protocol)?;
                anyhow::ensure!(
                    challenge == pending.value,
                    "authentication challenge changed"
                );
                validate_claimed_identity(peer_id, &browser_public_key)?;
                let secret = self
                    .pairing_secret
                    .as_ref()
                    .cloned()
                    .context("pairing secret is missing or already consumed")?;
                anyhow::ensure!(secret.route == pending.route, "pairing route changed");
                let now = now_epoch_secs();
                anyhow::ensure!(secret.expires_at >= now, "pairing secret expired");
                anyhow::ensure!(expires_at == secret.expires_at, "pairing expiry changed");
                let transcript = pairing_transcript(
                    &pending.route,
                    &self.hub_public_key(),
                    &challenge,
                    &browser_public_key,
                    expires_at,
                    &pending.dtls_binding,
                );
                verify_hmac(&secret.value, transcript.as_bytes(), &proof)?;
                self.store_credential(&browser_public_key)?;
                self.pairing_secret = None;
                (challenge, browser_public_key, true)
            }
            BrowserAuthRequest::Authenticate {
                protocol,
                challenge,
                browser_public_key,
                signature,
            } => {
                validate_protocol(protocol)?;
                anyhow::ensure!(
                    challenge == pending.value,
                    "authentication challenge changed"
                );
                validate_claimed_identity(peer_id, &browser_public_key)?;
                let credential_id = credential_id(&browser_public_key)?;
                let credential = self
                    .credentials
                    .get(&credential_id)
                    .filter(|credential| credential.active())
                    .context("browser credential is missing or revoked")?;
                anyhow::ensure!(
                    credential.public_key == browser_public_key,
                    "browser credential key changed"
                );
                let transcript = authentication_transcript(
                    &pending.route,
                    &self.hub_public_key(),
                    &challenge,
                    &browser_public_key,
                    &pending.dtls_binding,
                );
                verify_browser_signature(&browser_public_key, transcript.as_bytes(), &signature)?;
                (challenge, browser_public_key, false)
            }
        };

        let credential_id = credential_id(&browser_public_key)?;
        let response_transcript = accepted_transcript(
            &pending.route,
            &challenge,
            &browser_public_key,
            &credential_id,
            paired,
            &pending.dtls_binding,
        );
        let signature = self.signing_key.sign(response_transcript.as_bytes());
        Ok(BrowserAdmission {
            credential_id: credential_id.clone(),
            response: AuthAccepted {
                kind: "auth_accepted",
                protocol: PAIRING_PROTOCOL_VERSION,
                challenge,
                credential_id,
                browser_public_key,
                hub_public_key: self.hub_public_key(),
                signature: encode(&signature.to_bytes()),
                paired,
            },
        })
    }

    pub fn revoke(&mut self, credential_id: &str) -> Result<bool> {
        let Some(credential) = self.credentials.get_mut(credential_id) else {
            return Ok(false);
        };
        if credential.revoked_at.is_none() {
            credential.revoked_at = Some(now_epoch_secs());
            self.persist_credentials()?;
        }
        Ok(true)
    }

    pub fn remove_peer(&mut self, peer_id: &str) {
        self.challenges.remove(peer_id);
    }

    fn store_credential(&mut self, public_key: &str) -> Result<()> {
        let id = credential_id(public_key)?;
        if let Some(existing) = self.credentials.get(&id) {
            anyhow::ensure!(
                existing.public_key == public_key,
                "browser credential key changed"
            );
            anyhow::ensure!(existing.active(), "browser credential is revoked");
            return Ok(());
        }
        self.credentials.insert(
            id.clone(),
            BrowserCredential {
                id,
                public_key: public_key.to_string(),
                created_at: now_epoch_secs(),
                revoked_at: None,
            },
        );
        self.persist_credentials()
    }

    fn persist_credentials(&self) -> Result<()> {
        let Some(path) = self.credentials_path.as_deref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("cannot create browser credential directory")?;
        }
        let mut records = self.credentials.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| left.id.cmp(&right.id));
        let json = serde_json::to_string_pretty(&records)?;
        fs::write(path, json).context("cannot write browser credentials")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .context("cannot protect browser credentials")?;
        }
        Ok(())
    }
}

pub fn pairing_transcript(
    route: &str,
    hub_public_key: &str,
    challenge: &str,
    browser_public_key: &str,
    expires_at: u64,
    dtls_binding: &DtlsBinding,
) -> String {
    format!(
        "botster-pairing-v1\n{route}\n{hub_public_key}\n{challenge}\n{browser_public_key}\n{expires_at}\n{}\n{}",
        dtls_binding.browser_fingerprint, dtls_binding.hub_fingerprint
    )
}

pub fn authentication_transcript(
    route: &str,
    hub_public_key: &str,
    challenge: &str,
    browser_public_key: &str,
    dtls_binding: &DtlsBinding,
) -> String {
    format!(
        "botster-auth-v1\n{route}\n{hub_public_key}\n{challenge}\n{browser_public_key}\n{}\n{}",
        dtls_binding.browser_fingerprint, dtls_binding.hub_fingerprint
    )
}

pub fn accepted_transcript(
    route: &str,
    challenge: &str,
    browser_public_key: &str,
    credential_id: &str,
    paired: bool,
    dtls_binding: &DtlsBinding,
) -> String {
    format!(
        "botster-auth-accepted-v1\n{route}\n{challenge}\n{browser_public_key}\n{credential_id}\n{}\n{}\n{}",
        if paired { "pair" } else { "auth" },
        dtls_binding.browser_fingerprint,
        dtls_binding.hub_fingerprint
    )
}

fn credential_id(public_key: &str) -> Result<String> {
    let bytes = decode_32(public_key, "browser public key")?;
    let digest = Sha256::digest(bytes);
    Ok(digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_claimed_identity(peer_id: &str, public_key: &str) -> Result<()> {
    let claimed_id = peer_id.split(':').next().unwrap_or(peer_id);
    anyhow::ensure!(
        claimed_id == credential_id(public_key)?,
        "browser credential identity changed"
    );
    Ok(())
}

fn verify_hmac(secret: &[u8], message: &[u8], proof: &str) -> Result<()> {
    let proof = URL_SAFE_NO_PAD
        .decode(proof)
        .context("invalid pairing proof encoding")?;
    let mut mac = HmacSha256::new_from_slice(secret).context("invalid pairing secret")?;
    mac.update(message);
    mac.verify_slice(&proof).context("invalid pairing proof")
}

fn verify_browser_signature(public_key: &str, message: &[u8], signature: &str) -> Result<()> {
    let key = VerifyingKey::from_bytes(&decode_32(public_key, "browser public key")?)
        .context("invalid browser public key")?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature)
        .context("invalid browser signature encoding")?;
    let signature = Signature::from_slice(&signature_bytes).context("invalid browser signature")?;
    key.verify(message, &signature)
        .context("invalid browser signature")
}

fn validate_protocol(protocol: u8) -> Result<()> {
    anyhow::ensure!(
        protocol == PAIRING_PROTOCOL_VERSION,
        "unsupported browser protocol version {protocol}"
    );
    Ok(())
}

fn decode_32(value: &str, label: &str) -> Result<[u8; 32]> {
    URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("invalid {label} encoding"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid {label} length"))
}

fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_credentials(path: &Path) -> Result<HashMap<String, BrowserCredential>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let json = fs::read_to_string(path).context("cannot read browser credentials")?;
    let records: Vec<BrowserCredential> =
        serde_json::from_str(&json).context("cannot parse browser credentials")?;
    Ok(records
        .into_iter()
        .map(|credential| (credential.id.clone(), credential))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> PairingAuthority {
        PairingAuthority::in_memory(SigningKey::from_bytes(&[7u8; 32]), "/hubs/42".into())
    }

    fn peer_id(public_key: &str) -> String {
        format!("{}:tab", credential_id(public_key).unwrap())
    }

    fn binding() -> DtlsBinding {
        DtlsBinding {
            browser_fingerprint: "sha-256 browser".into(),
            hub_fingerprint: "sha-256 hub".into(),
        }
    }

    fn begin(authority: &mut PairingAuthority, peer: &str) -> AuthChallenge {
        authority.begin_challenge(peer, binding())
    }

    #[test]
    fn pairing_bundle_round_trips_route_key_secret_and_expiry() {
        let mut authority = authority();
        let bundle = authority.create_pairing_bundle();
        assert_eq!(
            PairingBundle::from_binary(&bundle.to_binary().unwrap()).unwrap(),
            bundle
        );
    }

    #[test]
    fn rust_transcripts_match_the_shared_browser_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../test/fixtures/browser_pairing_transcripts.json"
        ))
        .unwrap();
        let value = |key: &str| fixture[key].as_str().unwrap();
        let dtls_binding = DtlsBinding {
            browser_fingerprint: value("browser_fingerprint").to_string(),
            hub_fingerprint: value("hub_fingerprint").to_string(),
        };

        assert_eq!(
            pairing_transcript(
                value("route"),
                value("hub_public_key"),
                value("challenge"),
                value("browser_public_key"),
                fixture["expires_at"].as_u64().unwrap(),
                &dtls_binding,
            ),
            value("pairing_transcript")
        );
        assert_eq!(
            authentication_transcript(
                value("route"),
                value("hub_public_key"),
                value("challenge"),
                value("browser_public_key"),
                &dtls_binding,
            ),
            value("authentication_transcript")
        );
        assert_eq!(
            accepted_transcript(
                value("route"),
                value("challenge"),
                value("browser_public_key"),
                value("credential_id"),
                fixture["paired"].as_bool().unwrap(),
                &dtls_binding,
            ),
            value("accepted_transcript")
        );
    }

    #[test]
    fn pairing_secret_is_consumed_once() {
        let mut authority = authority();
        let bundle = authority.create_pairing_bundle();
        let browser_key = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        let browser_public_key = encode(browser_key.as_bytes());
        let peer = peer_id(&browser_public_key);
        let challenge = begin(&mut authority, &peer);
        let transcript = pairing_transcript(
            &bundle.route,
            &encode(&bundle.hub_public_key),
            &challenge.challenge,
            &browser_public_key,
            bundle.expires_at,
            &binding(),
        );
        let mut mac = HmacSha256::new_from_slice(&bundle.pairing_secret).unwrap();
        mac.update(transcript.as_bytes());
        let request = BrowserAuthRequest::Pair {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key: browser_public_key.clone(),
            expires_at: bundle.expires_at,
            proof: encode(&mac.finalize().into_bytes()),
        };
        let admission = authority.verify(&peer, request).unwrap();
        let accepted = accepted_transcript(
            &bundle.route,
            &admission.response.challenge,
            &admission.response.browser_public_key,
            &admission.response.credential_id,
            true,
            &binding(),
        );
        let signature = Signature::from_slice(
            &URL_SAFE_NO_PAD
                .decode(&admission.response.signature)
                .unwrap(),
        )
        .unwrap();
        authority
            .signing_key
            .verifying_key()
            .verify(accepted.as_bytes(), &signature)
            .unwrap();

        let challenge = begin(&mut authority, "other:tab");
        let request = BrowserAuthRequest::Pair {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key,
            expires_at: bundle.expires_at,
            proof: "invalid".into(),
        };
        assert!(authority.verify("other:tab", request).is_err());
    }

    #[test]
    fn invalid_proof_does_not_consume_pairing_secret() {
        let mut authority = authority();
        let bundle = authority.create_pairing_bundle();
        let browser_key = SigningKey::from_bytes(&[15u8; 32]).verifying_key();
        let browser_public_key = encode(browser_key.as_bytes());
        let peer = peer_id(&browser_public_key);
        let challenge = begin(&mut authority, &peer);
        let invalid = BrowserAuthRequest::Pair {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key: browser_public_key.clone(),
            expires_at: bundle.expires_at,
            proof: encode(&[0; 32]),
        };
        assert!(authority.verify(&peer, invalid).is_err());
        assert!(authority.pairing_secret.is_some());
    }

    #[test]
    fn changed_dtls_fingerprint_is_rejected() {
        let mut authority = authority();
        let bundle = authority.create_pairing_bundle();
        let browser_key = SigningKey::from_bytes(&[16u8; 32]).verifying_key();
        let browser_public_key = encode(browser_key.as_bytes());
        let peer = peer_id(&browser_public_key);
        let challenge = begin(&mut authority, &peer);
        let changed_binding = DtlsBinding {
            browser_fingerprint: "sha-256 attacker".into(),
            hub_fingerprint: "sha-256 hub".into(),
        };
        let transcript = pairing_transcript(
            &bundle.route,
            &authority.hub_public_key(),
            &challenge.challenge,
            &browser_public_key,
            bundle.expires_at,
            &changed_binding,
        );
        let mut mac = HmacSha256::new_from_slice(&bundle.pairing_secret).unwrap();
        mac.update(transcript.as_bytes());
        let request = BrowserAuthRequest::Pair {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key,
            expires_at: bundle.expires_at,
            proof: encode(&mac.finalize().into_bytes()),
        };

        assert!(authority.verify(&peer, request).is_err());
        assert!(authority.pairing_secret.is_some());
    }

    #[test]
    fn expired_pairing_secret_is_rejected() {
        let mut authority = authority();
        let mut bundle = authority.create_pairing_bundle();
        bundle.expires_at = now_epoch_secs().saturating_sub(1);
        authority.pairing_secret.as_mut().unwrap().expires_at = bundle.expires_at;
        let browser_key = SigningKey::from_bytes(&[17u8; 32]).verifying_key();
        let browser_public_key = encode(browser_key.as_bytes());
        let peer = peer_id(&browser_public_key);
        let challenge = begin(&mut authority, &peer);
        let transcript = pairing_transcript(
            &bundle.route,
            &authority.hub_public_key(),
            &challenge.challenge,
            &browser_public_key,
            bundle.expires_at,
            &binding(),
        );
        let mut mac = HmacSha256::new_from_slice(&bundle.pairing_secret).unwrap();
        mac.update(transcript.as_bytes());
        let request = BrowserAuthRequest::Pair {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key,
            expires_at: bundle.expires_at,
            proof: encode(&mac.finalize().into_bytes()),
        };
        assert!(authority.verify(&peer, request).is_err());
        assert!(authority.pairing_secret.is_some());
    }

    #[test]
    fn authentication_challenge_is_single_use() {
        let mut authority = authority();
        let browser_signing = SigningKey::from_bytes(&[19u8; 32]);
        let browser_public_key = encode(browser_signing.verifying_key().as_bytes());
        authority.store_credential(&browser_public_key).unwrap();
        let peer = peer_id(&browser_public_key);
        let challenge = begin(&mut authority, &peer);
        let transcript = authentication_transcript(
            "/hubs/42",
            &authority.hub_public_key(),
            &challenge.challenge,
            &browser_public_key,
            &binding(),
        );
        let signature = encode(&browser_signing.sign(transcript.as_bytes()).to_bytes());
        let request = || BrowserAuthRequest::Authenticate {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge.clone(),
            browser_public_key: browser_public_key.clone(),
            signature: signature.clone(),
        };
        assert!(authority.verify(&peer, request()).is_ok());
        assert!(authority.verify(&peer, request()).is_err());
    }

    #[test]
    fn revoked_browser_cannot_authenticate() {
        let mut authority = authority();
        let browser_signing = SigningKey::from_bytes(&[11u8; 32]);
        let browser_public_key = encode(browser_signing.verifying_key().as_bytes());
        authority.store_credential(&browser_public_key).unwrap();
        let id = credential_id(&browser_public_key).unwrap();
        authority.revoke(&id).unwrap();
        let peer = peer_id(&browser_public_key);
        let challenge = begin(&mut authority, &peer);
        let transcript = authentication_transcript(
            "/hubs/42",
            &authority.hub_public_key(),
            &challenge.challenge,
            &browser_public_key,
            &binding(),
        );
        let request = BrowserAuthRequest::Authenticate {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key,
            signature: encode(&browser_signing.sign(transcript.as_bytes()).to_bytes()),
        };
        assert!(authority.verify(&peer, request).is_err());
    }

    #[test]
    fn changed_signaling_identity_is_rejected() {
        let mut authority = authority();
        let browser_signing = SigningKey::from_bytes(&[13u8; 32]);
        let browser_public_key = encode(browser_signing.verifying_key().as_bytes());
        authority.store_credential(&browser_public_key).unwrap();
        let challenge = begin(&mut authority, "changed:tab");
        let transcript = authentication_transcript(
            "/hubs/42",
            &authority.hub_public_key(),
            &challenge.challenge,
            &browser_public_key,
            &binding(),
        );
        let request = BrowserAuthRequest::Authenticate {
            protocol: PAIRING_PROTOCOL_VERSION,
            challenge: challenge.challenge,
            browser_public_key,
            signature: encode(&browser_signing.sign(transcript.as_bytes()).to_bytes()),
        };
        assert!(authority.verify("changed:tab", request).is_err());
    }
}
