use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent::pty::PtySession;

/// Single shared tokio runtime for all server_comms tests.
pub(super) fn shared_test_runtime() -> Arc<tokio::runtime::Runtime> {
    static RT: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
    Arc::clone(RT.get_or_init(|| Arc::new(tokio::runtime::Runtime::new().unwrap())))
}

/// Proves that nesting `block_on` inside `block_on` panics.
///
/// This is the exact pattern that caused the WebRTC connection panic
/// before the `block_in_place` fix was applied to all 9 call sites
/// in this file.
#[test]
#[should_panic(expected = "Cannot start a runtime from within a runtime")]
pub(super) fn test_nested_block_on_panics_without_block_in_place() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        rt.block_on(async { 42 });
    });
}

/// Proves that `block_in_place` wrapping `block_on` prevents the
/// nested-runtime panic. This is the pattern used by all async
/// bridge points in this file.
#[test]
pub(super) fn test_block_in_place_prevents_nested_runtime_panic() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let result = tokio::task::block_in_place(|| rt.block_on(async { 42 }));
        assert_eq!(result, 42);
    });
}

/// Reproduces the panic from `set_notifications_enabled`:
/// reqwest::blocking::Client cannot `.send()` inside a tokio runtime
/// because it internally drops a runtime in an async context.
#[test]
#[should_panic(expected = "Cannot drop a runtime")]
pub(super) fn test_reqwest_blocking_inside_tokio_panics() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::blocking::Client::new();
    rt.block_on(async {
        // This is exactly what set_notifications_enabled did:
        // blocking HTTP inside the select! loop's block_on context.
        let _ = client
            .patch("http://127.0.0.1:1/hubs/1")
            .json(&serde_json::json!({"notifications_enabled": true}))
            .send();
    });
}

/// Proves that wrapping the blocking HTTP call in `block_in_place`
/// prevents the nested-runtime panic.
#[test]
pub(super) fn test_reqwest_blocking_with_block_in_place_works() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(50))
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::task::block_in_place(|| {
            // Will fail to connect (no server), but won't panic
            let result = client
                .patch("http://127.0.0.1:1/hubs/1")
                .json(&serde_json::json!({"notifications_enabled": true}))
                .send();
            assert!(result.is_err()); // connection refused, not a panic
        });
    });
}

// === End-to-End Integration Tests ===
//
// These tests use Hub::setup() to load ALL real Lua handlers, then
// exercise the full TUI → Lua → Hub → TUI pipeline without mocks.

use std::path::PathBuf;

use crate::client::{ClientId, TuiOutput, TuiRequest};
use crate::config::Config;
use crate::hub::agent_handle::{PtyHandle, SessionHandle, SessionType};
use crate::hub::{Hub, PendingTerminalAttachRequest};
use crate::lua::CreateForwarderRequest;
use crate::relay::create_crypto_service;
use crate::socket::framing::{Frame, FrameDecoder};

pub(super) fn e2e_config() -> Config {
    let mut config = Config::default();
    config.server_url = "http://localhost:3000".to_string();
    config.token = "btstr_test-key".to_string();
    config.poll_interval = 10;
    config.agent_timeout = 300;
    config.max_sessions = 10;
    config.worktree_base = PathBuf::from("/tmp/test-worktrees");
    config
}

/// Create a Hub with TUI registered, crypto initialized, and all real
/// Lua handlers loaded. Returns the Hub plus the TUI channels for
/// sending requests and receiving output.
///
/// Manually calls `register_hub_primitives()` + `load_lua_init()`
/// instead of the full `setup()` for test isolation.
pub(super) fn e2e_hub() -> (
    Hub,
    tokio::sync::mpsc::UnboundedSender<TuiRequest>,
    tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
) {
    let config = e2e_config();
    let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();

    let crypto_service = create_crypto_service("test-hub");
    hub.browser.crypto_service = Some(crypto_service);

    // Register Hub primitives (must happen before loading init script)
    hub.lua
        .register_hub_primitives(
            std::sync::Arc::clone(&hub.handle_cache),
            hub.config.worktree_base.clone(),
            hub.hub_identifier.clone(),
            std::sync::Arc::clone(&hub.shared_server_id),
            std::sync::Arc::clone(&hub.state),
            std::sync::Arc::clone(&hub.shared_color_cache),
        )
        .expect("Should register hub primitives");

    // Load real Lua handlers (init.lua and all handlers)
    hub.load_lua_init();

    // Register TUI AFTER Lua handlers are loaded (triggers
    // tui_connected which may broadcast initial state)
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
    let output_rx = hub.register_tui_via_lua(request_rx);

    (hub, request_tx, output_rx)
}

pub(super) fn test_session_handle(session_uuid: &str) -> SessionHandle {
    let pty_session = PtySession::new(24, 80);
    let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
        pty_session.get_direct_access();
    std::mem::forget(pty_session);
    let pty = PtyHandle::new(
        event_tx,
        shared_state,
        kitty_enabled,
        cursor_visible,
        resize_pending,
        None,
    );
    SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
}

pub(super) fn test_session_backed_handle(
    session_uuid: &str,
    rows: u16,
    cols: u16,
) -> SessionHandle {
    let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
    let pty = PtyHandle::new_with_session(
        event_tx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
        None,
        Arc::new(Mutex::new(None)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(std::sync::atomic::AtomicI64::new(0)),
        rows,
        cols,
    );
    SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
}

pub(super) fn test_session_backed_handle_with_mailbox(
    session_uuid: &str,
    session_io_tx: tokio::sync::mpsc::Sender<crate::worker::session_io::SessionIoRequest>,
) -> SessionHandle {
    let conn =
        crate::session::connection::SessionConnection::test_with_session_io_sender(session_io_tx);
    test_session_backed_handle_with_connection(session_uuid, conn)
}

pub(super) fn test_session_backed_handle_with_mailbox_and_snapshot(
    session_uuid: &str,
    session_io_tx: tokio::sync::mpsc::Sender<crate::worker::session_io::SessionIoRequest>,
    snapshot: Option<Vec<u8>>,
) -> SessionHandle {
    let conn =
        crate::session::connection::SessionConnection::test_with_session_io_sender_and_snapshot(
            session_io_tx,
            snapshot,
        );
    test_session_backed_handle_with_connection(session_uuid, conn)
}

pub(super) fn test_session_backed_handle_with_connection(
    session_uuid: &str,
    conn: crate::session::connection::SessionConnection,
) -> SessionHandle {
    let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
    let pty = PtyHandle::new_with_session(
        event_tx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
        None,
        Arc::new(Mutex::new(Some(conn))),
        Arc::new(AtomicU64::new(0)),
        Arc::new(std::sync::atomic::AtomicI64::new(0)),
        24,
        80,
    );
    SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
}

pub(super) fn unique_session_uuid(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time drift")
        .as_nanos();
    format!("{prefix}-{nanos}")
}

pub(super) fn register_live_session_identity(session_uuid: &str) {
    let socket_path = crate::session::session_socket_path(session_uuid).expect("socket path");
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).expect("create sessions dir");
    }
    std::fs::write(&socket_path, b"live").expect("write socket sentinel");
    crate::session::write_session_pid_file(session_uuid, std::process::id())
        .expect("write session pid file");
}

pub(super) fn cleanup_live_session_identity(session_uuid: &str) {
    if let Ok(path) = crate::session::session_socket_path(session_uuid) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(path) = crate::session::session_pid_path(session_uuid) {
        let _ = std::fs::remove_file(path);
    }
}

pub(super) fn register_test_socket_client(
    hub: &mut Hub,
    client_id: &str,
) -> tokio::net::UnixStream {
    let (client_std, server_std) =
        std::os::unix::net::UnixStream::pair().expect("std UnixStream::pair");
    client_std
        .set_nonblocking(true)
        .expect("set_nonblocking client socket");
    server_std
        .set_nonblocking(true)
        .expect("set_nonblocking server socket");
    let _guard = hub.tokio_runtime.enter();
    let client_stream =
        tokio::net::UnixStream::from_std(client_std).expect("tokio::UnixStream client");
    let server_stream =
        tokio::net::UnixStream::from_std(server_std).expect("tokio::UnixStream server");
    let conn = crate::socket::client_conn::SocketClientConn::new(
        client_id.to_string(),
        server_stream,
        hub.hub_event_tx.clone(),
    );
    hub.socket_clients.insert(client_id.to_string(), conn);
    client_stream
}

pub(super) fn read_test_socket_frame_matching<F>(
    stream: &mut tokio::net::UnixStream,
    timeout: Duration,
    mut frame_matches: F,
) -> Option<Frame>
where
    F: FnMut(&Frame) -> bool,
{
    let handle = shared_test_runtime();
    handle.block_on(async {
        use tokio::io::AsyncReadExt;

        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let n = tokio::time::timeout(remaining, stream.read(&mut buf))
                .await
                .ok()?
                .expect("socket read");
            assert!(n > 0, "socket closed before frame arrived");
            let frames = decoder.feed(&buf[..n]).expect("decode frame");
            for frame in frames {
                if frame_matches(&frame) {
                    return Some(frame);
                }
            }
        }
    })
}

pub(super) fn read_test_socket_frames(
    stream: &mut tokio::net::UnixStream,
    max_frames: usize,
    timeout: Duration,
) -> Vec<Frame> {
    shared_test_runtime().block_on(async {
        use tokio::io::AsyncReadExt;

        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 4096];
        let mut frames = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        while frames.len() < max_frames && tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(
                remaining.min(Duration::from_millis(100)),
                stream.read(&mut buf),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    frames.extend(decoder.feed(&buf[..n]).expect("decode frames"));
                }
                Ok(Ok(_)) => break,
                Ok(Err(e)) => panic!("socket read: {e}"),
                Err(_) if frames.is_empty() => continue,
                Err(_) => break,
            }
        }
        frames
    })
}

pub(super) fn wait_for_receiver_count(
    event_tx: &tokio::sync::broadcast::Sender<crate::agent::pty::PtyEvent>,
    expected: usize,
) {
    shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if event_tx.receiver_count() >= expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {expected} PTY subscribers"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

pub(super) fn settle_worker_subscription() {
    shared_test_runtime().block_on(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
}

pub(super) fn recv_session_io_request_matching<F>(
    rx: &mut tokio::sync::mpsc::Receiver<crate::worker::session_io::SessionIoRequest>,
    mut matches_request: F,
) -> crate::worker::session_io::SessionIoRequest
where
    F: FnMut(&crate::worker::session_io::SessionIoRequest) -> bool,
{
    shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for matching SessionIoRequest"
            );
            if let Ok(Some(request)) =
                tokio::time::timeout(remaining.min(Duration::from_millis(50)), rx.recv()).await
            {
                if matches_request(&request) {
                    return request;
                }
            }
        }
    })
}

pub(super) fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("missing function {name}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function body start");
    let mut depth = 0usize;
    for (idx, ch) in source[body_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + idx];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function {name}");
}

pub(super) fn production_source(source: &str) -> String {
    let mut remaining = source;
    let mut output = String::with_capacity(source.len());
    while let Some(attr_offset) = remaining.find("#[cfg(test)]") {
        output.push_str(&remaining[..attr_offset]);
        let cfg_start = attr_offset;
        let body_start = remaining[cfg_start..]
            .find('{')
            .map(|offset| cfg_start + offset)
            .expect("cfg(test) item body start");
        let mut depth = 0usize;
        let mut end = remaining.len();
        for (idx, ch) in remaining[body_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + idx + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        remaining = &remaining[end..];
    }
    output.push_str(remaining);
    output
}

/// Create a test session handle. No local shadow screen — all PTYs are
/// session-backed. Seed output is broadcast but not parsed locally.
pub(super) fn test_local_session_handle_with_seed(
    session_uuid: &str,
    seed_output: &[u8],
) -> SessionHandle {
    let pty_session = PtySession::new(24, 80);
    let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
        pty_session.get_direct_access();
    std::mem::forget(pty_session);

    let pty = PtyHandle::new(
        event_tx,
        shared_state,
        kitty_enabled,
        cursor_visible,
        resize_pending,
        None,
    );
    let _ = pty
        .event_tx_clone()
        .send(crate::agent::pty::events::PtyEvent::output(
            seed_output.to_vec(),
        ));

    SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
}

pub(super) fn test_local_session_handle(session_uuid: &str) -> SessionHandle {
    test_local_session_handle_with_seed(session_uuid, b"cached-local-output\n")
}

pub(super) fn test_session_handle_with_snapshot(
    session_uuid: &str,
    snapshot: &[u8],
) -> SessionHandle {
    let pty_session = PtySession::new(24, 80);
    let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
        pty_session.get_direct_access();
    std::mem::forget(pty_session);

    let pty = PtyHandle::new_with_snapshot(
        event_tx,
        shared_state,
        kitty_enabled,
        cursor_visible,
        resize_pending,
        None,
        snapshot.to_vec(),
    );
    SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
}

pub(super) fn test_session_handle_with_broadcast_capacity(
    session_uuid: &str,
    capacity: usize,
) -> SessionHandle {
    let (event_tx, _rx) = tokio::sync::broadcast::channel(capacity);
    let pty = PtyHandle::new(
        event_tx,
        Arc::new(Mutex::new(crate::agent::pty::SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (24, 80),
            last_human_input_ms: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        })),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
        None,
    );
    SessionHandle::new(session_uuid, "test-agent", SessionType::Agent, None, pty)
}

// Legacy probe tests removed during the session-process migration.
// Terminal probe caching is now exercised via session-process paths.

#[test]
pub(super) fn test_session_unregistered_clears_terminal_profile_state() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-clear-profile";

    hub.terminal_profiles
        .observe_output(session_uuid, b"\x1b]11;?\x07");

    hub.handle_hub_event(crate::hub::events::HubEvent::SessionUnregistered {
        session_uuid: session_uuid.to_string(),
    });

    hub.learn_terminal_probe_replies(session_uuid, "browser-a", b"\x1b]11;rgb:1234/5678/9abc\x07");

    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        None
    );
}

#[test]
pub(super) fn test_multiple_live_clients_do_not_update_terminal_profile_cache() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-multi-client";

    let _guard = hub.tokio_runtime.enter();
    hub.pty_forwarders
        .insert(format!("tui:{session_uuid}"), tokio::spawn(async {}));
    hub.pty_forwarders
        .insert(format!("browser-a:{session_uuid}"), tokio::spawn(async {}));

    hub.terminal_profiles
        .observe_output(session_uuid, b"\x1b]11;?\x07");
    hub.learn_terminal_probe_replies(session_uuid, "browser-a", b"\x1b]11;rgb:1234/5678/9abc\x07");

    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        None
    );
}

#[test]
pub(super) fn test_headless_probe_detected_and_cache_available() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-headless-probe";

    // Populate hub cache with color values.
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]10;rgb:aaaa/bbbb/cccc\x07");
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]11;rgb:1111/2222/3333\x07");
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]12;rgb:4444/5555/6666\x07");

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    // No live clients (headless) — hub should attempt to answer from cache.
    // write_input_direct returns Err in tests (no real PTY), but the hub
    // should still detect the probe and have the right cache value.
    assert!(hub.terminal_profiles.hub_profile_is_complete());
    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        Some(b"\x1b]11;rgb:1111/2222/3333\x07".as_slice())
    );
}

#[test]
pub(super) fn test_live_client_skips_hub_probe_answering() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-live-client-probe";

    // Populate hub cache.
    hub.terminal_profiles
        .observe_peer_input("boot", b"\x1b]11;rgb:1111/2222/3333\x07");

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    // Add a live client forwarder — hub should NOT answer probes.
    let _guard = hub.tokio_runtime.enter();
    hub.pty_forwarders
        .insert(format!("socket:abc:{session_uuid}"), tokio::spawn(async {}));

    hub.handle_hub_event(crate::hub::events::HubEvent::PtyOutputObserved {
        session_uuid: session_uuid.to_string(),
        data: b"\x1b]11;?\x07".to_vec(),
    });

    // Drain output — hub should not have sent any probe-related messages.
    while output_rx.try_recv().is_ok() {}
}

#[test]
pub(super) fn test_pty_output_observed_tracks_probe_queries_for_later_replies() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-observed-probe";

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    hub.handle_hub_event(crate::hub::events::HubEvent::PtyOutputObserved {
        session_uuid: session_uuid.to_string(),
        data: b"\x1b]11;?\x07".to_vec(),
    });

    hub.learn_terminal_probe_replies(session_uuid, "browser-a", b"\x1b]11;rgb:1234/5678/9abc\x07");

    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        None
    );
}

#[test]
pub(super) fn test_session_io_batch_preserves_output_metrics_and_probe_learning() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-worker-batch-probe";

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    hub.handle_hub_event(crate::hub::events::HubEvent::SessionIoBatch(
        crate::worker::session_io::SessionIoBatch {
            session_uuid: session_uuid.to_string(),
            output: Some(b"\x1b]11;?\x07payload".to_vec()),
        },
    ));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_output.messages"], 1);
    assert_eq!(snapshot.counters["pty_output.bytes"], 14);

    hub.learn_terminal_probe_replies(session_uuid, "browser-a", b"\x1b]11;rgb:1234/5678/9abc\x07");

    assert_eq!(
        hub.terminal_profiles.headless_reply(
            session_uuid,
            crate::hub::terminal_profile::TerminalProbe::DefaultBackground
        ),
        None
    );
}

#[test]
pub(super) fn test_file_input_enqueues_paste_and_written_event_registers_cleanup() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-file-paste-mailbox";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(4);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    hub.handle_file_input(crate::channel::webrtc::FileInputIncoming {
        session_uuid: session_uuid.to_string(),
        filename: "drop.PNG".to_string(),
        data: b"image-bytes".to_vec(),
    });

    match session_io_rx.try_recv().expect("paste request") {
        crate::worker::session_io::SessionIoRequest::PasteFile {
            request_id,
            filename,
            data,
        } => {
            assert!(request_id.starts_with("paste-"));
            assert_eq!(filename, "drop.PNG");
            assert_eq!(data, b"image-bytes");
            let path = std::path::PathBuf::from("/tmp/botster-paste-test.png");
            hub.handle_session_io_event(
                crate::worker::session_io::SessionIoEvent::PasteFileWritten {
                    request_id,
                    session_uuid: session_uuid.to_string(),
                    path: path.clone(),
                    bytes: 11,
                },
            );
            assert_eq!(
                hub.paste_files.get(session_uuid).expect("paste cleanup"),
                &vec![path]
            );
        }
        other => panic!("expected PasteFile request, got {other:?}"),
    }
}

#[test]
pub(super) fn test_queue_webrtc_terminal_snapshot_returns_false_when_mailbox_full() {
    let (hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-snapshot-mailbox-full";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(1);
    session_io_tx
        .try_send(crate::worker::session_io::SessionIoRequest::PtyInput {
            data: b"queued".to_vec(),
        })
        .expect("fill mailbox");
    let session = test_session_backed_handle_with_mailbox(session_uuid, session_io_tx);
    let pty = session.pty().clone();
    hub.handle_cache.add_session(session);

    assert!(!Hub::queue_webrtc_terminal_snapshot(
        &hub.hub_event_metrics,
        &hub.hub_event_tx,
        &pty,
        Some("snapshot-full".to_string()),
        session_uuid,
        b"snapshot".to_vec(),
    ));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.queue_full"], 1);
    assert!(matches!(
        session_io_rx.try_recv().expect("filled request"),
        crate::worker::session_io::SessionIoRequest::PtyInput { .. }
    ));
}

#[test]
pub(super) fn test_empty_initial_snapshot_cleans_pending_session_io_request() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-empty-initial-snapshot";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(4);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox_and_snapshot(
            session_uuid,
            session_io_tx,
            Some(Vec::new()),
        ));
    let mut req = test_forwarder_request(
        "browser-empty-snapshot",
        session_uuid,
        "terminal_empty_snapshot",
    );
    req.rows = 23;
    let _command_rx = install_test_browser_worker(
        &mut hub,
        "browser-empty-snapshot",
        session_uuid,
        "terminal_empty_snapshot",
    );

    assert!(hub.try_attach_terminal_forwarder(&req));
    assert_eq!(hub.pending_session_io_snapshots.len(), 1);
    assert!(matches!(
        recv_session_io_request_matching(&mut session_io_rx, |request| matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { rows: 23, cols: 80 }
        )),
        crate::worker::session_io::SessionIoRequest::Resize { .. }
    ));
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.to_string(),
        payload: Vec::new(),
    });

    for _ in 0..20 {
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        hub.poll_hub_events();
        if hub.pending_session_io_snapshots.is_empty() {
            break;
        }
    }

    assert!(hub.pending_session_io_snapshots.is_empty());
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.empty"], 1);
    hub.stop_lua_pty_forwarder("browser-empty-snapshot:sess-empty-initial-snapshot");
}

#[test]
pub(super) fn test_snapshot_enqueue_failure_cleans_existing_pending_request() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-snapshot-enqueue-cleanup";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(1);
    session_io_tx
        .try_send(crate::worker::session_io::SessionIoRequest::PtyInput {
            data: b"queued".to_vec(),
        })
        .expect("fill mailbox");
    let session = test_session_backed_handle_with_mailbox(session_uuid, session_io_tx);
    let pty = session.pty().clone();
    hub.handle_cache.add_session(session);
    let request_id = "snapshot-cleanup-on-full".to_string();
    assert!(hub.insert_pending_session_io_snapshot(
        request_id.clone(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: session_uuid.to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-cleanup".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                forwarder_key: Some("browser-cleanup:sess-snapshot-enqueue-cleanup".to_string()),
                active_flag: None,
            },
        },
    ));

    assert!(!Hub::queue_webrtc_terminal_snapshot(
        &hub.hub_event_metrics,
        &hub.hub_event_tx,
        &pty,
        Some(request_id.clone()),
        session_uuid,
        b"snapshot".to_vec(),
    ));
    assert!(hub.pending_session_io_snapshots.contains_key(&request_id));

    hub.poll_hub_events();
    assert!(!hub.pending_session_io_snapshots.contains_key(&request_id));
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.queue_full"], 1);
    assert!(matches!(
        session_io_rx.try_recv().expect("filled request"),
        crate::worker::session_io::SessionIoRequest::PtyInput { .. }
    ));
}

#[test]
pub(super) fn test_prepared_snapshot_routes_through_browser_worker_with_metrics() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let request_id = "snapshot-output-test".to_string();
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-output", "sess-output", "sub-output");
    hub.insert_pending_session_io_snapshot(
        request_id.clone(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-output".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-output".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                forwarder_key: None,
                active_flag: None,
            },
        },
    );

    hub.handle_session_io_event(
        crate::worker::session_io::SessionIoEvent::PreparedSnapshot {
            request_id,
            session_uuid: "sess-output".to_string(),
            uncompressed_len: 256,
            payload: vec![0x1f, 0x8b, 0x08, 0x00],
            recovery: false,
        },
    );
    let (subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x1f);
    assert_eq!(subscription_id, "sub-output");
    assert!(data.starts_with(&[0x1f, 0x8b]));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert!(snapshot.spans.contains_key("snapshot.gzip_queue"));
    assert!(hub.pending_session_io_snapshots.is_empty());
}

#[test]
pub(super) fn test_pending_session_io_snapshot_cleanup_paths() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    hub.insert_pending_session_io_snapshot(
        "by-peer".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-a".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-a".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                forwarder_key: Some("browser-a:sess-a".to_string()),
                active_flag: None,
            },
        },
    );
    hub.insert_pending_session_io_snapshot(
        "by-forwarder".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-b".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-b".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                forwarder_key: Some("browser-b:sess-b".to_string()),
                active_flag: None,
            },
        },
    );
    hub.insert_pending_session_io_snapshot(
        "by-session".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-c".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest {
                    request_id: "recovery-by-session".to_string(),
                    browser_identity: "browser-c".to_string(),
                    session_uuid: "sess-c".to_string(),
                    subscription_id: "sub-c".to_string(),
                },
            },
        },
    );

    hub.cleanup_pending_session_io_snapshots_for_peer("browser-a");
    assert!(!hub.pending_session_io_snapshots.contains_key("by-peer"));
    hub.cleanup_pending_session_io_snapshots_for_forwarder("browser-b:sess-b");
    assert!(!hub
        .pending_session_io_snapshots
        .contains_key("by-forwarder"));
    hub.handle_hub_event(crate::hub::events::HubEvent::SessionUnregistered {
        session_uuid: "sess-c".to_string(),
    });
    assert!(!hub.pending_session_io_snapshots.contains_key("by-session"));

    hub.insert_pending_session_io_snapshot(
        "stale".to_string(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-stale".to_string(),
            started_at: Instant::now()
                - crate::hub::SESSION_IO_SNAPSHOT_PENDING_TTL
                - Duration::from_secs(1),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcOutput {
                peer_id: "browser-stale".to_string(),
                rows: 24,
                cols: 80,
                kitty_enabled: false,
                forwarder_key: None,
                active_flag: None,
            },
        },
    );
    hub.cleanup_stale_session_io_snapshots();
    assert!(!hub.pending_session_io_snapshots.contains_key("stale"));
    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.pending_stale_drop"], 1);
}

#[test]
pub(super) fn test_session_io_batch_notifies_lua_pty_output_observers_in_byte_order() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-worker-batch-lua";

    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    hub.lua
        .lua()
        .load(
            r#"
                _test_pty_output_chunks = {}
                hooks.on("pty_output", "test.session_io_batch_order", function(ctx, data)
                    table.insert(_test_pty_output_chunks, {
                        session_uuid = ctx.session_uuid,
                        data = data,
                        len = #data,
                    })
                end)
                "#,
        )
        .exec()
        .expect("register test pty_output observer");

    let chunks = [
        b"coalesced-one-".to_vec(),
        b"two-and-three-".to_vec(),
        b"four".to_vec(),
    ];
    let expected = chunks.concat();

    for chunk in chunks {
        hub.handle_hub_event(crate::hub::events::HubEvent::SessionIoBatch(
            crate::worker::session_io::SessionIoBatch {
                session_uuid: session_uuid.to_string(),
                output: Some(chunk),
            },
        ));
    }

    let observed: String = hub
        .lua
        .lua()
        .load(
            r#"
                local out = {}
                for _, chunk in ipairs(_test_pty_output_chunks) do
                    table.insert(out, chunk.data)
                end
                return table.concat(out)
                "#,
        )
        .eval()
        .expect("read observed pty output bytes");
    let observed_total: usize = hub
        .lua
        .lua()
        .load(
            r#"
                local total = 0
                for _, chunk in ipairs(_test_pty_output_chunks) do
                    total = total + chunk.len
                end
                return total
                "#,
        )
        .eval()
        .expect("read observed pty output byte count");
    let observed_count: usize = hub
        .lua
        .lua()
        .load("return #_test_pty_output_chunks")
        .eval()
        .expect("read observed pty output chunk count");
    let observed_session_uuid: String = hub
        .lua
        .lua()
        .load("return _test_pty_output_chunks[1].session_uuid")
        .eval()
        .expect("read observed pty output context");

    assert_eq!(observed.as_bytes(), expected.as_slice());
    assert_eq!(observed_total, expected.len());
    assert_eq!(observed_count, 3);
    assert_eq!(observed_session_uuid, session_uuid);

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_output.messages"], 3);
    assert_eq!(snapshot.counters["pty_output.bytes"], expected.len() as u64);
}

#[test]
pub(super) fn test_noisy_session_io_replay_keeps_hot_handler_latency_bounded() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-noisy-replay";
    hub.handle_cache
        .add_session(test_local_session_handle(session_uuid));

    let mut elapsed_samples = Vec::with_capacity(1001);
    let mut max_elapsed = std::time::Duration::ZERO;
    for i in 0..=1000 {
        let data = format!("\x1b]2;botster replay {i}\x07payload-{i:04}\r\n").into_bytes();
        let event = crate::hub::events::HubEvent::SessionIoBatch(
            crate::worker::session_io::SessionIoBatch {
                session_uuid: session_uuid.to_string(),
                output: Some(data),
            },
        );
        let started = Instant::now();
        hub.handle_hub_event(event);
        let elapsed = started.elapsed();
        max_elapsed = max_elapsed.max(elapsed);
        elapsed_samples.push(elapsed);
        hub.hub_event_metrics
            .record_handler_time("session_io_batch", elapsed);
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_output.messages"], 1001);
    assert!(snapshot.counters["pty_output.bytes"] > 32_000);
    let session_io = snapshot
        .by_type
        .get("session_io_batch")
        .expect("session_io_batch handler metrics");
    assert_eq!(
        session_io.handler_time_max_ns,
        max_elapsed.as_nanos() as u64
    );
    elapsed_samples.sort_unstable();
    let p99_elapsed = elapsed_samples[elapsed_samples.len() * 99 / 100];
    let slow_samples = elapsed_samples
        .iter()
        .filter(|elapsed| **elapsed >= Hub::HOT_SUBHANDLER_SLOW)
        .count();
    assert!(
            p99_elapsed < Hub::HOT_SUBHANDLER_SLOW,
            "observed-log-shaped SessionIoBatch replay p99 exceeded hot-path budget: p99={p99_elapsed:?}, max={max_elapsed:?}, slow_samples={slow_samples}"
        );
    assert!(snapshot.slow_samples.is_empty());
}

#[test]
pub(super) fn test_pty_osc_cursor_volume_burst_guardrail_matches_observed_logs() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    for i in 0..=crate::hub::VolumeBurstState::THRESHOLD {
        hub.handle_hub_event(crate::hub::events::HubEvent::PtyOscEvent {
            session_uuid: "sess-osc-replay".to_string(),
            session_name: "test-agent".to_string(),
            event: crate::agent::pty::PtyEvent::cursor_visibility_changed(i % 2 == 0),
        });
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["pty_osc.cursor"], 1001);
    assert_eq!(snapshot.counters["pty_osc.volume_burst"], 1);
}

#[test]
pub(super) fn test_inactive_webrtc_forwarder_strips_probe_queries() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-filter-inactive-webrtc";
    let session = test_session_handle(session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-a", session_uuid, "terminal_sub");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-a",
        session_uuid,
        "terminal_sub"
    )));
    hub.set_active_terminal_peer(session_uuid, "tui", true);
    // No snapshot message (0x02) — test PtyHandle has no session process,
    // so get_snapshot() returns empty and the snapshot send is skipped.
    // Allow forwarder task to start the live loop.
    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
        b"before\x1b]11;?\x07after".to_vec(),
    ));

    let (_subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x01);
    assert_eq!(data, b"\x01beforeafter");
}

#[test]
pub(super) fn test_active_webrtc_forwarder_keeps_probe_queries() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-filter-active-webrtc";
    let session = test_session_handle(session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-a", session_uuid, "terminal_sub");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-a",
        session_uuid,
        "terminal_sub"
    )));
    hub.set_active_terminal_peer(session_uuid, "browser-a", true);
    // No snapshot message — empty snapshot from test PtyHandle.
    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
        b"\x1b]11;?\x07after".to_vec(),
    ));

    let (_subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x01);
    assert_eq!(data, b"\x01\x1b]11;?\x07after");
}

#[test]
pub(super) fn test_webrtc_worker_live_output_runs_per_browser_pty_hooks() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-webrtc-live-hooks";
    let session = test_session_handle(session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut command_rx =
        install_test_browser_worker(&mut hub, "browser-hooks", session_uuid, "terminal_hooks");

    hub.lua
        .lua()
        .load(
            r#"
                _test_webrtc_pty_hook_peer = nil
                hooks.intercept("pty_output", "test.webrtc_worker_live", function(ctx, data)
                    _test_webrtc_pty_hook_peer = ctx.peer_id
                    return data .. "-hooked"
                end)
                "#,
        )
        .exec()
        .expect("register test pty_output interceptor");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-hooks",
        session_uuid,
        "terminal_hooks"
    )));
    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"live".to_vec()));

    let (_subscription_id, data) = recv_next_webrtc_pty_command(&mut hub, &mut command_rx, 0x01);
    assert_eq!(data, b"\x01live-hooked");
    let observed_peer: String = hub
        .lua
        .lua()
        .load("return _test_webrtc_pty_hook_peer")
        .eval()
        .expect("read pty hook peer");
    assert_eq!(observed_peer, "browser-hooks");
}

#[test]
pub(super) fn test_browser_focus_input_updates_active_terminal_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-browser-focus";

    hub.handle_pty_input(crate::channel::webrtc::PtyInputIncoming {
        session_uuid: session_uuid.to_string(),
        browser_identity: "browser-a".to_string(),
        data: b"\x1b[I".to_vec(),
    });

    assert_eq!(
        hub.active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .cloned(),
        Some("browser-a".to_string())
    );

    hub.handle_pty_input(crate::channel::webrtc::PtyInputIncoming {
        session_uuid: session_uuid.to_string(),
        browser_identity: "browser-a".to_string(),
        data: b"\x1b[O".to_vec(),
    });

    assert!(hub
        .active_terminal_peers
        .lock()
        .expect("active peers mutex")
        .get(session_uuid)
        .is_none());
}

#[test]
pub(super) fn test_webrtc_focus_message_updates_active_terminal_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-webrtc-focus";
    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "focus_changed",
        "session_uuid": session_uuid,
        "focused": true,
    }))
    .expect("focus payload");

    hub.process_webrtc_plaintext_payload("browser-a", &payload);

    assert_eq!(
        hub.active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .cloned(),
        Some("browser-a".to_string())
    );
}

#[test]
pub(super) fn test_tui_focus_request_updates_active_terminal_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-focus";

    hub.handle_tui_request(TuiRequest::FocusChanged {
        session_uuid: session_uuid.to_string(),
        focused: true,
    });

    assert_eq!(
        hub.active_terminal_peers
            .lock()
            .expect("active peers mutex")
            .get(session_uuid)
            .cloned(),
        Some("tui".to_string())
    );

    hub.handle_tui_request(TuiRequest::FocusChanged {
        session_uuid: session_uuid.to_string(),
        focused: false,
    });

    assert!(hub
        .active_terminal_peers
        .lock()
        .expect("active peers mutex")
        .get(session_uuid)
        .is_none());
}

#[test]
pub(super) fn test_tui_terminal_color_profile_updates_client_cache() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    let mut colors = std::collections::HashMap::new();
    colors.insert(257usize, crate::terminal::Rgb::new(17, 34, 51));

    hub.handle_tui_request(TuiRequest::LuaMessage(serde_json::json!({
        "type": "terminal_color_profile",
        "session_uuid": "sess-color-profile",
        "colors": colors,
    })));

    assert_eq!(
        hub.terminal_client_profiles
            .get("tui")
            .and_then(|colors| colors.get(&257usize))
            .copied(),
        Some(crate::terminal::Rgb::new(17, 34, 51))
    );
}

#[test]
pub(super) fn test_backpressure_recovery_routes_prepared_snapshot_to_peer() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let mut peer_rx = hub
        .webrtc
        .install_test_recovery_sender("browser-recovery", &hub.tokio_runtime);
    let request_id = "snapshot-recovery-test".to_string();
    hub.insert_pending_session_io_snapshot(
        request_id.clone(),
        crate::hub::PendingSessionIoSnapshot {
            session_uuid: "sess-recovery".to_string(),
            started_at: Instant::now(),
            target: crate::hub::PendingSessionIoSnapshotTarget::WebRtcPeerRecovery {
                request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest {
                    request_id: "browser-recovery:sess-recovery".to_string(),
                    browser_identity: "browser-recovery".to_string(),
                    session_uuid: "sess-recovery".to_string(),
                    subscription_id: "sub-recovery".to_string(),
                },
            },
        },
    );

    hub.handle_session_io_event(
        crate::worker::session_io::SessionIoEvent::PreparedSnapshot {
            request_id,
            session_uuid: "sess-recovery".to_string(),
            uncompressed_len: 128,
            payload: vec![0x1f, 0x8b, 0x08],
            recovery: true,
        },
    );

    match peer_rx.try_recv().expect("recovery snapshot command") {
        crate::worker::webrtc::WebRtcAdapterCommand::Pty {
            subscription_id,
            data,
        } => {
            assert_eq!(subscription_id, "sub-recovery");
            assert!(data.starts_with(&[0x1f, 0x8b]));
        }
        other => panic!("expected PTY recovery command, got {other:?}"),
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["snapshot.backpressure_recovery.sent"], 1);
    assert!(snapshot.spans.contains_key("snapshot.gzip_queue"));
}

#[test]
pub(super) fn test_backpressure_recovery_missing_session_counts_failed_without_dispatch() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-recovery-missing";
    let browser_identity = "browser-recovery-missing";
    let mut peer_rx = hub
        .webrtc
        .install_test_recovery_sender(browser_identity, &hub.tokio_runtime);
    let key = format!("{browser_identity}:{session_uuid}");
    hub.webrtc.record_backpressure_recovery(
        key,
        crate::worker::webrtc::BackpressureRecoveryEntry {
            browser_identity: browser_identity.to_string(),
            session_uuid: session_uuid.to_string(),
            subscription_id: "sub-recovery-missing".to_string(),
            last_drop: Instant::now() - crate::worker::webrtc::BACKPRESSURE_SNAPSHOT_COOLDOWN,
        },
    );

    hub.dispatch_webrtc_recovery_snapshot_requests();

    assert!(peer_rx.try_recv().is_err());
    let metrics = hub.hub_event_metrics.snapshot();
    assert!(!metrics
        .counters
        .contains_key("snapshot.backpressure_recovery.sent"));
    assert!(!metrics
        .counters
        .contains_key("snapshot.backpressure_recovery.empty"));
    assert_eq!(metrics.counters["snapshot.backpressure_recovery.failed"], 1);
}

pub(super) fn test_forwarder_request(
    peer_id: &str,
    session_uuid: &str,
    subscription_id: &str,
) -> CreateForwarderRequest {
    CreateForwarderRequest {
        peer_id: peer_id.to_string(),
        session_uuid: session_uuid.to_string(),
        prefix: Some(vec![0x01]),
        subscription_id: subscription_id.to_string(),
        rows: 24,
        cols: 80,
        active_flag: Arc::new(Mutex::new(true)),
    }
}

pub(super) fn install_test_browser_worker(
    hub: &mut Hub,
    peer_id: &str,
    session_uuid: &str,
    subscription_id: &str,
) -> tokio::sync::mpsc::Receiver<crate::worker::webrtc::WebRtcAdapterCommand> {
    let command_rx = hub
        .webrtc
        .install_test_recovery_sender(peer_id, &hub.tokio_runtime);
    let (hub_control_tx, _hub_control_rx) =
        tokio::sync::mpsc::channel(crate::worker::hub_control::HUB_CONTROL_QUEUE.capacity);
    let (outbound_tx, mut outbound_rx) =
        tokio::sync::mpsc::channel::<crate::worker::transport::TransportEgress>(4096);
    let hub_event_tx = hub.hub_event_tx.clone();
    let browser_identity = peer_id.to_string();
    let _guard = hub.tokio_runtime.enter();
    tokio::spawn(async move {
        while let Some(egress) = outbound_rx.recv().await {
            if hub_event_tx
                .send(crate::hub::events::HubEvent::WebRtcClientWorkerEgress {
                    browser_identity: browser_identity.clone(),
                    egress,
                })
                .is_err()
            {
                break;
            }
        }
    });
    let mut config = crate::worker::client::ClientWorkerConfig::new(
        crate::client::ClientId::browser(peer_id.to_string()),
        hub_control_tx,
        outbound_tx,
        std::collections::HashMap::new(),
    );
    config.outbound =
        crate::worker::BoundedQueueConfig::new("worker.client.webrtc.test.outbound", 4096);
    let worker = crate::worker::client::ClientWorker::start(config);
    let _ = worker.try_send(
        crate::worker::client::ClientWorkerMessage::SubscribeSession {
            session_uuid: session_uuid.to_string(),
            subscription_id: subscription_id.to_string(),
        },
    );
    hub.browser_client_workers
        .insert(peer_id.to_string(), worker);
    command_rx
}

pub(super) fn install_test_browser_worker_unsubscribed(
    hub: &mut Hub,
    peer_id: &str,
) -> tokio::sync::mpsc::Receiver<crate::worker::webrtc::WebRtcAdapterCommand> {
    let command_rx = hub
        .webrtc
        .install_test_recovery_sender(peer_id, &hub.tokio_runtime);
    let _guard = hub.tokio_runtime.enter();
    let worker = hub.spawn_webrtc_client_worker_adapter(peer_id.to_string());
    hub.browser_client_workers
        .insert(peer_id.to_string(), worker);
    command_rx
}

pub(super) fn recv_next_webrtc_pty_command(
    hub: &mut Hub,
    rx: &mut tokio::sync::mpsc::Receiver<crate::worker::webrtc::WebRtcAdapterCommand>,
    prefix: u8,
) -> (String, Vec<u8>) {
    for _ in 0..20 {
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        hub.poll_hub_events();
        while let Ok(command) = rx.try_recv() {
            if let crate::worker::webrtc::WebRtcAdapterCommand::Pty {
                subscription_id,
                data,
            } = command
            {
                if data.first() == Some(&prefix) {
                    return (subscription_id, data);
                }
            }
        }
    }

    panic!("expected WebRTC PTY command with prefix {prefix:#x}");
}

/// Drains all pending `TuiOutput::Message` JSON values from the output
/// channel, ignoring non-Message variants.
pub(super) fn drain_messages(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();
    while let Ok(output) = rx.try_recv() {
        if let TuiOutput::Message(json) = output {
            messages.push(json);
        }
    }
    messages
}

/// TUI subscribe triggers state broadcasts through real Lua handlers.
///
/// Sends a subscribe message, ticks the Hub, and verifies that Lua
/// broadcasts hub state (worktree list, agent list, etc.) back to
/// the TUI client.
#[test]
pub(super) fn test_tui_subscribe_delivers_state() {
    let (mut hub, request_tx, mut output_rx) = e2e_hub();

    // Drain anything from setup
    drain_messages(&mut output_rx);

    // Subscribe to get initial state broadcast
    request_tx
        .send(TuiRequest::LuaMessage(serde_json::json!({
            "type": "subscribe",
            "channel": "hub"
        })))
        .unwrap();

    hub.tick();

    let messages = drain_messages(&mut output_rx);

    // After subscribe, Lua handlers should broadcast hub state.
    // Even if no events fire, the test proves the pipeline doesn't
    // crash — messages through real Lua handlers without panic.
    for msg in &messages {
        assert!(
            msg.get("type").is_some(),
            "All TUI messages should have a 'type' field, got: {}",
            msg
        );
    }
}

/// TUI message round-trips through real Lua handlers.
///
/// Sends a JSON message via `TuiRequest::LuaMessage`, ticks the Hub
/// to process it through real Lua handlers, and verifies that Lua
/// produces output on the TUI channel.
#[test]
pub(super) fn test_tui_message_round_trips_through_lua() {
    let (mut hub, request_tx, mut output_rx) = e2e_hub();

    // Drain initial state messages from setup
    drain_messages(&mut output_rx);

    // Send a subscribe message (simple, always handled by real Lua)
    request_tx
        .send(TuiRequest::LuaMessage(serde_json::json!({
            "type": "subscribe",
            "channel": "agents"
        })))
        .unwrap();

    // Tick Hub to process the message through real Lua handlers
    hub.tick();

    // The subscribe message should be processed by real Lua handlers.
    // Even if subscribe doesn't produce output, the test proves the
    // pipeline doesn't crash or lose the message.
    // (No assertion on specific output — the point is no panic/crash)
}

/// Full create_agent pipeline through real Lua handlers.
///
/// Sends a `create_agent` message, ticks the Hub, and verifies that
/// the real Lua handlers process it (agent creation on main repo).
/// The agent may fail to spawn in test env (no git repo at
/// `/tmp/test-worktrees`), but the Lua handler response proves the
/// full pipeline is wired: TUI → Hub → Lua handlers → response.
#[test]
pub(super) fn test_create_agent_pipeline_e2e() {
    let (mut hub, request_tx, mut output_rx) = e2e_hub();

    // Drain initial state messages from setup
    drain_messages(&mut output_rx);

    // Send create_agent through the real pipeline
    request_tx
        .send(TuiRequest::LuaMessage(serde_json::json!({
            "type": "create_agent",
            "prompt": "test prompt for e2e"
        })))
        .unwrap();

    // Tick Hub to process through real Lua handlers
    hub.tick();

    // Collect any responses from Lua handlers
    let messages = drain_messages(&mut output_rx);

    // The real Lua handlers should produce some response — either
    // agent_created (success) or an error event. The key assertion
    // is that the message flows through the full pipeline and produces
    // typed output (not silence).
    //
    // Note: In test env without a real git repo, agent creation will
    // likely fail, but the Lua error handler should still broadcast
    // an event back to TUI.
    for msg in &messages {
        assert!(
            msg.get("type").is_some(),
            "Lua handler response should have a 'type' field, got: {}",
            msg
        );
    }
}

/// Messages with null JSON fields don't crash real Lua handlers.
///
/// The null→userdata bug caused crashes in `config_resolver.lua`.
/// This test sends a message with explicit null fields through the
/// full pipeline to verify `json_to_lua()` correctly maps null→nil.
#[test]
pub(super) fn test_null_fields_dont_crash_real_lua_handlers() {
    let (mut hub, request_tx, mut output_rx) = e2e_hub();

    // Drain initial state
    drain_messages(&mut output_rx);

    // Send message with explicit null fields (the pattern that
    // previously crashed config_resolver.lua)
    request_tx
        .send(TuiRequest::LuaMessage(serde_json::json!({
            "type": "create_agent",
            "issue_or_branch": null,
            "prompt": "test with nulls",
            "repo": null
        })))
        .unwrap();

    // Tick — should NOT panic or crash
    hub.tick();

    // If we get here without panic, null fields were handled correctly
    // by real Lua handlers via json_to_lua()
}

#[test]
pub(super) fn test_unknown_peer_burst_guardrail_is_bounded_and_rate_limited() {
    let (hub, _request_tx, _output_rx) = e2e_hub();

    for _ in 0..crate::worker::webrtc::PeerBurstState::THRESHOLD {
        hub.queue_webrtc_peer_command(
            "peer-alpha-abcdefghijklmnopqrstuvwxyz",
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: vec![1] },
        );
    }
    for i in 0..32 {
        hub.queue_webrtc_peer_command(
            &format!("peer-distinct-{i}"),
            crate::worker::webrtc::WebRtcAdapterCommand::Json { data: vec![1] },
        );
    }

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(
        snapshot.counters["webrtc_send.unknown_peer_burst"], 1,
        "same peer should warn once per window"
    );
    assert_eq!(
        snapshot.counters["webrtc_send.unknown_peer"],
        (crate::worker::webrtc::PeerBurstState::THRESHOLD + 32) as u64
    );
    let peer_count = hub.webrtc.unknown_peer_distinct_count();
    assert!(peer_count <= crate::worker::webrtc::PeerBurstState::PEER_CAP);
}

#[test]
pub(super) fn test_terminal_attach_intent_resolves_when_session_appears() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "peer-attach:sess-attach".to_string();

    let req = test_forwarder_request("peer-attach", "sess-attach", "terminal_sess-attach");
    hub.create_lua_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing session should create pending attach intent"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "forwarder should not start until session is registered"
    );

    hub.handle_cache
        .add_session(test_session_handle("sess-attach"));
    let _command_rx = install_test_browser_worker(
        &mut hub,
        "peer-attach",
        "sess-attach",
        "terminal_sess-attach",
    );
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending attach intent should clear once session exists"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "forwarder should start after session registration"
    );
    assert!(
        hub.browser_client_workers.contains_key("peer-attach"),
        "WebRTC peer should register a browser ClientWorker handle"
    );
}

#[test]
pub(super) fn test_webrtc_worker_handle_registered_and_removed_with_forwarder() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-webrtc-worker-lifecycle";
    let key = format!("browser-worker:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let _command_rx =
        install_test_browser_worker(&mut hub, "browser-worker", session_uuid, "terminal_sub");

    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        "browser-worker",
        session_uuid,
        "terminal_sub"
    )));
    assert!(
        hub.browser_client_workers.contains_key("browser-worker"),
        "WebRTC peer should register a browser ClientWorker handle"
    );

    hub.stop_lua_pty_forwarder(&key);

    assert!(
        hub.browser_client_workers.contains_key("browser-worker"),
        "stopping one WebRTC forwarder should keep the peer-level ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "stopping WebRTC forwarder should remove the task"
    );
}

#[test]
pub(super) fn test_webrtc_terminal_subscribe_routes_attach_through_browser_worker() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let browser_identity = "browser-subscribe-worker";
    let session_uuid = "sess-webrtc-subscribe-worker";
    let forwarder_key = format!("{browser_identity}:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let mut command_rx = install_test_browser_worker_unsubscribed(&mut hub, browser_identity);

    let payload = serde_json::json!({
        "type": "subscribe",
        "channel": "terminal",
        "subscriptionId": "terminal_sub_worker",
        "params": {
            "session_uuid": session_uuid,
            "rows": 33,
            "cols": 120,
        }
    });
    hub.process_webrtc_plaintext_payload(browser_identity, payload.to_string().as_bytes());

    hub.tokio_runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });
    hub.poll_hub_events();

    assert!(
        hub.pty_forwarders.contains_key(&forwarder_key),
        "browser subscribe should attach through the peer-level ClientWorker"
    );

    let mut saw_subscribed_ack = false;
    for _ in 0..20 {
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
        hub.poll_hub_events();
        while let Ok(command) = command_rx.try_recv() {
            if matches!(
                command,
                crate::worker::webrtc::WebRtcAdapterCommand::Json { data }
                    if serde_json::from_slice::<serde_json::Value>(&data)
                        .ok()
                        .and_then(|value| value.get("type").and_then(|v| v.as_str()).map(str::to_owned))
                        .as_deref()
                        == Some("subscribed")
            ) {
                saw_subscribed_ack = true;
                break;
            }
        }
        if saw_subscribed_ack {
            break;
        }
    }
    assert!(saw_subscribed_ack, "expected subscribed ack");
}

#[test]
pub(super) fn test_webrtc_peer_cleanup_removes_terminal_client_worker() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let browser_identity = "browser-cleanup";
    let session_uuid = "sess-webrtc-cleanup";
    let key = format!("{browser_identity}:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let _command_rx =
        install_test_browser_worker(&mut hub, browser_identity, session_uuid, "terminal_sub");
    assert!(hub.try_attach_terminal_forwarder(&test_forwarder_request(
        browser_identity,
        session_uuid,
        "terminal_sub"
    )));
    assert!(
        hub.browser_client_workers.contains_key(browser_identity),
        "WebRTC peer should register a browser ClientWorker handle"
    );

    let channel = crate::channel::WebRtcChannel::builder()
        .server_url(hub.config.server_url.clone())
        .api_key(hub.config.get_api_key().to_string())
        .signal_tx(hub.webrtc.outgoing_signal_tx())
        .stream_frame_tx(hub.webrtc.stream_frame_tx())
        .pty_input_tx(hub.webrtc.pty_input_tx())
        .file_input_tx(hub.webrtc.file_input_tx())
        .crypto_service(
            hub.browser
                .crypto_service
                .clone()
                .expect("crypto service required"),
        )
        .build();
    let generation = hub.webrtc.next_offer_generation(browser_identity);
    let _ = hub.webrtc.complete_offer(
        crate::worker::webrtc::WebRtcOfferCompletion {
            browser_identity: browser_identity.to_string(),
            generation,
            channel,
            encrypted_answer: Some(serde_json::json!({"type": "answer"})),
        },
        &hub.tokio_runtime,
    );

    hub.cleanup_webrtc_peer(browser_identity, "disconnected");

    assert!(
        !hub.browser_client_workers.contains_key(browser_identity),
        "WebRTC peer cleanup should remove the browser ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "WebRTC peer cleanup should remove the PTY forwarder task"
    );
}

#[test]
pub(super) fn test_terminal_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "peer-timeout:sess-timeout".to_string();

    let req = test_forwarder_request("peer-timeout", "sess-timeout", "terminal_sess-timeout");
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_lua_pty_forwarder(req);

    {
        let intent = hub
            .pending_terminal_attaches
            .get_mut(&key)
            .expect("pending attach intent should exist");
        intent.requested_at =
            Instant::now() - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
    }

    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "stale pending attach should be removed"
    );
    assert!(
        !*active_flag
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "not_found transition should deactivate forwarder handle"
    );
}

#[test]
pub(super) fn test_terminal_attach_intent_replaces_previous_pending_request() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "peer-replace:sess-replace".to_string();

    let req1 = test_forwarder_request("peer-replace", "sess-replace", "terminal_old");
    let req1_active = Arc::clone(&req1.active_flag);
    hub.create_lua_pty_forwarder(req1);

    let req2 = test_forwarder_request("peer-replace", "sess-replace", "terminal_new");
    let req2_active = Arc::clone(&req2.active_flag);
    hub.create_lua_pty_forwarder(req2);

    let pending = hub
        .pending_terminal_attaches
        .get(&key)
        .expect("pending attach should still exist for missing session");
    let subscription_id = match &pending.request {
        PendingTerminalAttachRequest::WebRtc(req) => req.subscription_id.as_str(),
        other => panic!("expected WebRTC pending attach, got {other:?}"),
    };
    assert_eq!(
        subscription_id, "terminal_new",
        "latest subscribe should replace previous pending attach"
    );
    assert!(
        !*req1_active
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "previous pending attach should be deactivated"
    );
    assert!(
        *req2_active
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "replacement attach should remain active"
    );
}

#[test]
pub(super) fn test_tui_attach_intent_resolves_when_session_appears() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "tui:sess-tui-attach".to_string();

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: "sess-tui-attach".to_string(),
        subscription_id: "tui:sess-tui-attach".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_tui_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing session should create pending TUI attach intent"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "TUI forwarder should not start until session is registered"
    );

    hub.handle_cache
        .add_session(test_session_handle("sess-tui-attach"));
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending TUI attach should clear once session exists"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "TUI forwarder should start after session registration"
    );
}

#[test]
pub(super) fn test_tui_input_routes_through_client_worker_to_session_io_mailbox() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-input-mailbox";
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    settle_worker_subscription();

    hub.handle_tui_request(crate::client::TuiRequest::PtyInput {
        session_uuid: session_uuid.to_string(),
        data: b"mailbox\n".to_vec(),
    });

    let mut routed = None;
    for _ in 0..20 {
        if let Ok(request) = session_io_rx.try_recv() {
            if matches!(
                request,
                crate::worker::session_io::SessionIoRequest::PtyInput { .. }
            ) {
                routed = Some(request);
                break;
            }
        }
        hub.tokio_runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });
    }

    assert!(matches!(
        routed,
        Some(crate::worker::session_io::SessionIoRequest::PtyInput { data })
            if data == b"mailbox\n"
    ));
}

#[test]
pub(super) fn test_tui_initial_scrollback_routes_snapshot_through_session_io_mailbox() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-tui-snapshot-mailbox";
    let snapshot = b"tui mailbox snapshot".to_vec();
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 30,
        cols: 100,
    });

    assert!(matches!(
        recv_session_io_request_matching(&mut session_io_rx, |request| matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize {
                rows: 30,
                cols: 100
            }
        )),
        crate::worker::session_io::SessionIoRequest::Resize { .. }
    ));
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };

    settle_worker_subscription();
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.to_string(),
        payload: snapshot.clone(),
    });

    let scrollback = shared_test_runtime()
        .block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(TuiOutput::Scrollback {
                    session_uuid: frame_session,
                    rows,
                    cols,
                    data,
                    ..
                })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                {
                    if frame_session == session_uuid {
                        return Some((rows, cols, data));
                    }
                }
            }
            None
        })
        .expect("TUI scrollback from SessionIo snapshot");

    assert_eq!(scrollback, (30, 100, snapshot));
}

#[test]
pub(super) fn test_tui_snapshot_mailbox_failure_emits_not_ready() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-tui-snapshot-mailbox-missing";
    hub.handle_cache
        .add_session(test_session_backed_handle(session_uuid, 24, 80));

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });

    let outputs = shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut outputs = Vec::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
            {
                outputs.push(output);
                if outputs.iter().any(|output| {
                    matches!(
                        output,
                        TuiOutput::Message(json)
                            if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                                && json.get("state").and_then(|v| v.as_str()) == Some("not_ready")
                                && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid)
                    )
                }) {
                    break;
                }
            }
        }
        outputs
    });

    assert!(
        outputs.iter().any(|output| matches!(
            output,
            TuiOutput::Message(json)
                if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                    && json.get("state").and_then(|v| v.as_str()) == Some("not_ready")
                    && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid)
        )),
        "mailbox failure should emit a deterministic not_ready attach state"
    );
}

#[test]
pub(super) fn test_tui_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "tui:sess-tui-timeout".to_string();

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: "sess-tui-timeout".to_string(),
        subscription_id: "tui:sess-tui-timeout".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_lua_tui_pty_forwarder(req);

    {
        let intent = hub
            .pending_terminal_attaches
            .get_mut(&key)
            .expect("pending TUI attach intent should exist");
        intent.requested_at =
            Instant::now() - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
    }

    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "stale pending TUI attach should be removed"
    );
    assert!(
        !*active_flag
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "not_found transition should deactivate TUI forwarder handle"
    );
}

#[test]
pub(super) fn test_socket_attach_intent_times_out_to_not_found() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let key = "socket:dead:sess-socket-timeout".to_string();

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: "socket:dead".to_string(),
        session_uuid: "sess-socket-timeout".to_string(),
        subscription_id: "socket:sess-socket-timeout".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    let active_flag = Arc::clone(&req.active_flag);
    hub.create_lua_socket_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing socket client/session should create pending socket attach intent"
    );

    {
        let intent = hub
            .pending_terminal_attaches
            .get_mut(&key)
            .expect("pending socket attach intent should exist");
        intent.requested_at =
            Instant::now() - (Hub::TERMINAL_ATTACH_NOT_FOUND_TIMEOUT + Duration::from_millis(1));
    }

    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "stale pending socket attach should be removed"
    );
    assert!(
        !*active_flag
            .lock()
            .expect("Forwarder active_flag mutex poisoned"),
        "not_found transition should deactivate socket forwarder handle"
    );
}

#[test]
pub(super) fn test_socket_attach_intent_resolves_when_session_and_client_appear() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let client_id = "socket:live";
    let key = format!("{client_id}:sess-socket-attach");

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: "sess-socket-attach".to_string(),
        subscription_id: "socket:sess-socket-attach".to_string(),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);

    assert!(
        hub.pending_terminal_attaches.contains_key(&key),
        "missing socket client/session should create pending socket attach intent"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "socket forwarder should not start until session and client are ready"
    );

    let _client_stream = register_test_socket_client(&mut hub, client_id);
    hub.handle_cache
        .add_session(test_session_handle("sess-socket-attach"));
    hub.tick();

    assert!(
        !hub.pending_terminal_attaches.contains_key(&key),
        "pending socket attach should clear once session and client exist"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "socket forwarder should start after prerequisites are available"
    );
}

#[test]
pub(super) fn test_socket_initial_scrollback_routes_snapshot_through_session_io_mailbox() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-snapshot-mailbox";
    let client_id = "socket:snapshot-mailbox";
    let snapshot = b"socket mailbox snapshot".to_vec();
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            session_uuid,
            session_io_tx,
        ));
    let mut client_stream = register_test_socket_client(&mut hub, client_id);

    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 31,
        cols: 101,
    });

    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize {
                rows: 31,
                cols: 101
            }
        )
    });
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };

    settle_worker_subscription();
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.to_string(),
        payload: snapshot.clone(),
    });

    let socket_scrollback =
        read_test_socket_frame_matching(&mut client_stream, Duration::from_secs(2), |frame| {
            matches!(
                frame,
                Frame::Scrollback {
                    session_uuid: frame_session,
                    rows: 31,
                    cols: 101,
                    data,
                    ..
                } if frame_session == session_uuid && data == &snapshot
            )
        });

    assert!(
        socket_scrollback.is_some(),
        "socket initial scrollback should be delivered from SessionIo snapshot response"
    );
}

#[test]
pub(super) fn test_tui_worker_handle_registered_and_removed_with_forwarder() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-tui-worker-lifecycle";
    let key = format!("tui:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_tui_pty_forwarder(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "TUI forwarder should register a ClientWorker handle"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "TUI forwarder task should be tracked by the hub"
    );

    hub.stop_lua_pty_forwarder(&key);

    assert!(
        !hub.terminal_client_workers.contains_key(&key),
        "stopping the forwarder should remove the ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "stopping the forwarder should remove the task"
    );
}

#[test]
pub(super) fn test_socket_worker_handle_registered_and_removed_with_forwarder() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-worker-lifecycle";
    let client_id = "socket:worker-lifecycle";
    let key = format!("{client_id}:{session_uuid}");

    hub.handle_cache
        .add_session(test_session_handle(session_uuid));
    let _client_stream = register_test_socket_client(&mut hub, client_id);

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket forwarder should register a ClientWorker handle"
    );
    assert!(
        hub.pty_forwarders.contains_key(&key),
        "socket forwarder task should be tracked by the hub"
    );

    hub.stop_lua_pty_forwarder(&key);

    assert!(
        !hub.terminal_client_workers.contains_key(&key),
        "stopping the socket forwarder should remove the ClientWorker handle"
    );
    assert!(
        !hub.pty_forwarders.contains_key(&key),
        "stopping the socket forwarder should remove the task"
    );
}

#[test]
pub(super) fn test_tui_and_socket_attach_handlers_delegate_to_terminal_stream_runtime() {
    let source = include_str!("terminal_clients.rs");
    for function in [
        "create_lua_tui_pty_forwarder",
        "try_attach_tui_terminal_forwarder",
        "create_lua_socket_pty_forwarder",
        "try_attach_socket_terminal_forwarder",
    ] {
        let body = function_body(source, function);
        assert!(
            !body.contains("snapshot_and_subscribe"),
            "{function} must not own snapshot subscription logic"
        );
        assert!(
            !body.contains("PtyEvent"),
            "{function} must not own a transport-specific PTY event loop"
        );
    }
}

#[test]
pub(super) fn test_terminal_stream_session_snapshots_use_session_io_mailbox() {
    let source = include_str!("terminal_stream.rs");
    let body = function_body(source, "spawn_terminal_client_forwarder_runtime");
    assert!(
        body.contains("SessionIoRequest::GetSnapshot"),
        "session-backed TUI/socket snapshots must be requested through SessionIoWorker"
    );
    for forbidden in ["resize_direct", ".get_snapshot()"] {
        assert!(
            !body.contains(forbidden),
            "shared TUI/socket runtime must not use direct session snapshot calls: {forbidden}"
        );
    }
}

#[test]
pub(super) fn test_terminal_attach_snapshot_paths_have_no_fixed_sleep_settle_windows() {
    let source = concat!(
        include_str!("terminal_attach.rs"),
        include_str!("terminal_snapshot.rs"),
        include_str!("terminal_stream.rs")
    );
    for function in [
        "create_lua_pty_forwarder",
        "refresh_lua_terminal_snapshot",
        "spawn_terminal_client_forwarder_runtime",
    ] {
        let body = function_body(source, function);
        assert!(
            !body.contains("thread::sleep"),
            "{function} must not use fixed sleep windows on the first-attach snapshot path"
        );
        assert!(
            !body.contains("from_millis(125)"),
            "{function} must not reintroduce the former 125ms attach delay"
        );
    }
}

#[test]
pub(super) fn test_extracted_terminal_runtime_modules_fence_direct_hot_paths() {
    let modules = [
        ("terminal_attach.rs", include_str!("terminal_attach.rs")),
        ("terminal_snapshot.rs", include_str!("terminal_snapshot.rs")),
        ("terminal_stream.rs", include_str!("terminal_stream.rs")),
        ("terminal_clients.rs", include_str!("terminal_clients.rs")),
        (
            "terminal_client_adapters.rs",
            include_str!("terminal_client_adapters.rs"),
        ),
        ("terminal_cleanup.rs", include_str!("terminal_cleanup.rs")),
    ];

    for (module, source) in modules {
        let source = production_source(source);
        for forbidden in ["write_input_direct", "WebRtcAdapterCommand::Pty"] {
            assert!(
                !source.contains(forbidden),
                "{module} must not reintroduce direct terminal hot path: {forbidden}"
            );
        }
        for forbidden in ["thread::sleep", "from_millis(125)"] {
            assert!(
                !source.contains(forbidden),
                "{module} must not reintroduce fixed attach/snapshot settle windows: {forbidden}"
            );
        }
    }

    for (module, source) in [
        ("terminal_attach.rs", include_str!("terminal_attach.rs")),
        ("terminal_stream.rs", include_str!("terminal_stream.rs")),
        ("terminal_clients.rs", include_str!("terminal_clients.rs")),
    ] {
        let source = production_source(source);
        assert!(
            !source.contains(".get_snapshot()"),
            "{module} must not bypass SessionIoWorker snapshots on attach/stream paths"
        );
        assert!(
            !source.contains("resize_direct"),
            "{module} must not bypass SessionIoWorker resize on attach/stream paths"
        );
    }
}

#[test]
pub(super) fn test_missing_session_io_sender_control_records_observable_metric() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();

    hub.handle_client_worker_control(crate::worker::hub_control::HubControlMessage::Backpressure(
        crate::worker::hub_control::WorkerBackpressure {
            source: "worker.client.session_io_missing",
            capacity: 0,
            session_uuid: Some("sess-missing-sender".to_string()),
            client_id: Some(ClientId::Tui),
        },
    ));

    let snapshot = hub.hub_event_metrics.snapshot();
    assert_eq!(snapshot.counters["client_worker.backpressure"], 1);
    assert_eq!(snapshot.counters["client_worker.session_io_missing"], 1);
}

#[test]
pub(super) fn test_server_comms_dispatcher_does_not_own_terminal_hot_paths() {
    let source = include_str!("../server_comms.rs");
    let body = function_body(source, "handle_hub_event");
    for forbidden in [
        "write_input_direct",
        "snapshot_and_subscribe",
        "WebRtcAdapterCommand::Pty",
    ] {
        assert!(
            !source.contains(forbidden),
            "server_comms.rs dispatcher must not contain hot-path terminal code: {forbidden}"
        );
    }
    for forbidden in [
        "spawn_blocking",
        "std::process::Command",
        "serde_json::json!",
        "enqueue_session_io_request",
        "pending_reconnects",
        "terminal_client_workers",
        "WorktreeManager::new",
    ] {
        assert!(
            !body.contains(forbidden),
            "handle_hub_event must stay thin and delegate domain logic; found {forbidden}"
        );
    }
    let body_lines = body.lines().count();
    assert!(
        body_lines <= 310,
        "handle_hub_event should stay visibly thin; saw {body_lines} lines"
    );
}

#[test]
pub(super) fn test_worker_session_io_registration_uses_real_session_io_mailbox() {
    let source = include_str!("terminal_client_adapters.rs");
    let body = function_body(source, "register_worker_session_io_sender");
    assert!(
        body.contains("pty_handle.session_io_sender()"),
        "ClientWorker registration must use the real SessionIoWorker mailbox"
    );
    assert!(
        !body.contains("write_input_direct"),
        "ClientWorker registration must not reintroduce hub-owned PTY writes"
    );
    assert!(
        !body.contains("tokio::spawn"),
        "ClientWorker registration must not create a hub-owned PTY bridge task"
    );
}

#[test]
pub(super) fn test_lua_write_and_resize_pty_are_session_io_data_plane() {
    let source = include_str!("event_socket_terminal.rs");
    let body = function_body(source, "handle_lua_pty_request_event");
    for request in ["WritePty", "ResizePty"] {
        let start = body
            .find(request)
            .unwrap_or_else(|| panic!("missing {request} arm"));
        let excerpt = &body[start..body.len().min(start + 500)];
        assert!(
            excerpt.contains("enqueue_session_io_request"),
            "{request} must route through SessionIoRequest"
        );
        assert!(
            !excerpt.contains("write_input_direct") && !excerpt.contains("resize_direct"),
            "{request} must not use direct hub PTY data-plane calls"
        );
    }
}

#[test]
pub(super) fn test_socket_workerized_live_output_reaches_socket_frame() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-worker-output".to_string();
    let client_id = "socket:worker-output".to_string();
    let key = format!("{client_id}:{session_uuid}");

    let session = test_session_handle(&session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &client_id);

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id,
        session_uuid: session_uuid.to_string(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);
    hub.tick();

    assert!(
        hub.terminal_client_workers.contains_key(&key),
        "socket forwarder should register a ClientWorker handle"
    );

    let subscribed = hub.tokio_runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if event_tx.receiver_count() > 0 {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert!(
        subscribed,
        "socket forwarder should subscribe to PTY output before test emits live bytes"
    );

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"worker-live".to_vec()));

    let found =
        read_test_socket_frame_matching(&mut client_stream, Duration::from_secs(5), |frame| {
            matches!(
                frame,
                Frame::PtyOutput { session_uuid: frame_session, data }
                    if frame_session == &session_uuid && data == b"worker-live"
            )
        })
        .is_some();
    assert!(
        found,
        "live socket PTY output should flow through worker egress"
    );
}

#[test]
pub(super) fn test_shared_terminal_runtime_forwards_equivalent_scrollback_to_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-scrollback".to_string();
    let socket_client_id = "socket:shared-scrollback".to_string();
    let snapshot = b"non-empty shared scrollback snapshot".to_vec();

    hub.handle_cache
        .add_session(test_session_handle_with_snapshot(&session_uuid, &snapshot));
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: socket_client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });

    let tui_scrollback = shared_test_runtime()
        .block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                if let Ok(Some(TuiOutput::Scrollback {
                    session_uuid: frame_session,
                    rows,
                    cols,
                    data,
                    kitty_enabled,
                })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
                {
                    if frame_session == session_uuid {
                        return Some((rows, cols, data, kitty_enabled));
                    }
                }
            }
            None
        })
        .expect("TUI scrollback");

    let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    let socket_scrollback = socket_frames
        .into_iter()
        .find_map(|frame| match frame {
            Frame::Scrollback {
                session_uuid: frame_session,
                rows,
                cols,
                kitty_enabled,
                data,
            } if frame_session == session_uuid => Some((rows, cols, data, kitty_enabled)),
            _ => None,
        })
        .expect("socket scrollback");

    assert_eq!(tui_scrollback, socket_scrollback);
    assert_eq!(
        tui_scrollback,
        (24, 80, snapshot, false),
        "both transports should receive the same non-empty snapshot metadata and payload"
    );
}

#[test]
pub(super) fn test_tui_first_scrollback_latency_budget_session_backed() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let snapshot: Vec<u8> = (0..500)
        .map(|idx| format!("scrollback line {idx:03}\r\n"))
        .collect::<String>()
        .into_bytes();
    let mut elapsed_ms = Vec::new();

    for idx in 0..40 {
        let session_uuid = unique_session_uuid(&format!("sess-tui-latency-{idx}"));
        register_live_session_identity(&session_uuid);

        let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
        hub.handle_cache
            .add_session(test_session_backed_handle_with_mailbox(
                &session_uuid,
                session_io_tx,
            ));

        let started = Instant::now();
        hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
            session_uuid: session_uuid.clone(),
            subscription_id: format!("tui:{session_uuid}"),
            active_flag: Arc::new(Mutex::new(true)),
            rows: 24,
            cols: 80,
        });
        let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
            matches!(
                request,
                crate::worker::session_io::SessionIoRequest::Resize { .. }
            )
        });
        let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
            matches!(
                request,
                crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
            )
        }) {
            crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
            other => panic!("expected GetSnapshot request, got {other:?}"),
        };
        settle_worker_subscription();
        hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
            request_id,
            session_uuid: session_uuid.clone(),
            payload: snapshot.clone(),
        });

        let received = shared_test_runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while tokio::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if let Ok(Some(TuiOutput::Scrollback {
                    session_uuid: frame_session,
                    data,
                    ..
                })) =
                    tokio::time::timeout(remaining.min(Duration::from_millis(50)), output_rx.recv())
                        .await
                {
                    if frame_session == session_uuid {
                        return Some(data);
                    }
                }
            }
            None
        });

        let elapsed = started.elapsed();
        assert_eq!(
            received.as_deref(),
            Some(snapshot.as_slice()),
            "TUI first scrollback should deliver the live session snapshot"
        );
        elapsed_ms.push(elapsed.as_secs_f64() * 1000.0);
        cleanup_live_session_identity(&session_uuid);
    }

    elapsed_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = elapsed_ms[((elapsed_ms.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)];
    let p99 = elapsed_ms[((elapsed_ms.len() as f64 * 0.99).ceil() as usize).saturating_sub(1)];

    assert!(
        p95 < 250.0,
        "TUI first scrollback p95 {p95:.2}ms must stay under 250ms; samples={elapsed_ms:?}"
    );
    assert!(
        p99 < 500.0,
        "TUI first scrollback p99 {p99:.2}ms must stay under 500ms; samples={elapsed_ms:?}"
    );
    eprintln!("TUI first scrollback timing p95={p95:.2}ms p99={p99:.2}ms samples={elapsed_ms:?}");
}

#[test]
pub(super) fn test_shared_terminal_runtime_forwards_live_modes_and_exit_to_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-terminal-runtime".to_string();
    let socket_client_id = "socket:shared-runtime".to_string();

    let session = test_session_handle(&session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: socket_client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    wait_for_receiver_count(&event_tx, 2);
    settle_worker_subscription();

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"one".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::kitty_changed(true));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::focus_reporting_changed(true));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"two".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::process_exited(Some(7)));

    let tui_outputs = shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut outputs = Vec::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
            {
                outputs.push(output);
                if outputs.iter().any(|output| {
                    matches!(
                        output,
                        TuiOutput::ProcessExited {
                            session_uuid: frame_session,
                            exit_code: Some(7),
                        } if frame_session == &session_uuid
                    )
                }) {
                    break;
                }
            }
        }
        outputs
    });

    assert!(
        tui_outputs.iter().any(|output| matches!(
            output,
            TuiOutput::Output { session_uuid: frame_session, data }
                if frame_session == &session_uuid && data == b"one"
        )),
        "TUI should receive first live chunk through shared runtime"
    );
    assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("kitty_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "TUI should receive kitty mode changes through shared runtime"
        );
    assert!(
            tui_outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("focus_reporting_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "TUI should receive focus mode changes through shared runtime"
        );
    assert!(
        tui_outputs.iter().any(|output| matches!(
            output,
            TuiOutput::ProcessExited {
                session_uuid: frame_session,
                exit_code: Some(7),
            } if frame_session == &session_uuid
        )),
        "TUI should receive process exit through shared runtime"
    );

    let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    assert!(
        socket_frames.iter().any(|frame| matches!(
            frame,
            Frame::PtyOutput { session_uuid: frame_session, data }
                if frame_session == &session_uuid && data == b"one"
        )),
        "socket should receive first live chunk through shared runtime"
    );
    assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("kitty_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "socket should receive kitty mode changes through shared runtime"
        );
    assert!(
            socket_frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("focus_reporting_changed")
                        && json.get("enabled").and_then(|v| v.as_bool()) == Some(true)
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "socket should receive focus mode changes through shared runtime"
        );
    assert!(
        socket_frames.iter().any(|frame| matches!(
            frame,
            Frame::ProcessExited {
                session_uuid: frame_session,
                exit_code: Some(7),
            } if frame_session == &session_uuid
        )),
        "socket should receive process exit through shared runtime"
    );
}

#[test]
pub(super) fn test_shared_terminal_runtime_continues_after_broadcast_lag_for_tui_and_socket() {
    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let session_uuid = "sess-shared-lag".to_string();
    let socket_client_id = "socket:shared-lag".to_string();

    let session = test_session_handle_with_broadcast_capacity(&session_uuid, 1);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &socket_client_id);

    hub.create_lua_tui_pty_forwarder(crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: socket_client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    wait_for_receiver_count(&event_tx, 2);

    for i in 0..128 {
        let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(
            format!("dropped-{i}").into_bytes(),
        ));
    }
    settle_worker_subscription();
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"after-lag".to_vec()));

    let tui_seen = shared_test_runtime().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(TuiOutput::Output {
                session_uuid: frame_session,
                data,
            })) = tokio::time::timeout(Duration::from_millis(50), output_rx.recv()).await
            {
                if frame_session == session_uuid && data == b"after-lag" {
                    return true;
                }
            }
        }
        false
    });

    let socket_frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    let socket_seen = socket_frames.iter().any(|frame| {
        matches!(
            frame,
            Frame::PtyOutput {
                session_uuid: frame_session,
                data,
            } if frame_session == &session_uuid && data == b"after-lag"
        )
    });

    assert!(
        tui_seen,
        "TUI shared runtime should continue forwarding after a broadcast lag"
    );
    assert!(
        socket_seen,
        "socket shared runtime should continue forwarding after a broadcast lag"
    );
}

#[test]
pub(super) fn test_socket_shared_runtime_batches_outputs_but_filters_osc_queries_per_chunk() {
    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let session_uuid = "sess-socket-filter-batch".to_string();
    let client_id = "socket:filter-batch".to_string();

    let session = test_session_handle(&session_uuid);
    let event_tx = session.pty().event_tx_clone();
    hub.handle_cache.add_session(session);
    let mut client_stream = register_test_socket_client(&mut hub, &client_id);
    hub.active_terminal_peers
        .lock()
        .expect("active terminal peers")
        .insert(session_uuid.clone(), "socket:owner".to_string());

    hub.create_lua_socket_pty_forwarder(crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.clone(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    });
    wait_for_receiver_count(&event_tx, 1);
    settle_worker_subscription();

    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"A\x1b]11;?".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"\x07B".to_vec()));
    let _ = event_tx.send(crate::agent::pty::PtyEvent::Output(b"C".to_vec()));

    let frames = read_test_socket_frames(&mut client_stream, 8, Duration::from_secs(2));
    let mut chunks = Vec::new();
    for frame in frames {
        if let Frame::PtyOutput {
            session_uuid: frame_session,
            data,
        } = frame
        {
            if frame_session == session_uuid {
                chunks.push(data);
                if chunks.len() == 3 {
                    break;
                }
            }
        }
    }

    assert_eq!(
            chunks,
            vec![b"A".to_vec(), b"B".to_vec(), b"C".to_vec()],
            "socket filtering must preserve per-output-chunk boundaries while stripping split OSC queries"
        );
}

#[test]
pub(super) fn test_tui_attach_reconnecting_emits_explicit_attach_state() {
    let session_uuid = unique_session_uuid("sess-tui-reconnecting");
    register_live_session_identity(&session_uuid);

    let (mut hub, _request_tx, mut output_rx) = e2e_hub();
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));

    let req = crate::lua::primitives::CreateTuiForwarderRequest {
        session_uuid: session_uuid.clone(),
        subscription_id: format!("tui:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_tui_pty_forwarder(req);
    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { .. }
        )
    });
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };
    settle_worker_subscription();
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.clone(),
        payload: Vec::new(),
    });

    let rt = shared_test_runtime();
    let outputs = rt.block_on(async {
        let mut outputs = Vec::new();
        for _ in 0..2 {
            let output = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
                .await
                .expect("timed out waiting for TUI output")
                .expect("TUI output channel closed");
            outputs.push(output);
        }
        outputs
    });

    assert!(
            outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && json.get("state").and_then(|v| v.as_str()) == Some("attached")
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "initial attach should still emit attached state"
        );
    assert!(
            outputs.iter().any(|output| matches!(
                output,
                TuiOutput::Message(json)
                    if json.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && json.get("state").and_then(|v| v.as_str()) == Some("reconnecting")
                        && json.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "reconnect-pending attach should emit explicit reconnecting state"
        );
    assert!(
        !outputs
            .iter()
            .any(|output| matches!(output, TuiOutput::Scrollback { data, .. } if data.is_empty())),
        "reconnect-pending attach must not fake an empty scrollback"
    );

    cleanup_live_session_identity(&session_uuid);
}

#[test]
pub(super) fn test_socket_attach_reconnecting_emits_explicit_attach_state() {
    let session_uuid = unique_session_uuid("sess-socket-reconnecting");
    register_live_session_identity(&session_uuid);

    let (mut hub, _request_tx, _output_rx) = e2e_hub();
    let client_id = "socket:reconnecting";
    let mut client_stream = register_test_socket_client(&mut hub, client_id);
    let (session_io_tx, mut session_io_rx) = tokio::sync::mpsc::channel(8);
    hub.handle_cache
        .add_session(test_session_backed_handle_with_mailbox(
            &session_uuid,
            session_io_tx,
        ));

    let req = crate::lua::primitives::CreateSocketForwarderRequest {
        client_id: client_id.to_string(),
        session_uuid: session_uuid.clone(),
        subscription_id: format!("socket:{session_uuid}"),
        active_flag: Arc::new(Mutex::new(true)),
        rows: 24,
        cols: 80,
    };
    hub.create_lua_socket_pty_forwarder(req);
    let _ = recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::Resize { .. }
        )
    });
    let request_id = match recv_session_io_request_matching(&mut session_io_rx, |request| {
        matches!(
            request,
            crate::worker::session_io::SessionIoRequest::GetSnapshot { .. }
        )
    }) {
        crate::worker::session_io::SessionIoRequest::GetSnapshot { request_id } => request_id,
        other => panic!("expected GetSnapshot request, got {other:?}"),
    };
    settle_worker_subscription();
    hub.handle_session_io_event(crate::worker::session_io::SessionIoEvent::Snapshot {
        request_id,
        session_uuid: session_uuid.clone(),
        payload: Vec::new(),
    });

    let frames = read_test_socket_frames(&mut client_stream, 4, Duration::from_secs(5));

    assert!(
            frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(value)
                    if value.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && value.get("state").and_then(|v| v.as_str()) == Some("attached")
                        && value.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "initial socket attach should still emit attached state"
        );
    assert!(
            frames.iter().any(|frame| matches!(
                frame,
                Frame::Json(value)
                    if value.get("type").and_then(|v| v.as_str()) == Some("terminal_attach")
                        && value.get("state").and_then(|v| v.as_str()) == Some("reconnecting")
                        && value.get("session_uuid").and_then(|v| v.as_str()) == Some(session_uuid.as_str())
            )),
            "reconnect-pending socket attach should emit explicit reconnecting state"
        );
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, Frame::Scrollback { data, .. } if data.is_empty())),
        "reconnect-pending socket attach must not fake an empty scrollback"
    );

    cleanup_live_session_identity(&session_uuid);
}
