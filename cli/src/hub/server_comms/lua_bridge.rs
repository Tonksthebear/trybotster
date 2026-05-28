use super::*;

impl Hub {
    /// Handle a completed worktree creation result from Lua primitives.
    pub fn handle_worktree_result(&mut self, result: crate::lua::primitives::WorktreeCreateResult) {
        match result.result {
            Ok(ref path) => {
                let path_str = path.to_string_lossy().to_string();
                log::info!(
                    "[Worktree] Async creation complete: {} at {}",
                    result.branch,
                    path_str
                );

                let mut worktrees = self.handle_cache.get_worktrees();
                worktrees.push((path_str.clone(), result.branch.clone()));
                self.handle_cache.set_worktrees(worktrees);

                let event_data = serde_json::json!({
                    "label": result.label,
                    "branch": result.branch,
                    "path": path_str,
                    "metadata": result.metadata,
                    "prompt": result.prompt,
                    "agent_name": result.agent_name,
                    "client_rows": result.client_rows,
                    "client_cols": result.client_cols,
                });
                if let Err(e) = self.lua.fire_json_event("worktree_created", &event_data) {
                    log::error!("[Worktree] Failed to fire worktree_created event: {e}");
                }
            }
            Err(ref error) => {
                log::error!(
                    "[Worktree] Async creation failed for {}: {}",
                    result.branch,
                    error
                );

                let event_data = serde_json::json!({
                    "label": result.label,
                    "branch": result.branch,
                    "error": error,
                });
                if let Err(e) = self
                    .lua
                    .fire_json_event("worktree_create_failed", &event_data)
                {
                    log::error!("[Worktree] Failed to fire worktree_create_failed event: {e}");
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn poll_user_file_watches(&self) {
        let fired = self.lua.poll_user_file_watches();
        if fired > 0 {
            log::debug!("Fired {} user file watch event(s)", fired);
        }
    }

    #[cfg(test)]
    pub(super) fn poll_lua_timers(&self) {
        let fired = self.lua.poll_timers();
        if fired > 0 {
            log::debug!("Fired {} Lua timer callback(s)", fired);
        }
    }

    #[cfg(test)]
    pub(super) fn poll_lua_http_responses(&self) {
        let fired = self.lua.poll_http_responses();
        if fired > 0 {
            log::debug!("Fired {} Lua HTTP callback(s)", fired);
        }
    }

    pub(super) fn spawn_notification_watcher(
        &mut self,
        watcher_key: String,
        session_uuid: String,
        session_name: String,
        observe_output: bool,
        event_tx: tokio::sync::broadcast::Sender<crate::agent::pty::PtyEvent>,
    ) {
        // Abort any existing watcher for this key
        if let Some(old) = self.notification_watcher_handles.remove(&watcher_key) {
            old.abort();
            log::debug!(
                "[NotifWatcher] Aborted existing watcher for {}",
                watcher_key
            );
        }

        let hub_tx = self.hub_event_tx.clone();
        let mut rx = event_tx.subscribe();
        let key = watcher_key.clone();

        let _guard = self.tokio_runtime.enter();
        let task = tokio::spawn(async move {
            use crate::agent::pty::PtyEvent;

            log::info!("[NotifWatcher] Started for {}", key);

            loop {
                match rx.recv().await {
                    Ok(PtyEvent::Notification(notif)) => {
                        log::debug!("[NotifWatcher] Notification for {}: {:?}", key, notif);
                        let event = crate::hub::PtyNotificationEvent {
                            session_uuid: session_uuid.clone(),
                            session_name: session_name.clone(),
                            notification: notif,
                        };
                        if hub_tx
                            .send(crate::hub::events::HubEvent::PtyNotification(event))
                            .is_err()
                        {
                            log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                            break;
                        }
                    }
                    Ok(PtyEvent::ProcessExited { exit_code }) => {
                        log::info!(
                            "[NotifWatcher] Process exited (code={:?}) for {}",
                            exit_code,
                            key
                        );
                        let event = crate::hub::events::HubEvent::PtyProcessExited {
                            session_uuid: session_uuid.clone(),
                            session_name: session_name.clone(),
                            exit_code,
                        };
                        let _ = hub_tx.send(event);
                        break;
                    }
                    Ok(PtyEvent::Output(data)) => {
                        if observe_output {
                            let event = crate::hub::events::HubEvent::PtyOutput {
                                session_uuid: session_uuid.clone(),
                                session_name: session_name.clone(),
                                data,
                            };
                            if hub_tx.send(event).is_err() {
                                log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                                break;
                            }
                        }
                    }
                    Ok(event @ PtyEvent::TitleChanged(_))
                    | Ok(event @ PtyEvent::CwdChanged(_))
                    | Ok(event @ PtyEvent::PromptMark(_))
                    | Ok(event @ PtyEvent::CursorVisibilityChanged(_)) => {
                        if hub_tx
                            .send(crate::hub::events::HubEvent::PtyOscEvent {
                                session_uuid: session_uuid.clone(),
                                session_name: session_name.clone(),
                                event,
                            })
                            .is_err()
                        {
                            log::warn!("[NotifWatcher] Hub event channel closed for {}", key);
                            break;
                        }
                    }
                    Ok(_) => {
                        // Ignore other events (Output, Resized)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("[NotifWatcher] Lagged by {} events for {}", n, key);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        log::info!("[NotifWatcher] Channel closed for {}", key);
                        break;
                    }
                }
            }
        });

        self.notification_watcher_handles.insert(watcher_key, task);
    }

    #[cfg(test)]
    pub(super) fn poll_pty_notifications(&mut self) {
        let events: Vec<crate::hub::PtyNotificationEvent> = {
            let mut queue = self
                .pty_notification_queue
                .lock()
                .expect("pty_notification_queue lock poisoned");
            std::mem::take(&mut *queue)
        };

        if events.is_empty() {
            return;
        }

        for event in events {
            self.lua.notify_pty_notification(
                &event.session_uuid,
                &event.session_name,
                &event.notification,
            );
        }
    }

    #[cfg(test)]
    pub(super) fn poll_lua_websocket_events(&mut self) {
        let _count = self.lua.poll_websocket_events();
    }

    pub(super) fn process_single_action_cable_request(
        &mut self,
        request: crate::lua::primitives::ActionCableRequest,
    ) {
        use crate::lua::primitives::action_cable::{LuaAcChannel, LuaAcConnection};
        use crate::lua::primitives::ActionCableRequest;

        match request {
            ActionCableRequest::Connect {
                connection_id,
                crypto,
            } => {
                let handle = self.tokio_runtime.handle().clone();
                let _guard = handle.enter();
                let connection =
                    crate::hub::action_cable_connection::ActionCableConnection::connect(
                        &self.config.server_url,
                        self.config.get_api_key(),
                    );
                self.lua_ac_connections.insert(
                    connection_id.clone(),
                    LuaAcConnection {
                        connection,
                        crypto_enabled: crypto,
                    },
                );
                log::info!(
                    "[ActionCable-Lua] Connection '{}' opened (crypto={})",
                    connection_id,
                    crypto
                );
            }

            ActionCableRequest::Subscribe {
                connection_id,
                channel_id,
                channel_name,
                params,
                owner_plugin,
                handler_id,
            } => {
                if let Some(conn) = self.lua_ac_connections.get(&connection_id) {
                    if owner_plugin.is_some() {
                        if let Ok(mut registry) = self.lua.ac_callback_registry().lock() {
                            registry.insert(
                                channel_id.clone(),
                                crate::lua::primitives::action_cable::AcCallbackEntry {
                                    callback_key: None,
                                    owner_plugin: owner_plugin.clone(),
                                    handler_id: handler_id.clone(),
                                },
                            );
                        }
                    }

                    // Build the ActionCable identifier JSON with channel name and params
                    let mut identifier = serde_json::json!({ "channel": channel_name });
                    if let serde_json::Value::Object(map) = params {
                        if let serde_json::Value::Object(ref mut id_map) = identifier {
                            for (k, v) in map {
                                id_map.insert(k, v);
                            }
                        }
                    }

                    let mut ch_handle = conn.connection.subscribe(identifier);

                    // Spawn a forwarding task for incoming channel messages.
                    let forwarder_handle = if let Some(mut rx) = ch_handle.take_message_rx() {
                        let tx = self.hub_event_tx.clone();
                        let ch_id = channel_id.clone();
                        let handle = self.tokio_runtime.handle().clone();
                        Some(handle.spawn(async move {
                            while let Some(msg) = rx.recv().await {
                                if tx
                                    .send(crate::hub::events::HubEvent::AcChannelMessage {
                                        channel_id: ch_id.clone(),
                                        message: msg,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }))
                    } else {
                        None
                    };

                    self.lua_ac_channels.insert(
                        channel_id.clone(),
                        LuaAcChannel {
                            handle: ch_handle,
                            connection_id,
                            forwarder_handle,
                        },
                    );
                    log::info!(
                        "[ActionCable-Lua] Channel '{}' subscribed to '{}'",
                        channel_id,
                        channel_name
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Subscribe failed: connection '{}' not found",
                        connection_id
                    );
                }
            }

            ActionCableRequest::Perform {
                channel_id,
                action,
                data,
            } => {
                if let Some(ch) = self.lua_ac_channels.get(&channel_id) {
                    ch.handle.perform(&action, data);
                    log::trace!(
                        "[ActionCable-Lua] Performed '{}' on channel '{}'",
                        action,
                        channel_id
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Perform failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Unsubscribe { channel_id } => {
                if self.lua_ac_channels.remove(&channel_id).is_some() {
                    // Clean up the callback registry entry and release the RegistryKey.
                    if let Ok(mut reg) = self.lua.ac_callback_registry().lock() {
                        if let Some(entry) = reg.remove(&channel_id) {
                            // Only platform entries have a hub-Lua RegistryKey to release.
                            if let Some(key) = entry.callback_key {
                                let _ = self.lua.lua_ref().remove_registry_value(key);
                            }
                            // Owned entries: explicitly unregister the handler in the
                            // worker VM so the handlers table does not leak (V9).
                            if let (Some(owner), Some(hid)) =
                                (&entry.owner_plugin, &entry.handler_id)
                            {
                                if let Ok(invoke) = self
                                    .lua
                                    .lua_ref()
                                    .globals()
                                    .get::<mlua::Function>("__plugin_worker_invoke")
                                {
                                    if let Err(e) = invoke.call::<mlua::Value>((
                                        owner.clone(),
                                        "ac_unregister".to_string(),
                                        hid.clone(),
                                        mlua::Value::Nil,
                                        mlua::Value::Nil,
                                        250u64,
                                    )) {
                                        log::warn!("[ActionCable] Failed to unregister owned handler {hid} in worker {owner}: {e}");
                                    }
                                }
                            }
                        }
                    }
                    log::info!("[ActionCable-Lua] Channel '{}' unsubscribed", channel_id);
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Unsubscribe failed: channel '{}' not found",
                        channel_id
                    );
                }
            }

            ActionCableRequest::Close { connection_id } => {
                // Remove all channels belonging to this connection
                let orphaned: Vec<String> = self
                    .lua_ac_channels
                    .iter()
                    .filter(|(_, ch)| ch.connection_id == connection_id)
                    .map(|(id, _)| id.clone())
                    .collect();

                for ch_id in &orphaned {
                    self.lua_ac_channels.remove(ch_id);
                }

                // Clean up callback registry entries for all removed channels.
                if let Ok(mut reg) = self.lua.ac_callback_registry().lock() {
                    for ch_id in &orphaned {
                        if let Some(entry) = reg.remove(ch_id) {
                            // Only platform entries have a hub-Lua RegistryKey to release.
                            if let Some(key) = entry.callback_key {
                                let _ = self.lua.lua_ref().remove_registry_value(key);
                            }
                            // Owned entries: explicitly unregister the handler in the
                            // worker VM so the handlers table does not leak (V9).
                            if let (Some(owner), Some(hid)) =
                                (&entry.owner_plugin, &entry.handler_id)
                            {
                                if let Ok(invoke) = self
                                    .lua
                                    .lua_ref()
                                    .globals()
                                    .get::<mlua::Function>("__plugin_worker_invoke")
                                {
                                    if let Err(e) = invoke.call::<mlua::Value>((
                                        owner.clone(),
                                        "ac_unregister".to_string(),
                                        hid.clone(),
                                        mlua::Value::Nil,
                                        mlua::Value::Nil,
                                        250u64,
                                    )) {
                                        log::warn!("[ActionCable] Failed to unregister owned handler {hid} in worker {owner}: {e}");
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(conn) = self.lua_ac_connections.remove(&connection_id) {
                    conn.connection.shutdown();
                    log::info!(
                        "[ActionCable-Lua] Connection '{}' closed ({} channels removed)",
                        connection_id,
                        orphaned.len()
                    );
                } else {
                    log::warn!(
                        "[ActionCable-Lua] Close failed: connection '{}' not found",
                        connection_id
                    );
                }
            }
        }
    }

    pub(super) fn process_hub_client_request(
        &mut self,
        request: crate::lua::primitives::HubClientRequest,
    ) {
        use crate::lua::primitives::hub_client::LuaHubClientConn;
        use crate::lua::primitives::HubClientRequest;
        use crate::socket::framing::{Frame, FrameDecoder};

        match request {
            HubClientRequest::Connect {
                connection_id,
                socket_path,
            } => {
                let hub_tx = self.hub_event_tx.clone();
                let conn_id = connection_id.clone();
                let handle = self.tokio_runtime.handle().clone();

                let hub_tx2 = hub_tx.clone();
                let conn_id2 = conn_id.clone();
                // Clone pending_requests so the read task can deliver _mcp_rid
                // responses directly, bypassing the Hub event loop. This is
                // required because hub_client.request() blocks the event loop
                // thread via recv_timeout() — the event loop cannot process
                // HubClientMessage while Lua is blocked.
                let pending_requests2 =
                    std::sync::Arc::clone(self.lua.hub_client_pending_requests());

                // Use std UnixStream::connect (synchronous) and convert to tokio.
                // Cannot use tokio's async connect here because we're inside the
                // Hub's block_on event loop — nested block_on panics.
                let std_stream = match std::os::unix::net::UnixStream::connect(&socket_path) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("[HubClient] Failed to connect to {}: {}", socket_path, e);
                        return;
                    }
                };
                if let Err(e) = std_stream.set_nonblocking(true) {
                    log::warn!(
                        "[HubClient] Failed to set nonblocking on {}: {}",
                        socket_path,
                        e
                    );
                    return;
                }
                let stream = match tokio::net::UnixStream::from_std(std_stream) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!(
                            "[HubClient] Failed to convert to tokio stream for {}: {}",
                            socket_path,
                            e
                        );
                        return;
                    }
                };

                let (read_half, write_half) = stream.into_split();
                let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

                // Subscribe immediately (same as TuiBridge)
                let sub_frame = Frame::Json(serde_json::json!({
                    "type": "subscribe",
                    "channel": "hub",
                    "subscriptionId": format!("hub_client_{}", conn_id)
                }));
                let _ = frame_tx.send(sub_frame.encode());

                // Spawn write task
                let write_handle = handle.spawn(async move {
                    let mut writer = tokio::io::BufWriter::new(write_half);
                    while let Some(data) = frame_rx.recv().await {
                        use tokio::io::AsyncWriteExt;
                        if writer.write_all(&data).await.is_err() {
                            break;
                        }
                        if writer.flush().await.is_err() {
                            break;
                        }
                    }
                });

                // Spawn read task
                let read_handle = handle.spawn(async move {
                    let mut reader = tokio::io::BufReader::new(read_half);
                    let mut decoder = FrameDecoder::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        use tokio::io::AsyncReadExt;
                        match reader.read(&mut buf).await {
                            Ok(0) | Err(_) => {
                                let _ = hub_tx2.send(
                                    crate::hub::events::HubEvent::HubClientDisconnected {
                                        connection_id: conn_id2.clone(),
                                    },
                                );
                                break;
                            }
                            Ok(n) => {
                                match decoder.feed(&buf[..n]) {
                                    Ok(frames) => {
                                        for frame in frames {
                                            if let Frame::Json(v) = frame {
                                                // Short-circuit _mcp_rid responses directly to
                                                // the pending_requests map. hub_client.request()
                                                // blocks the Hub event loop thread via recv_timeout(),
                                                // so we cannot route through HubEvent — the event
                                                // loop is not being polled while Lua waits.
                                                if let Some(rid) = v.get("_mcp_rid").and_then(|r| r.as_str()) {
                                                    let sender = {
                                                        let mut map = pending_requests2
                                                            .lock()
                                                            .expect("HubClientPendingRequests mutex poisoned");
                                                        map.remove(rid)
                                                    };
                                                    if let Some(tx) = sender {
                                                        let _ = tx.send(v);
                                                        continue;
                                                    }
                                                }
                                                let _ = hub_tx2.send(
                                                    crate::hub::events::HubEvent::HubClientMessage {
                                                        connection_id: conn_id2.clone(),
                                                        message: v,
                                                    },
                                                );
                                            }
                                            // Other frame types (PtyOutput etc) could be handled later
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "[HubClient] Frame decode error on '{}': {}",
                                            conn_id2,
                                            e
                                        );
                                        let _ = hub_tx2.send(
                                            crate::hub::events::HubEvent::HubClientDisconnected {
                                                connection_id: conn_id2.clone(),
                                            },
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }
                });

                // Register the frame sender so hub_client.request() can write
                // directly without going through the Hub event loop.
                if let Ok(mut senders) = self.lua.hub_client_frame_senders().lock() {
                    senders.insert(connection_id.clone(), frame_tx.clone());
                }

                // Store connection
                self.lua_hub_client_connections.insert(
                    connection_id.clone(),
                    LuaHubClientConn {
                        frame_tx,
                        read_handle,
                        write_handle,
                    },
                );
                log::info!(
                    "[HubClient] Connection '{}' opened to '{}'",
                    connection_id,
                    socket_path
                );
            }

            HubClientRequest::Send {
                connection_id,
                data,
            } => {
                if let Some(conn) = self.lua_hub_client_connections.get(&connection_id) {
                    let frame = Frame::Json(data);
                    if conn.frame_tx.send(frame.encode()).is_err() {
                        log::warn!(
                            "[HubClient] Send failed: write task closed for '{}'",
                            connection_id
                        );
                    } else {
                        log::trace!("[HubClient] Sent frame to '{}'", connection_id);
                    }
                } else {
                    log::warn!(
                        "[HubClient] Send failed: connection '{}' not found",
                        connection_id
                    );
                }
            }

            HubClientRequest::Close { connection_id } => {
                if self
                    .lua_hub_client_connections
                    .remove(&connection_id)
                    .is_some()
                {
                    // Clean up the callback registry entry and release the RegistryKey.
                    if let Ok(mut reg) = self.lua.hub_client_callback_registry().lock() {
                        if let Some(key) = reg.remove(&connection_id) {
                            let _ = self.lua.lua_ref().remove_registry_value(key);
                        }
                    }
                    // Remove the direct frame sender (used by hub_client.request()).
                    if let Ok(mut senders) = self.lua.hub_client_frame_senders().lock() {
                        senders.remove(&connection_id);
                    }
                    log::info!("[HubClient] Connection '{}' closed", connection_id);
                } else {
                    log::warn!(
                        "[HubClient] Close failed: connection '{}' not found",
                        connection_id
                    );
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn poll_lua_action_cable_channels(&mut self) {
        use crate::lua::primitives::action_cable;

        let crypto = self.browser.crypto_service.as_ref();
        let _count = action_cable::poll_lua_action_cable_channels(
            self.lua.lua_ref(),
            &mut self.lua_ac_channels,
            &self.lua_ac_connections,
            self.lua.ac_callback_registry(),
            crypto,
        );
    }

    #[cfg(test)]
    pub(super) fn poll_worktree_results(&mut self) {
        let Some(ref mut rx) = self.worktree_result_rx else {
            return;
        };
        let results: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        for result in results {
            match result.result {
                Ok(ref path) => {
                    let path_str = path.to_string_lossy().to_string();
                    log::info!(
                        "[Worktree] Async creation complete: {} at {}",
                        result.branch,
                        path_str
                    );

                    // Update HandleCache so worktree.find() sees it immediately
                    let mut worktrees = self.handle_cache.get_worktrees();
                    worktrees.push((path_str.clone(), result.branch.clone()));
                    self.handle_cache.set_worktrees(worktrees);

                    // Refresh state-level worktree list
                    if let Err(e) = self.load_available_worktrees() {
                        log::warn!("Failed to refresh worktrees after creation: {e}");
                    }

                    // Fire Lua event with all context for agent spawning
                    let event_data = serde_json::json!({
                        "label": result.label,
                        "branch": result.branch,
                        "path": path_str,
                        "metadata": result.metadata,
                        "prompt": result.prompt,
                        "agent_name": result.agent_name,
                        "client_rows": result.client_rows,
                        "client_cols": result.client_cols,
                    });
                    if let Err(e) = self.lua.fire_json_event("worktree_created", &event_data) {
                        log::error!("[Worktree] Failed to fire worktree_created event: {e}");
                    }
                }
                Err(ref error) => {
                    log::error!(
                        "[Worktree] Async creation failed for {}: {}",
                        result.branch,
                        error
                    );

                    let event_data = serde_json::json!({
                        "label": result.label,
                        "branch": result.branch,
                        "error": error,
                    });
                    if let Err(e) = self
                        .lua
                        .fire_json_event("worktree_create_failed", &event_data)
                    {
                        log::error!("[Worktree] Failed to fire worktree_create_failed event: {e}");
                    }
                }
            }
        }
    }
}
