//! Web push message sending and subscription management.
//!
//! Stores browser push subscriptions and sends encrypted web push
//! messages (RFC 8030) using VAPID authentication (RFC 8292).

// Rust guideline compliant 2026-02

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A browser's push subscription, received via DataChannel.
///
/// Contains everything CLI needs to send a web push message to this browser.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushSubscription {
    /// Push service endpoint URL.
    pub endpoint: String,
    /// Browser's P-256 ECDH public key (base64url).
    pub p256dh: String,
    /// Shared auth secret (base64url).
    pub auth: String,
}

/// Stores push subscriptions per browser identity.
///
/// Subscriptions are kept in memory and also persisted to disk so they
/// survive CLI restarts. When a push service returns 410 Gone, the
/// subscription is removed.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PushSubscriptionStore {
    /// Maps browser identity key → push subscription.
    subscriptions: HashMap<String, PushSubscription>,
}

impl PushSubscriptionStore {
    /// Add or update a push subscription for a browser.
    ///
    /// Deduplicates by endpoint: if another identity already holds a subscription
    /// with the same push endpoint, the old entry is removed first. This prevents
    /// duplicate notifications when a browser reconnects with a new identity key
    /// but reuses the same push subscription.
    pub fn upsert(&mut self, browser_identity: String, subscription: PushSubscription) {
        // Remove any existing subscription with the same endpoint under a different key
        let stale_key = self
            .subscriptions
            .iter()
            .find(|(k, v)| *k != &browser_identity && v.endpoint == subscription.endpoint)
            .map(|(k, _)| k.clone());

        if let Some(key) = stale_key {
            log::info!(
                "[WebPush] Replacing stale subscription for {} (same endpoint, new identity {})",
                &key[..key.len().min(8)],
                &browser_identity[..browser_identity.len().min(8)]
            );
            self.subscriptions.remove(&key);
        }

        self.subscriptions.insert(browser_identity, subscription);
    }

    /// Remove a push subscription for a browser.
    pub fn remove(&mut self, browser_identity: &str) {
        self.subscriptions.remove(browser_identity);
    }

    /// Get all active subscriptions.
    pub fn all(&self) -> impl Iterator<Item = (&str, &PushSubscription)> {
        self.subscriptions.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of stored subscriptions.
    pub fn len(&self) -> usize {
        self.subscriptions.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    /// Check if a subscription exists for a given browser identity.
    pub fn contains(&self, browser_identity: &str) -> bool {
        self.subscriptions.contains_key(browser_identity)
    }

    /// Remove duplicate subscriptions that share the same push endpoint.
    ///
    /// When a browser reconnects with a new identity key but the same push
    /// subscription, old entries accumulate. This keeps only the most recent
    /// entry (last in iteration order) for each unique endpoint.
    /// Returns the number of duplicates removed.
    pub fn dedup_by_endpoint(&mut self) -> usize {
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut to_remove = Vec::new();

        for (identity, sub) in &self.subscriptions {
            if let Some(prev_identity) = seen.insert(sub.endpoint.clone(), identity.clone()) {
                // Keep the newer one (current), mark the older one for removal
                to_remove.push(prev_identity);
            }
        }

        for key in &to_remove {
            self.subscriptions.remove(key);
        }

        to_remove.len()
    }
}

/// Send a declarative web push notification using VAPID authentication.
///
/// Uses the `web-push` crate for RFC 8291 payload encryption and VAPID signing,
/// then sends the HTTP request via reqwest with `Content-Type: application/notification+json`
/// for Safari 18.4+ Declarative Web Push support.
///
/// The caller should reuse a single `reqwest::Client` across multiple calls
/// for connection pooling.
///
/// Returns `Ok(true)` on success, `Ok(false)` if the subscription is stale (410 Gone).
pub async fn send_push_direct(
    client: &reqwest::Client,
    vapid_private_b64: &str,
    subscription: &PushSubscription,
    payload: &[u8],
) -> Result<bool> {
    use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessageBuilder};

    let sub_info =
        SubscriptionInfo::new(&subscription.endpoint, &subscription.p256dh, &subscription.auth);

    let mut sig_builder = VapidSignatureBuilder::from_base64(vapid_private_b64, &sub_info)
        .context("Failed to build VAPID signature")?;
    sig_builder.add_claim("sub", "https://trybotster.com");
    let sig = sig_builder
        .build()
        .context("Failed to sign VAPID JWT")?;

    let mut builder = WebPushMessageBuilder::new(&sub_info);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload);
    builder.set_vapid_signature(sig);
    builder.set_ttl(86400); // 24 hours

    let message = builder.build().context("Failed to build web push message")?;

    // Build the HTTP request manually to set Content-Type: application/notification+json
    // (the web-push crate hardcodes application/octet-stream).
    let mut request = client
        .post(message.endpoint.to_string())
        .header("TTL", message.ttl.to_string());

    if let Some(urgency) = message.urgency {
        request = request.header("Urgency", urgency.to_string());
    }

    if let Some(topic) = message.topic {
        request = request.header("Topic", topic);
    }

    if let Some(push_payload) = message.payload {
        request = request
            .header("Content-Encoding", push_payload.content_encoding.to_str())
            .header("Content-Type", "application/notification+json");

        for (key, value) in &push_payload.crypto_headers {
            request = request.header(*key, value.as_str());
        }

        request = request.body(push_payload.content);
    }

    let response = request.send().await.context("Web push HTTP request failed")?;
    let status = response.status().as_u16();

    match status {
        200..=299 => Ok(true),
        410 => {
            log::info!("[WebPush] Subscription expired (410 Gone)");
            Ok(false)
        }
        429 => {
            log::warn!("[WebPush] Rate limited (429)");
            Ok(true) // Don't remove subscription
        }
        _ => {
            let body = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!("Web push send failed (HTTP {status}): {body}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_subscription_store() {
        let mut store = PushSubscriptionStore::default();
        assert!(store.is_empty());

        store.upsert(
            "browser_1".to_string(),
            PushSubscription {
                endpoint: "https://push.example.com/1".to_string(),
                p256dh: "key1".to_string(),
                auth: "auth1".to_string(),
            },
        );
        assert_eq!(store.len(), 1);

        // Upsert replaces
        store.upsert(
            "browser_1".to_string(),
            PushSubscription {
                endpoint: "https://push.example.com/2".to_string(),
                p256dh: "key2".to_string(),
                auth: "auth2".to_string(),
            },
        );
        assert_eq!(store.len(), 1);

        store.remove("browser_1");
        assert!(store.is_empty());
    }

    #[test]
    fn test_push_subscription_store_serde() {
        let mut store = PushSubscriptionStore::default();
        store.upsert(
            "browser_1".to_string(),
            PushSubscription {
                endpoint: "https://push.example.com/1".to_string(),
                p256dh: "key1".to_string(),
                auth: "auth1".to_string(),
            },
        );

        let json = serde_json::to_string(&store).expect("serialize");
        let loaded: PushSubscriptionStore = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(loaded.len(), 1);
    }
}
