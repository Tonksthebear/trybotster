use super::*;

impl Hub {
    pub(super) fn handle_push_subscriptions_expired(&mut self, identities: Vec<String>) {
        for identity in &identities {
            self.push_subscriptions.remove(identity);
            log::info!(
                "[WebPush] Removed stale subscription for {}",
                &identity[..identity.len().min(8)]
            );
        }
        if !identities.is_empty() {
            if let Err(e) =
                crate::relay::persistence::save_push_subscriptions(&self.push_subscriptions)
            {
                log::error!("[WebPush] Failed to save push subscriptions after cleanup: {e}");
            }
        }
    }

    pub(super) fn send_vapid_public_key(&mut self, browser_identity: &str) {
        let Some(ref vapid) = self.vapid_keys else {
            return;
        };

        let msg = serde_json::json!({
            "type": "vapid_pub",
            "key": vapid.public_key_base64url(),
        });

        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize vapid_pub: {e}");
                return;
            }
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
        log::info!(
            "[WebPush] Queued VAPID public key for {}",
            &browser_identity[..browser_identity.len().min(8)]
        );
    }

    pub(super) fn handle_browser_push_control(
        &mut self,
        browser_identity: &str,
        msg: &serde_json::Value,
    ) {
        let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) else {
            log::warn!("[WebPush] Browser push control missing type");
            return;
        };

        match msg_type {
            "push_sub" => self.handle_push_subscription(browser_identity, msg),
            "vapid_generate" => self.handle_vapid_generate(browser_identity),
            "vapid_key_req" => self.handle_vapid_key_request(browser_identity),
            "vapid_key_set" => self.handle_vapid_key_set(browser_identity, msg),
            "vapid_pub_req" => self.handle_vapid_pub_request(browser_identity),
            "push_test" => self.handle_push_test(browser_identity),
            "push_disable" => self.handle_push_disable(browser_identity),
            "push_status_req" => self.handle_push_status_request(browser_identity, msg),
            other => log::warn!("[WebPush] Unknown browser push control: {other}"),
        }
    }

    pub(super) fn handle_push_subscription(
        &mut self,
        browser_identity: &str,
        msg: &serde_json::Value,
    ) {
        let endpoint = msg.get("endpoint").and_then(|v| v.as_str()).unwrap_or("");
        let p256dh = msg.get("p256dh").and_then(|v| v.as_str()).unwrap_or("");
        let auth = msg.get("auth").and_then(|v| v.as_str()).unwrap_or("");

        if endpoint.is_empty() || p256dh.is_empty() || auth.is_empty() {
            log::warn!("[WebPush] Received incomplete push subscription");
            return;
        }

        // Validate endpoint is HTTPS to prevent SSRF
        if !endpoint.starts_with("https://") {
            log::warn!("[WebPush] Rejected push endpoint with non-HTTPS scheme");
            return;
        }

        // Use stable browser_id when available, fall back to ephemeral identity
        let storage_key = msg
            .get("browser_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(browser_identity)
            .to_string();

        let subscription = crate::notifications::push::PushSubscription {
            endpoint: endpoint.to_string(),
            p256dh: p256dh.to_string(),
            auth: auth.to_string(),
        };

        self.push_subscriptions
            .upsert(storage_key.clone(), subscription);

        // Persist to encrypted storage
        if let Err(e) = crate::relay::persistence::save_push_subscriptions(&self.push_subscriptions)
        {
            log::error!("[WebPush] Failed to save push subscriptions: {e}");
        }

        log::info!(
            "[WebPush] Stored push subscription for {} ({} total)",
            &storage_key[..storage_key.len().min(8)],
            self.push_subscriptions.len()
        );

        // Send acknowledgment
        self.send_push_sub_ack(browser_identity);
    }

    pub(super) fn handle_vapid_generate(&mut self, browser_identity: &str) {
        // Load existing or generate fresh keys
        let keys = match crate::relay::persistence::load_vapid_keys() {
            Ok(Some(existing)) => existing,
            Ok(None) => match crate::notifications::vapid::VapidKeys::generate() {
                Ok(new_keys) => {
                    if let Err(e) = crate::relay::persistence::save_vapid_keys(&new_keys) {
                        log::error!("[WebPush] Failed to save generated VAPID keys: {e}");
                        return;
                    }
                    log::info!("[WebPush] Generated and saved new device-level VAPID keys");
                    new_keys
                }
                Err(e) => {
                    log::error!("[WebPush] Failed to generate VAPID keys: {e}");
                    return;
                }
            },
            Err(e) => {
                log::error!("[WebPush] Failed to load VAPID keys: {e}");
                return;
            }
        };

        self.vapid_keys = Some(keys);
        self.set_notifications_enabled(true);
        self.send_vapid_public_key(browser_identity);
    }

    pub(super) fn handle_vapid_key_set(&mut self, browser_identity: &str, msg: &serde_json::Value) {
        let pub_key = match msg.get("pub").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                log::warn!("[WebPush] vapid_key_set missing 'pub' field");
                return;
            }
        };
        let priv_key = match msg.get("priv").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                log::warn!("[WebPush] vapid_key_set missing 'priv' field");
                return;
            }
        };

        let keys = match crate::notifications::vapid::VapidKeys::from_base64url(pub_key, priv_key) {
            Ok(k) => k,
            Err(e) => {
                log::error!("[WebPush] Invalid VAPID keys in vapid_key_set: {e}");
                return;
            }
        };

        if let Err(e) = crate::relay::persistence::save_vapid_keys(&keys) {
            log::error!("[WebPush] Failed to save copied VAPID keys: {e}");
            return;
        }

        log::info!("[WebPush] Stored copied VAPID keys from another device");
        self.vapid_keys = Some(keys);
        self.set_notifications_enabled(true);
        self.send_vapid_public_key(browser_identity);
    }

    pub(super) fn handle_vapid_pub_request(&mut self, browser_identity: &str) {
        // Ensure keys are loaded into memory
        if self.vapid_keys.is_none() {
            match crate::relay::persistence::load_vapid_keys() {
                Ok(Some(keys)) => self.vapid_keys = Some(keys),
                Ok(None) => {
                    log::warn!("[WebPush] vapid_pub_req but no VAPID keys exist");
                    return;
                }
                Err(e) => {
                    log::error!("[WebPush] Failed to load VAPID keys for pub_req: {e}");
                    return;
                }
            }
        }

        self.send_vapid_public_key(browser_identity);
    }

    pub(super) fn handle_vapid_key_request(&mut self, browser_identity: &str) {
        let Some(ref vapid) = self.vapid_keys else {
            log::warn!("[WebPush] VAPID key requested but no keys loaded");
            return;
        };

        // Send full keypair (private + public) for multi-device VAPID key copying.
        // This is safe because the DataChannel is E2E encrypted via Olm.
        let msg = serde_json::json!({
            "type": "vapid_keys",
            "pub": vapid.public_key_base64url(),
            "priv": vapid.private_key_base64url(),
        });

        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize vapid_keys: {e}");
                return;
            }
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
        log::info!("[WebPush] Queued VAPID keypair for browser copy");
    }

    pub(super) fn send_push_sub_ack(&self, browser_identity: &str) {
        let msg = serde_json::json!({ "type": "push_sub_ack" });
        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_sub_ack: {e}");
                return;
            }
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
    }

    pub(super) fn handle_push_test(&mut self, browser_identity: &str) {
        let Some(ref vapid) = self.vapid_keys else {
            log::warn!("[WebPush] Cannot send test push: no VAPID keys");
            return;
        };
        if self.push_subscriptions.is_empty() {
            log::warn!("[WebPush] Cannot send test push: no subscriptions");
            return;
        }

        let Some(ref hub_id) = self.botster_id else {
            log::warn!("[WebPush] Cannot send test push: no server hub ID");
            return;
        };

        let base_url = self.config.server_url.trim_end_matches('/');
        let navigate_url = format!("{base_url}/hubs/{hub_id}");

        let payload = serde_json::json!({
            "web_push": 8030,
            "notification": {
                "title": "Botster",
                "body": "Test notification — push is working!",
                "icon": format!("{base_url}/icon.png"),
                "navigate": navigate_url,
                "data": {
                    "id": uuid::Uuid::new_v4().to_string(),
                    "kind": "test",
                    "hubId": hub_id,
                    "url": format!("/hubs/{hub_id}"),
                    "createdAt": chrono::Utc::now().to_rfc3339(),
                }
            }
        });
        let payload_bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                log::error!("[WebPush] Failed to serialize test payload: {e}");
                return;
            }
        };

        let vapid_b64 = vapid.private_key_base64url().to_string();

        let subs: Vec<(String, crate::notifications::push::PushSubscription)> = self
            .push_subscriptions
            .all()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

        // Ack immediately — the push notification arriving is the real confirmation
        self.send_push_test_ack(browser_identity, subs.len());

        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.handle().spawn(async move {
            let client = reqwest::Client::new();
            let mut stale = Vec::new();
            let mut sent = 0usize;

            for (identity, sub) in &subs {
                match send_push_direct(&client, &vapid_b64, sub, &payload_bytes).await {
                    Ok(true) => sent += 1,
                    Ok(false) => stale.push(identity.clone()),
                    Err(e) => {
                        log::error!(
                            "[WebPush] Test push failed for {}: {e}",
                            &identity[..identity.len().min(8)]
                        );
                    }
                }
            }

            log::info!("[WebPush] Test push: {sent} sent, {} stale", stale.len());

            if !stale.is_empty() {
                let _ = event_tx.send(crate::hub::events::HubEvent::PushSubscriptionsExpired {
                    identities: stale,
                });
            }
        });
    }

    pub(super) fn send_push_test_ack(&self, browser_identity: &str, count: usize) {
        let msg = serde_json::json!({ "type": "push_test_ack", "sent": count });
        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_test_ack: {e}");
                return;
            }
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
    }

    pub(super) fn handle_push_disable(&mut self, browser_identity: &str) {
        // Clear all stored push subscriptions
        self.push_subscriptions = crate::notifications::push::PushSubscriptionStore::default();
        if let Err(e) = crate::relay::persistence::save_push_subscriptions(&self.push_subscriptions)
        {
            log::error!("[WebPush] Failed to clear push subscriptions: {e}");
        }

        self.set_notifications_enabled(false);

        log::info!("[WebPush] Push notifications disabled");

        // Ack browser
        let msg = serde_json::json!({ "type": "push_disable_ack" });
        let payload = match serde_json::to_vec(&msg) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_disable_ack: {e}");
                return;
            }
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
    }

    pub(super) fn handle_push_status_request(
        &mut self,
        browser_identity: &str,
        msg: &serde_json::Value,
    ) {
        let has_keys = self.vapid_keys.is_some();

        // Use stable browser_id when available, fall back to ephemeral identity
        let browser_id = msg
            .get("browser_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(browser_identity);

        let browser_subscribed = self.push_subscriptions.contains(browser_id);

        let vapid_pub = self
            .vapid_keys
            .as_ref()
            .map(|k| k.public_key_base64url().to_string());

        let response = serde_json::json!({
            "type": "push_status",
            "has_keys": has_keys,
            "browser_subscribed": browser_subscribed,
            "vapid_pub": vapid_pub,
        });

        let payload = match serde_json::to_vec(&response) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[WebPush] Failed to serialize push_status: {e}");
                return;
            }
        };
        self.queue_webrtc_peer_command(
            browser_identity,
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: payload },
        );
        log::info!(
            "[WebPush] Queued push_status for {} (has_keys={has_keys}, subscribed={browser_subscribed})",
            &browser_identity[..browser_identity.len().min(8)]
        );
    }

    pub(super) fn set_notifications_enabled(&self, enabled: bool) {
        let Some(ref hub_id) = self.botster_id else {
            log::warn!("[WebPush] No hub_id, cannot update notifications_enabled on Rails");
            return;
        };
        let url = format!("{}/hubs/{}", self.config.server_url, hub_id);
        let body = serde_json::json!({ "notifications_enabled": enabled });
        // block_in_place: reqwest::blocking cannot run inside a tokio runtime
        // (it drops an internal runtime, which panics in async context).
        let result = tokio::task::block_in_place(|| {
            self.client
                .patch(&url)
                .bearer_auth(self.config.get_api_key())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
        });
        match result {
            Ok(response) if response.status().is_success() => {
                log::info!("[WebPush] Set notifications_enabled={enabled} on Rails");
            }
            Ok(response) => {
                log::warn!(
                    "[WebPush] Failed to update notifications_enabled: {}",
                    response.status()
                );
            }
            Err(e) => log::warn!("[WebPush] Failed to update notifications_enabled: {e}"),
        }
    }

    pub(super) fn handle_lua_push_request(&mut self, lua_payload: serde_json::Value) {
        let Some(ref vapid) = self.vapid_keys else {
            return;
        };
        if self.push_subscriptions.is_empty() {
            return;
        }

        let Some(ref hub_id) = self.botster_id else {
            log::warn!("[WebPush] Cannot send Lua push: no server hub ID yet");
            return;
        };

        let base_url = self.config.server_url.trim_end_matches('/');
        let lua = lua_payload.as_object();

        // Extract fields from Lua payload with defaults
        let id = lua
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = if id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            id
        };

        let kind = lua
            .and_then(|o| o.get("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or("agent_alert");
        let title = lua
            .and_then(|o| o.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("Botster");
        let body = lua
            .and_then(|o| o.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("Your attention is needed");
        let relative_url = lua
            .and_then(|o| o.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let relative_url = if relative_url.is_empty() {
            format!("/hubs/{hub_id}")
        } else {
            relative_url
        };

        let icon_path = lua
            .and_then(|o| o.get("icon"))
            .and_then(|v| v.as_str())
            .unwrap_or("/icon.png");

        // Build absolute URLs for declarative web push `navigate` field
        let navigate_url = if relative_url.starts_with("http") {
            relative_url.clone()
        } else {
            format!("{base_url}{relative_url}")
        };
        let icon_url = if icon_path.starts_with("http") {
            icon_path.to_string()
        } else {
            format!("{base_url}{icon_path}")
        };

        let data = serde_json::json!({
            "id": id,
            "kind": kind,
            "hubId": hub_id,
            "url": relative_url,
            "createdAt": chrono::Utc::now().to_rfc3339(),
        });

        let mut notification = serde_json::json!({
            "title": title,
            "body": body,
            "icon": icon_url,
            "navigate": navigate_url,
            "data": data,
        });

        // Forward optional `tag` field
        if let Some(tag) = lua.and_then(|o| o.get("tag")) {
            notification["tag"] = tag.clone();
        }

        let mut payload = serde_json::json!({
            "web_push": 8030,
            "notification": notification,
        });

        // Forward any extra Lua fields to the top-level payload (e.g. app_badge).
        // This keeps Rust generic — Lua uses Declarative Web Push field names directly.
        const CONSUMED_KEYS: &[&str] = &[
            "kind",
            "title",
            "body",
            "url",
            "icon",
            "tag",
            "id",
            "web_push",
            "notification", // prevent overwriting structured fields
        ];
        if let Some(obj) = lua {
            for (key, value) in obj {
                if !CONSUMED_KEYS.contains(&key.as_str()) {
                    payload[key] = value.clone();
                }
            }
        }

        let payload_bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                log::error!("[WebPush] Failed to serialize Lua push payload: {e}");
                return;
            }
        };

        let vapid_b64 = vapid.private_key_base64url().to_string();

        let subs: Vec<(String, crate::notifications::push::PushSubscription)> = self
            .push_subscriptions
            .all()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.handle().spawn(async move {
            let client = reqwest::Client::new();
            let mut stale = Vec::new();
            let mut sent = 0usize;

            for (identity, sub) in &subs {
                match send_push_direct(&client, &vapid_b64, sub, &payload_bytes).await {
                    Ok(true) => sent += 1,
                    Ok(false) => stale.push(identity.clone()),
                    Err(e) => {
                        log::error!(
                            "[WebPush] Lua push failed for {}: {e}",
                            &identity[..identity.len().min(8)]
                        );
                    }
                }
            }

            if sent > 0 || !stale.is_empty() {
                log::info!("[WebPush] Lua push: {sent} sent, {} stale", stale.len());
            }

            if !stale.is_empty() {
                let _ = event_tx.send(crate::hub::events::HubEvent::PushSubscriptionsExpired {
                    identities: stale,
                });
            }
        });
    }
}
