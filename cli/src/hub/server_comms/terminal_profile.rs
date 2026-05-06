use super::*;

impl Hub {
    pub(super) fn boot_terminal_colors(
        &self,
    ) -> std::collections::HashMap<usize, crate::terminal::Rgb> {
        self.shared_color_cache
            .lock()
            .map(|colors| colors.clone())
            .unwrap_or_default()
    }

    pub(super) fn pick_replacement_terminal_peer(
        &self,
        session_uuid: &str,
        excluding_peer_id: &str,
    ) -> Option<String> {
        self.terminal_session_peers
            .get(session_uuid)
            .into_iter()
            .flat_map(|peers| peers.iter())
            .filter(|peer_id| peer_id.as_str() != excluding_peer_id)
            .filter(|peer_id| self.terminal_client_profiles.contains_key(*peer_id))
            .min()
            .cloned()
    }

    pub(super) fn effective_terminal_colors(
        &self,
        session_uuid: &str,
    ) -> std::collections::HashMap<usize, crate::terminal::Rgb> {
        let active_peer = self
            .active_terminal_peers
            .lock()
            .ok()
            .and_then(|active| active.get(session_uuid).cloned());

        let mut colors = self.boot_terminal_colors();

        if let Some(peer_id) = active_peer {
            if let Some(peer_colors) = self.terminal_client_profiles.get(&peer_id) {
                colors.extend(peer_colors.iter().map(|(k, v)| (*k, *v)));
            }
        }

        colors
    }

    pub(super) fn sync_session_terminal_profile(&mut self, session_uuid: &str) {
        let Some(session_handle) = self.handle_cache.get_session(session_uuid) else {
            return;
        };

        let colors = self.effective_terminal_colors(session_uuid);
        if colors.is_empty() {
            return;
        }

        log::debug!(
            "[PTY-PROFILE] syncing session profile session={} colors={} active_peer={:?}",
            &session_uuid[..session_uuid.len().min(16)],
            colors.len(),
            self.active_terminal_peers
                .lock()
                .ok()
                .and_then(|active| active.get(session_uuid).cloned())
        );

        if let Err(error) = session_handle.pty().set_color_profile(&colors) {
            log::warn!(
                "[PTY-PROFILE] Failed to sync session {} color profile: {}",
                &session_uuid[..session_uuid.len().min(16)],
                error
            );
        }
    }

    pub(super) fn sync_active_sessions_for_terminal_peer(&mut self, peer_id: &str) {
        let session_ids: Vec<String> = self
            .active_terminal_peers
            .lock()
            .ok()
            .into_iter()
            .flat_map(|active| {
                active
                    .iter()
                    .filter_map(|(session_uuid, active_peer)| {
                        (active_peer == peer_id).then(|| session_uuid.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        for session_uuid in session_ids {
            self.sync_session_terminal_profile(&session_uuid);
        }
    }

    pub(super) fn update_terminal_client_profile(
        &mut self,
        peer_id: &str,
        colors: std::collections::HashMap<usize, crate::terminal::Rgb>,
    ) {
        // Merge into the shared boot cache so newly spawned sessions inherit
        // current colors. Uses extend (not replace) so a partial client profile
        // (e.g. fg/bg only) doesn't erase existing palette entries.
        if let Ok(mut shared) = self.shared_color_cache.lock() {
            shared.extend(colors.iter().map(|(k, v)| (*k, *v)));
        }
        self.terminal_client_profiles
            .entry(peer_id.to_string())
            .or_default()
            .extend(colors);
        self.sync_active_sessions_for_terminal_peer(peer_id);
    }

    pub(super) fn register_terminal_subscription_peer(
        &mut self,
        subscription_key: &str,
        session_uuid: &str,
        peer_id: &str,
    ) {
        self.terminal_subscription_peers.insert(
            subscription_key.to_string(),
            (session_uuid.to_string(), peer_id.to_string()),
        );
        self.terminal_session_peers
            .entry(session_uuid.to_string())
            .or_default()
            .insert(peer_id.to_string());
    }

    pub(super) fn unregister_terminal_subscription_peer(
        &mut self,
        subscription_key: &str,
        promote_next: bool,
    ) {
        self.cleanup_pending_session_io_snapshots_for_subscription(subscription_key);
        let Some((session_uuid, peer_id)) =
            self.terminal_subscription_peers.remove(subscription_key)
        else {
            return;
        };

        let mut remove_session_entry = false;
        if let Some(peers) = self.terminal_session_peers.get_mut(&session_uuid) {
            peers.remove(&peer_id);
            remove_session_entry = peers.is_empty();
        }
        if remove_session_entry {
            self.terminal_session_peers.remove(&session_uuid);
        }

        let mut should_sync = false;
        if let Ok(mut active) = self.active_terminal_peers.lock() {
            if active
                .get(&session_uuid)
                .is_some_and(|current| current == &peer_id)
            {
                active.remove(&session_uuid);
                if promote_next {
                    if let Some(next_peer) =
                        self.pick_replacement_terminal_peer(&session_uuid, &peer_id)
                    {
                        active.insert(session_uuid.clone(), next_peer);
                    }
                }
                should_sync = true;
            }
        }

        if should_sync {
            self.sync_session_terminal_profile(&session_uuid);
        }
    }

    pub(super) fn unregister_terminal_client_peer(&mut self, peer_id: &str, promote_next: bool) {
        self.terminal_client_profiles.remove(peer_id);

        let subscription_keys: Vec<String> = self
            .terminal_subscription_peers
            .iter()
            .filter_map(|(subscription_key, (_, owner_peer))| {
                (owner_peer == peer_id).then(|| subscription_key.clone())
            })
            .collect();

        for subscription_key in subscription_keys {
            self.unregister_terminal_subscription_peer(&subscription_key, promote_next);
        }
    }

    pub(super) fn handle_terminal_color_profile_message(
        &mut self,
        peer_id: &str,
        msg: &serde_json::Value,
    ) -> bool {
        if msg.get("type").and_then(|value| value.as_str()) != Some("terminal_color_profile") {
            return false;
        }

        let colors: std::collections::HashMap<usize, crate::terminal::Rgb> = msg
            .get("colors")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let session_uuid = msg
            .get("session_uuid")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let bg = colors.get(&257usize).copied();
        log::debug!(
            "[PTY-PROFILE] learned client profile peer={} session={} colors={} bg={:?}",
            peer_id,
            session_uuid,
            colors.len(),
            bg
        );
        self.update_terminal_client_profile(peer_id, colors);
        true
    }

    pub(super) fn set_active_terminal_peer(
        &mut self,
        session_uuid: &str,
        peer_id: &str,
        focused: bool,
    ) {
        let Ok(mut active) = self.active_terminal_peers.lock() else {
            return;
        };

        if focused {
            active.insert(session_uuid.to_string(), peer_id.to_string());
        } else if active
            .get(session_uuid)
            .is_some_and(|current| current == peer_id)
        {
            active.remove(session_uuid);
        } else {
            return;
        }

        drop(active);
        self.sync_session_terminal_profile(session_uuid);
    }

    #[cfg(test)]
    pub(super) fn learn_terminal_probe_replies(
        &mut self,
        session_uuid: &str,
        peer_id: &str,
        data: &[u8],
    ) {
        let descriptions = crate::hub::terminal_profile::describe_probe_sequences(data);
        if !descriptions.is_empty() {
            log::info!(
                "[PTY-PROBE] Learned terminal reply candidates from peer={} session={}: {}",
                peer_id,
                session_uuid,
                descriptions.join(", ")
            );
        }
        self.terminal_profiles
            .observe_input(session_uuid, peer_id, data);
    }
}
