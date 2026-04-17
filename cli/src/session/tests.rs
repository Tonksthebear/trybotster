//! Tests for the per-session process architecture.
//!
//! Covers: protocol encode/decode, session-backed PtyHandle paths,
//! hub manifest serialization, and socket path formatting.

#[cfg(test)]
mod protocol_tests {
    use crate::session::protocol::*;

    #[test]
    fn frame_roundtrip() {
        let data = b"hello world";
        let encoded = encode_frame(FRAME_PTY_OUTPUT, data);
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_PTY_OUTPUT);
        assert_eq!(frames[0].payload, data);
    }

    #[test]
    fn empty_frame_roundtrip() {
        let encoded = encode_empty(FRAME_PING);
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_PING);
        assert!(frames[0].payload.is_empty());
    }

    #[test]
    fn json_frame_roundtrip() {
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Resize {
            rows: u16,
            cols: u16,
        }

        let resize = Resize { rows: 24, cols: 80 };
        let encoded = encode_json(FRAME_RESIZE, &resize).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        let decoded: Resize = frames[0].json().unwrap();
        assert_eq!(decoded, resize);
    }

    #[test]
    fn partial_frame_buffering() {
        let encoded = encode_frame(FRAME_PTY_INPUT, b"test");
        let mut decoder = FrameDecoder::new();

        // Feed first 3 bytes (incomplete header)
        let frames = decoder.feed(&encoded[..3]);
        assert!(frames.is_empty());

        // Feed rest
        let frames = decoder.feed(&encoded[3..]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"test");
    }

    #[test]
    fn multiple_frames_in_one_feed() {
        let mut data = Vec::new();
        data.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, b"one"));
        data.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, b"two"));
        data.extend_from_slice(&encode_empty(FRAME_PONG));

        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&data);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].payload, b"one");
        assert_eq!(frames[1].payload, b"two");
        assert!(frames[2].payload.is_empty());
    }

    #[test]
    fn mode_flags_serialization() {
        let flags = ModeFlags {
            kitty_enabled: true,
            cursor_visible: false,
            bracketed_paste: true,
            mouse_mode: 3,
            alt_screen: true,
            focus_reporting: true,
            application_cursor: false,
        };
        let encoded = encode_json(FRAME_MODE_FLAGS, &flags).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        let decoded: ModeFlags = frames[0].json().unwrap();
        assert!(decoded.kitty_enabled);
        assert!(!decoded.cursor_visible);
        assert!(decoded.bracketed_paste);
        assert_eq!(decoded.mouse_mode, 3);
        assert!(decoded.alt_screen);
        assert!(decoded.focus_reporting);
        assert!(!decoded.application_cursor);
    }

    #[test]
    fn session_metadata_serialization() {
        let meta = SessionMetadata {
            session_uuid: "sess-test-123".to_string(),
            pid: 42,
            rows: 24,
            cols: 80,
            last_output_at: 1234567890,
            mode_flags: ModeFlags {
                kitty_enabled: true,
                cursor_visible: false,
                bracketed_paste: true,
                mouse_mode: 3,
                alt_screen: true,
                focus_reporting: true,
                application_cursor: false,
            },
        };
        let json = serde_json::to_vec(&meta).unwrap();
        let decoded: SessionMetadata = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.session_uuid, "sess-test-123");
        assert_eq!(decoded.pid, 42);
        assert_eq!(decoded.rows, 24);
        assert_eq!(decoded.cols, 80);
        assert_eq!(decoded.last_output_at, 1234567890);
        assert!(decoded.mode_flags.kitty_enabled);
        assert!(!decoded.mode_flags.cursor_visible);
        assert!(decoded.mode_flags.bracketed_paste);
        assert_eq!(decoded.mode_flags.mouse_mode, 3);
        assert!(decoded.mode_flags.alt_screen);
        assert!(decoded.mode_flags.focus_reporting);
        assert!(!decoded.mode_flags.application_cursor);
    }
}

#[cfg(test)]
mod pty_handle_tests {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

    use tokio::sync::broadcast;

    use crate::hub::agent_handle::PtyHandle;

    /// Create a session-backed PtyHandle for testing.
    ///
    /// Snapshots return empty since there's no session process to RPC to.
    fn create_session_backed_pty(rows: u16, cols: u16) -> PtyHandle {
        let (event_tx, _rx) = broadcast::channel(64);
        let kitty_enabled = Arc::new(AtomicBool::new(false));
        let cursor_visible = Arc::new(AtomicBool::new(true));
        let resize_pending = Arc::new(AtomicBool::new(false));
        let session_connection = Arc::new(Mutex::new(None));

        PtyHandle::new_with_session(
            event_tx,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            None,
            session_connection,
            Arc::new(AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicI64::new(0)),
            rows,
            cols,
        )
    }

    #[test]
    fn session_backed_handle_is_session_backed() {
        let handle = create_session_backed_pty(24, 80);
        assert!(handle.is_session_backed());
    }

    #[test]
    fn session_backed_handle_preserves_initial_dimensions() {
        let handle = create_session_backed_pty(59, 201);
        assert_eq!(handle.dims(), (59, 201));
    }

    #[test]
    fn session_backed_snapshot_returns_empty_without_session() {
        let handle = create_session_backed_pty(24, 80);
        let snapshot = handle.get_snapshot();
        assert!(
            snapshot.is_empty(),
            "session-backed handle without session should return empty snapshot"
        );
    }

    #[test]
    fn session_backed_snapshot_and_subscribe() {
        let handle = create_session_backed_pty(24, 80);
        let (snapshot, kitty, rows, cols, _rx) = handle.snapshot_and_subscribe();
        assert!(
            snapshot.is_empty(),
            "no session process means empty snapshot"
        );
        assert!(!kitty);
        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn session_backed_resize_without_connection() {
        let handle = create_session_backed_pty(24, 80);
        // Resize with no session connection should not panic
        handle.resize_direct(30, 120);
        // Shared dimensions are updated even without a session connection
        // (the session RPC fails silently, but dims track the requested size)
        assert_eq!(handle.dims(), (30, 120));
    }
}

#[cfg(test)]
mod hub_manifest_tests {
    use crate::hub::daemon::HubManifest;

    #[test]
    fn manifest_workspaces_default_empty() {
        let json = r#"{
            "hub_id": "test",
            "socket_path": "/tmp/test.sock",
            "pid": 1234,
            "updated_at": 0
        }"#;
        let manifest: HubManifest = serde_json::from_str(json).unwrap();
        assert!(
            manifest.workspaces.is_empty(),
            "workspaces should default to empty"
        );
    }

    #[test]
    fn manifest_workspaces_roundtrip() {
        let manifest = HubManifest {
            hub_id: "test".to_string(),
            server_id: None,
            socket_path: "/tmp/test.sock".to_string(),
            pid: 1234,
            updated_at: 0,
            workspaces: vec!["ws-1".to_string(), "ws-2".to_string()],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let decoded: HubManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.workspaces, vec!["ws-1", "ws-2"]);
    }

    #[test]
    fn manifest_without_workspaces_field_parses() {
        // Old manifests from before the workspaces field was added
        let json = r#"{
            "hub_id": "test",
            "socket_path": "/tmp/test.sock",
            "pid": 1234,
            "updated_at": 0
        }"#;
        let manifest: HubManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.workspaces.is_empty());
    }

    #[test]
    fn manifest_empty_workspaces_not_serialized() {
        let manifest = HubManifest {
            hub_id: "test".to_string(),
            server_id: None,
            socket_path: "/tmp/test.sock".to_string(),
            pid: 1234,
            updated_at: 0,
            workspaces: Vec::new(),
        };
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(
            !json.contains("workspaces"),
            "empty workspaces should be skipped in serialization"
        );
    }
}

#[cfg(test)]
mod socket_path_tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::session::{
        cleanup_orphaned_session_files, read_session_pid_file, session_pid_path,
        session_process_is_live, session_socket_path, sessions_socket_dir, write_session_pid_file,
    };

    fn unique_session_uuid(suffix: &str) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("sess-test-{}-{}-{ts}", std::process::id(), suffix)
    }

    #[test]
    fn session_socket_path_format() {
        let path = session_socket_path("sess-1234-abcd").unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "sess-1234-abcd.sock");
    }

    #[test]
    fn session_pid_path_format() {
        let path = session_pid_path("sess-1234-abcd").unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "sess-1234-abcd.pid");
    }

    #[test]
    fn sessions_socket_dir_exists() {
        let dir = sessions_socket_dir().unwrap();
        assert!(dir.exists(), "sessions socket dir should be created");
        assert!(
            dir.to_str().unwrap().contains("sessions"),
            "path should contain 'sessions'"
        );
    }

    #[test]
    fn session_process_is_live_requires_socket_and_live_pid() {
        let session_uuid = unique_session_uuid("live");
        let socket_path = session_socket_path(&session_uuid).unwrap();
        let pid_path = session_pid_path(&session_uuid).unwrap();

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path);

        std::fs::write(&socket_path, b"").unwrap();
        assert!(
            !session_process_is_live(&session_uuid),
            "socket alone should not count as a live session"
        );

        write_session_pid_file(&session_uuid, std::process::id()).unwrap();
        assert!(
            session_process_is_live(&session_uuid),
            "socket plus live pid should count as a live session"
        );
        assert_eq!(
            read_session_pid_file(&session_uuid).unwrap(),
            Some(std::process::id())
        );

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path);
    }

    #[test]
    fn cleanup_orphaned_session_files_preserves_socket_without_pid_file() {
        let session_uuid = unique_session_uuid("missing-pid");
        let socket_path = session_socket_path(&session_uuid).unwrap();
        let pid_path = session_pid_path(&session_uuid).unwrap();

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path);

        std::fs::write(&socket_path, b"").unwrap();
        cleanup_orphaned_session_files();

        assert!(
            socket_path.exists(),
            "cleanup should not remove a socket when pid metadata is missing"
        );

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path);
    }

    #[test]
    fn session_pid_file_is_json_identity_record() {
        let session_uuid = unique_session_uuid("identity");
        let pid_path = session_pid_path(&session_uuid).unwrap();
        let _ = std::fs::remove_file(&pid_path);

        write_session_pid_file(&session_uuid, std::process::id()).unwrap();
        let content = std::fs::read_to_string(&pid_path).unwrap();

        assert!(
            content.trim_start().starts_with('{'),
            "pid file should now serialize a structured identity record"
        );
        assert_eq!(
            read_session_pid_file(&session_uuid).unwrap(),
            Some(std::process::id())
        );

        let _ = std::fs::remove_file(&pid_path);
    }

    #[test]
    fn legacy_plaintext_session_pid_file_still_reads() {
        let session_uuid = unique_session_uuid("legacy");
        let pid_path = session_pid_path(&session_uuid).unwrap();
        let _ = std::fs::remove_file(&pid_path);

        std::fs::write(&pid_path, format!("{}\n", std::process::id())).unwrap();

        assert_eq!(
            read_session_pid_file(&session_uuid).unwrap(),
            Some(std::process::id())
        );

        let _ = std::fs::remove_file(&pid_path);
    }
}
