use super::*;

impl Hub {
    pub(super) fn handle_action_cable_message_event(
        &mut self,
        channel_id: String,
        message: serde_json::Value,
    ) {
        use crate::lua::primitives::action_cable;

        let crypto = self.browser.crypto_service.as_ref();
        action_cable::fire_single_ac_message(
            self.lua.lua_ref(),
            &self.lua_ac_channels,
            &self.lua_ac_connections,
            self.lua.ac_callback_registry(),
            crypto,
            &channel_id,
            message,
        );
    }

    pub(super) fn handle_hub_client_message_event(
        &mut self,
        connection_id: String,
        message: serde_json::Value,
    ) {
        use crate::lua::primitives::hub_client;

        hub_client::fire_hub_client_message(
            self.lua.lua_ref(),
            self.lua.hub_client_callback_registry(),
            self.lua.hub_client_pending_requests(),
            &connection_id,
            message,
        );
    }

    pub(super) fn handle_hub_client_disconnected_event(&mut self, connection_id: String) {
        if self
            .lua_hub_client_connections
            .remove(&connection_id)
            .is_some()
        {
            if let Ok(mut reg) = self.lua.hub_client_callback_registry().lock() {
                if let Some(key) = reg.remove(&connection_id) {
                    let _ = self.lua.lua_ref().remove_registry_value(key);
                }
            }
            if let Ok(mut senders) = self.lua.hub_client_frame_senders().lock() {
                senders.remove(&connection_id);
            }
            log::info!(
                "[HubClient] Connection '{}' disconnected (remote EOF)",
                connection_id
            );
        }
    }

    pub(super) fn handle_lua_hub_request_event(
        &mut self,
        request: crate::lua::primitives::HubRequest,
    ) {
        use crate::lua::primitives::HubRequest;

        match request {
            HubRequest::Quit => {
                log::info!("[Lua] Processing quit request");
                self.quit = true;
            }
            HubRequest::ExecRestart => {
                log::info!("[Lua] Processing exec-restart request (self-update)");
                self.exec_restart = true;
                self.quit = true;
            }
            HubRequest::GracefulRestart => {
                log::info!("[Lua] Processing graceful-restart request — agents will survive");
                self.quit = true;
            }
            HubRequest::DevRebuild => {
                self.handle_lua_dev_rebuild_request();
            }
            HubRequest::ProbeUrlReady {
                connector_session_uuid,
                parent_session_uuid,
                url,
                hostname,
                timeout_secs,
            } => {
                self.spawn_url_ready_probe(
                    connector_session_uuid,
                    parent_session_uuid,
                    url,
                    hostname,
                    timeout_secs,
                );
            }
            HubRequest::PreparePluginCommand {
                request_id,
                command,
                config_path,
                config_contents,
                context,
            } => {
                self.spawn_prepare_plugin_command(
                    request_id,
                    command,
                    config_path,
                    config_contents,
                    context,
                );
            }
            HubRequest::RunCommandGate {
                request_id,
                command,
                cwd,
                timeout_secs,
                env,
                config_path,
                config_contents,
                metadata,
                context,
            } => {
                self.spawn_run_command_gate(
                    request_id,
                    command,
                    cwd,
                    timeout_secs,
                    env,
                    config_path,
                    config_contents,
                    metadata,
                    context,
                );
            }
            HubRequest::HandleSignalingMessage { message } => {
                self.handle_signaling_message(message);
            }
        }
    }

    fn handle_lua_dev_rebuild_request(&mut self) {
        let current_exe = std::env::current_exe().ok();
        let profile = current_exe
            .as_deref()
            .and_then(detect_running_cargo_profile);
        let target_dir = current_exe.as_deref().and_then(detect_running_target_dir);
        match &profile {
            Some(CargoBuildProfile::Debug) => {
                log::info!(
                    "[Dev] Starting cargo build (debug profile) — Hub will exec-restart on success"
                );
            }
            Some(CargoBuildProfile::Release) => {
                log::info!(
                    "[Dev] Starting cargo build (--release) — Hub will exec-restart on success"
                );
            }
            Some(CargoBuildProfile::Named(name)) => {
                log::info!(
                    "[Dev] Starting cargo build (--profile {}) — Hub will exec-restart on success",
                    name
                );
            }
            None => {
                log::info!(
                    "[Dev] Starting cargo build (default profile: debug) — Hub will exec-restart on success"
                );
            }
        }
        let tx = self.hub_event_tx.clone();
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let profile_for_build = profile.clone();
        let target_dir_for_build = target_dir.clone();
        if let Some(exe) = current_exe.as_ref() {
            log::info!("[Dev] Running executable: {}", exe.display());
        }
        if let Some(td) = target_dir.as_ref() {
            log::info!("[Dev] Using Cargo target-dir: {}", td.display());
        }
        self.tokio_runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let mut cmd = std::process::Command::new("cargo");
                cmd.arg("build")
                    .arg("--manifest-path")
                    .arg(format!("{manifest_dir}/Cargo.toml"))
                    .current_dir(manifest_dir)
                    .stdin(std::process::Stdio::null());
                if let Some(target_dir) = target_dir_for_build {
                    cmd.arg("--target-dir").arg(target_dir);
                }
                match profile_for_build {
                    Some(CargoBuildProfile::Debug) | None => {}
                    Some(CargoBuildProfile::Release) => {
                        cmd.arg("--release");
                    }
                    Some(CargoBuildProfile::Named(name)) => {
                        cmd.arg("--profile").arg(name);
                    }
                }
                cmd.status()
            })
            .await;

            match result {
                Ok(Ok(status)) if status.success() => {
                    log::info!("[Dev] cargo build succeeded — triggering exec-restart");
                    let _ = tx.send(crate::hub::events::HubEvent::LuaHubRequest(
                        crate::lua::primitives::HubRequest::ExecRestart,
                    ));
                }
                Ok(Ok(status)) => {
                    log::error!("[Dev] cargo build failed with exit status: {status}");
                }
                Ok(Err(e)) => {
                    log::error!("[Dev] cargo build failed to launch: {e}");
                }
                Err(e) => {
                    log::error!("[Dev] cargo build task panicked: {e}");
                }
            }
        });
    }

    fn spawn_url_ready_probe(
        &mut self,
        connector_session_uuid: String,
        parent_session_uuid: String,
        url: String,
        hostname: String,
        timeout_secs: f64,
    ) {
        log::info!(
            "[UrlReadyProbe] Probe start connector={} parent={} url={} hostname={} timeout_secs={:.1}",
            connector_session_uuid,
            parent_session_uuid,
            url,
            hostname,
            timeout_secs
        );
        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.spawn(async move {
            let result = crate::plugin_helpers::wait_until_url_ready(
                &hostname,
                &url,
                std::time::Duration::from_secs_f64(timeout_secs.max(0.1)),
            )
            .await;
            let (ready, error) = match result {
                Ok(()) => {
                    log::info!(
                        "[UrlReadyProbe] Probe success connector={} parent={} url={}",
                        connector_session_uuid,
                        parent_session_uuid,
                        url
                    );
                    (true, None)
                }
                Err(e) => {
                    log::warn!(
                        "[UrlReadyProbe] Probe failure connector={} parent={} url={} reason={}",
                        connector_session_uuid,
                        parent_session_uuid,
                        url,
                        e
                    );
                    (false, Some(e))
                }
            };
            let _ = event_tx.send(crate::hub::events::HubEvent::UrlProbeReady {
                connector_session_uuid,
                parent_session_uuid,
                url,
                ready,
                error,
            });
        });
    }

    fn spawn_prepare_plugin_command(
        &mut self,
        request_id: String,
        command: String,
        config_path: Option<String>,
        config_contents: Option<String>,
        context: serde_json::Value,
    ) {
        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.spawn(async move {
            let request_id_for_task = request_id.clone();
            let config_path_for_task = config_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let config_path_ref = config_path_for_task.as_deref().map(Path::new);
                crate::plugin_helpers::prepare_plugin_command(
                    &command,
                    config_path_ref,
                    config_contents.as_deref(),
                )
            })
            .await;

            let event = match result {
                Ok(Ok(prepared)) => crate::hub::events::HubEvent::PluginCommandPrepared {
                    request_id: request_id_for_task,
                    command: Some(prepared.command.to_string_lossy().into_owned()),
                    config_path: prepared
                        .config_path
                        .map(|path| path.to_string_lossy().into_owned()),
                    context,
                    error_kind: None,
                    error: None,
                },
                Ok(Err(error)) => crate::hub::events::HubEvent::PluginCommandPrepared {
                    request_id: request_id_for_task,
                    command: None,
                    config_path,
                    context,
                    error_kind: Some(error.kind.as_str().to_string()),
                    error: Some(error.to_string()),
                },
                Err(error) => crate::hub::events::HubEvent::PluginCommandPrepared {
                    request_id: request_id_for_task,
                    command: None,
                    config_path,
                    context,
                    error_kind: Some("task_failed".to_string()),
                    error: Some(format!("Plugin command preparation task failed: {error}")),
                },
            };
            let _ = event_tx.send(event);
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_run_command_gate(
        &mut self,
        request_id: String,
        command: String,
        cwd: String,
        timeout_secs: f64,
        env: std::collections::BTreeMap<String, String>,
        config_path: Option<String>,
        config_contents: Option<String>,
        metadata: serde_json::Value,
        context: serde_json::Value,
    ) {
        let event_tx = self.hub_event_tx.clone();
        self.tokio_runtime.spawn(async move {
            let request_id_for_task = request_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::plugin_helpers::run_command_gate(crate::plugin_helpers::CommandGateRequest {
                    command,
                    cwd: std::path::PathBuf::from(cwd),
                    timeout: if timeout_secs > 0.0 {
                        std::time::Duration::from_secs_f64(timeout_secs)
                    } else {
                        std::time::Duration::ZERO
                    },
                    env,
                    config_path: config_path.map(std::path::PathBuf::from),
                    config_contents,
                })
            })
            .await;

            let event = match result {
                Ok(completion) => crate::hub::events::HubEvent::CommandGateCompleted {
                    request_id: request_id_for_task,
                    metadata,
                    context,
                    success: completion.success,
                    exit_status: completion.exit_status,
                    stdout_tail: completion.output_summary.stdout_tail,
                    stderr_tail: completion.output_summary.stderr_tail,
                    output_truncated: completion.output_summary.truncated,
                    error_kind: completion.error_kind,
                    error: completion.error,
                    duration_ms: completion.duration_ms,
                },
                Err(error) => crate::hub::events::HubEvent::CommandGateCompleted {
                    request_id: request_id_for_task,
                    metadata,
                    context,
                    success: false,
                    exit_status: None,
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    output_truncated: false,
                    error_kind: Some("task_failed".to_string()),
                    error: Some(format!("Command gate task failed: {error}")),
                    duration_ms: 0,
                },
            };
            let _ = event_tx.send(event);
        });
    }

    pub(super) fn handle_lua_connection_request_event(
        &mut self,
        request: crate::lua::primitives::ConnectionRequest,
    ) {
        use crate::lua::primitives::ConnectionRequest;

        match request {
            ConnectionRequest::Generate => {
                log::debug!("[Lua] Processing connection.generate() request");
                match self.generate_connection_url() {
                    Ok(ref url) => {
                        if let Err(e) = self.lua.fire_connection_code_ready(url) {
                            log::error!("Failed to fire connection_code_ready: {e}");
                        }
                    }
                    Err(ref e) => {
                        log::warn!("Connection URL generation failed: {e}");
                        if let Err(fire_err) = self.lua.fire_connection_code_error(e) {
                            log::error!("Failed to fire connection_code_error: {fire_err}");
                        }
                    }
                }
            }
            ConnectionRequest::Regenerate => {
                log::info!("[Lua] Processing connection.regenerate() request");
                actions::dispatch(self, HubAction::RegenerateConnectionCode);
            }
            ConnectionRequest::CopyToClipboard => {
                log::debug!("[Lua] Processing connection.copy_to_clipboard() request");
                actions::dispatch(self, HubAction::CopyConnectionUrl);
            }
        }
    }

    pub(super) fn handle_lua_worktree_request_event(
        &mut self,
        request: crate::lua::primitives::WorktreeRequest,
    ) {
        use crate::git::WorktreeManager;
        use crate::lua::primitives::{WorktreeCreateResult, WorktreeRequest};

        match request {
            WorktreeRequest::Create {
                label,
                branch,
                repo_root,
                metadata,
                prompt,
                agent_name,
                client_rows,
                client_cols,
            } => {
                log::info!(
                    "[Lua] Dispatching async worktree.create({}) for {}",
                    branch,
                    label
                );
                let worktree_base = self.config.worktree_base.clone();
                let result_tx = self.worktree_result_tx.clone();
                let branch_clone = branch.clone();
                let label_clone = label.clone();
                let repo_root_clone = repo_root.clone();

                self.tokio_runtime.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let manager = WorktreeManager::new(worktree_base);
                        if let Some(repo_root) = repo_root_clone {
                            let repo_path = Path::new(&repo_root);
                            if crate::lua::primitives::worktree::branch_is_repo_head(
                                repo_path,
                                &branch_clone,
                            ) {
                                Ok(repo_path.to_path_buf())
                            } else if let Some(path) =
                                manager.find_worktree_for_branch(repo_path, &branch_clone)?
                            {
                                Ok(path)
                            } else {
                                manager.create_worktree_for_repo_root(repo_path, &branch_clone)
                            }
                        } else {
                            manager.create_worktree_with_branch(&branch_clone)
                        }
                    })
                    .await;

                    let outcome = match result {
                        Ok(Ok(path)) => Ok(path),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(e) => Err(format!("spawn_blocking panicked: {e}")),
                    };

                    if result_tx
                        .try_send(WorktreeCreateResult {
                            label: label_clone,
                            branch,
                            repo_root,
                            result: outcome,
                            metadata,
                            prompt,
                            agent_name,
                            client_rows,
                            client_cols,
                        })
                        .is_err()
                    {
                        log::warn!("[Worktree] Result queue full/closed; dropping async result");
                    }
                });
            }
            WorktreeRequest::Delete { path, branch } => {
                log::info!(
                    "[Lua] Dispatching async worktree.delete({}, {})",
                    path,
                    branch
                );
                let worktree_base = self.config.worktree_base.clone();
                let event_tx = self.hub_event_tx.clone();
                let path_clone = path.clone();
                let branch_clone = branch.clone();

                self.tokio_runtime.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let manager = WorktreeManager::new(worktree_base);
                        manager.delete_worktree_by_path(Path::new(&path_clone), &branch_clone)
                    })
                    .await;

                    let outcome = match result {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(e) => Err(format!("spawn_blocking panicked: {e}")),
                    };

                    let _ = event_tx.send(crate::hub::events::HubEvent::WorktreeDeleteCompleted {
                        path,
                        branch,
                        result: outcome,
                    });
                });
            }
        }
    }

    pub(super) fn handle_worktree_delete_completed_event(
        &mut self,
        path: String,
        branch: String,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                log::info!("[Worktree] Async deletion complete: {} ({})", branch, path);
                self.handle_cache.remove_worktree_by_branch(&branch);
            }
            Err(e) => {
                log::error!("[Worktree] Async deletion failed for {}: {}", branch, e);
            }
        }
    }

    pub(super) fn handle_url_probe_ready_event(
        &mut self,
        connector_session_uuid: String,
        parent_session_uuid: String,
        url: String,
        ready: bool,
        error: Option<String>,
    ) {
        let payload = serde_json::json!({
            "connector_session_uuid": connector_session_uuid,
            "parent_session_uuid": parent_session_uuid,
            "url": url,
            "ready": ready,
            "error": error,
        });
        if let Err(e) = self.lua.fire_json_event("url_probe_ready", &payload) {
            log::error!("Failed to fire url_probe_ready: {e}");
        }
    }

    pub(super) fn handle_plugin_command_prepared_event(
        &mut self,
        request_id: String,
        command: Option<String>,
        config_path: Option<String>,
        context: serde_json::Value,
        error_kind: Option<String>,
        error: Option<String>,
    ) {
        let payload = serde_json::json!({
            "request_id": request_id,
            "command": command,
            "config_path": config_path,
            "context": context,
            "error_kind": error_kind,
            "error": error,
        });
        if let Err(e) = self
            .lua
            .fire_json_event("plugin_command_prepared", &payload)
        {
            log::error!("Failed to fire plugin_command_prepared: {e}");
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_command_gate_completed_event(
        &mut self,
        request_id: String,
        metadata: serde_json::Value,
        context: serde_json::Value,
        success: bool,
        exit_status: Option<i32>,
        stdout_tail: String,
        stderr_tail: String,
        output_truncated: bool,
        error_kind: Option<String>,
        error: Option<String>,
        duration_ms: u128,
    ) {
        let payload = serde_json::json!({
            "request_id": request_id,
            "metadata": metadata,
            "context": context,
            "success": success,
            "exit_status": exit_status,
            "stdout_tail": stdout_tail,
            "stderr_tail": stderr_tail,
            "output_truncated": output_truncated,
            "error_kind": error_kind,
            "error": error,
            "duration_ms": duration_ms,
        });
        if let Err(e) = self.lua.fire_json_event("command_gate_completed", &payload) {
            log::error!("Failed to fire command_gate_completed: {e}");
        }
    }
}
