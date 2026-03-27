//! Tests for the per-session process architecture.
//!
//! Covers: protocol encode/decode, snapshot survival across resize,
//! PtyHandle session-backed paths, and recovery flows.

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
        };
        let json = serde_json::to_vec(&meta).unwrap();
        let decoded: SessionMetadata = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.session_uuid, "sess-test-123");
        assert_eq!(decoded.pid, 42);
        assert_eq!(decoded.rows, 24);
        assert_eq!(decoded.cols, 80);
        assert_eq!(decoded.last_output_at, 1234567890);
    }
}

#[cfg(test)]
mod pty_handle_tests {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex};

    use tokio::sync::broadcast;

    use crate::agent::pty::PtySession;
    use crate::hub::agent_handle::PtyHandle;
    use crate::terminal::TerminalParser;

    /// Create a session-backed PtyHandle for testing (no shadow screen).
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

    /// Create a local PtyHandle with shadow screen for snapshot tests.
    fn create_local_pty(rows: u16, cols: u16) -> (PtyHandle, Arc<Mutex<TerminalParser>>) {
        let pty_session = PtySession::new(rows, cols);
        let (shared_state, shadow_screen, event_tx, kitty_enabled, cursor_visible, resize_pending) =
            pty_session.get_direct_access();
        std::mem::forget(pty_session);
        let handle = PtyHandle::new(
            event_tx,
            shared_state,
            shadow_screen.clone(),
            kitty_enabled,
            cursor_visible,
            resize_pending,
            None,
        );
        (handle, shadow_screen)
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
    fn snapshot_from_empty_screen() {
        let (handle, _) = create_local_pty(24, 80);
        let snapshot = handle.get_snapshot();
        assert!(
            !snapshot.is_empty(),
            "even an empty screen produces a snapshot"
        );
        assert!(
            snapshot.len() < 10000,
            "empty screen snapshot should be reasonable size, got {} bytes",
            snapshot.len()
        );
    }

    #[test]
    fn snapshot_after_feeding_content() {
        let (handle, shadow_screen) = create_local_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"Hello, World!\r\nSecond line here.\r\nThird line.\r\n");
        }

        let snapshot = handle.get_snapshot();
        let snapshot_str = String::from_utf8_lossy(&snapshot);
        assert!(
            snapshot_str.contains("Hello, World!"),
            "snapshot should contain fed content, got: {}",
            snapshot_str
        );
        assert!(
            snapshot_str.contains("Second line"),
            "snapshot should contain second line"
        );
    }

    #[test]
    fn snapshot_survives_resize_direct() {
        let (handle, shadow_screen) = create_local_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"\x1b[H");
            parser.process(b"user@host:~$ ls -la\r\n");
            parser.process(b"total 42\r\n");
            parser.process(b"drwxr-xr-x  5 user user  160 Mar 24 10:00 .\r\n");
            parser.process(b"drwxr-xr-x 30 user user  960 Mar 24 09:00 ..\r\n");
            parser.process(b"-rw-r--r--  1 user user 1234 Mar 24 10:00 file.txt\r\n");
            parser.process(b"user@host:~$ ");
        }

        let before_snapshot = handle.get_snapshot();
        let before_str = String::from_utf8_lossy(&before_snapshot);
        assert!(
            before_str.contains("file.txt"),
            "content should be present before resize"
        );

        handle.resize_direct(24, 80);

        let after_snapshot = handle.get_snapshot();
        let after_str = String::from_utf8_lossy(&after_snapshot);
        assert!(
            after_str.contains("file.txt"),
            "content must survive resize_direct! Got: {}",
            after_str
        );
        assert!(
            after_str.contains("ls -la"),
            "command must survive resize_direct"
        );
    }

    #[test]
    fn snapshot_survives_resize_to_different_dimensions() {
        let (handle, shadow_screen) = create_local_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"IMPORTANT_CONTENT\r\n");
            parser.process(b"MORE_DATA_HERE\r\n");
        }

        let before = handle.get_snapshot();
        assert!(String::from_utf8_lossy(&before).contains("IMPORTANT_CONTENT"));

        handle.resize_direct(24, 79);
        handle.resize_direct(24, 80);

        let after = handle.get_snapshot();
        assert!(
            String::from_utf8_lossy(&after).contains("IMPORTANT_CONTENT"),
            "content must survive resize bounce to different dimensions"
        );
    }

    #[test]
    fn alt_screen_snapshot_survives_resize() {
        let (handle, shadow_screen) = create_local_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"\x1b[?1049h");
            parser.process(b"\x1b[H");
            parser.process(b"ALT_SCREEN_CONTENT\r\n");
            parser.process(b"Line 2 in alt screen\r\n");
        }

        let before = handle.get_snapshot();
        let before_str = String::from_utf8_lossy(&before);
        assert!(
            before_str.contains("ALT_SCREEN_CONTENT"),
            "alt-screen content should be in snapshot before resize"
        );

        handle.resize_direct(24, 79);
        handle.resize_direct(24, 80);

        let after = handle.get_snapshot();
        let after_str = String::from_utf8_lossy(&after);
        assert!(
            after_str.contains("ALT_SCREEN_CONTENT"),
            "alt-screen content must survive resize! Got: {}",
            after_str
        );
    }

    #[test]
    fn get_snapshot_returns_content_for_local() {
        let (handle, shadow_screen) = create_local_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"Some content\r\n");
        }

        let snapshot = handle.get_snapshot();
        let snap_str = String::from_utf8_lossy(&snapshot);
        assert!(
            snap_str.contains("Some content"),
            "get_snapshot should return content for local handles"
        );
    }

    #[test]
    fn subscribe_and_snapshot_returns_content() {
        let (handle, shadow_screen) = create_local_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"Snapshot test content\r\n");
        }

        let (snapshot, kitty, rows, cols, _rx) = handle.snapshot_and_subscribe();
        assert!(
            String::from_utf8_lossy(&snapshot).contains("Snapshot test"),
            "snapshot_and_subscribe should return content"
        );
        assert!(!kitty);
        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
    }

    #[test]
    fn session_backed_snapshot_returns_empty_without_session() {
        let handle = create_session_backed_pty(24, 80);
        // No session process connected, so snapshot should be empty
        let snapshot = handle.get_snapshot();
        assert!(
            snapshot.is_empty(),
            "session-backed handle without session should return empty snapshot"
        );
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
    use crate::session::{session_socket_path, sessions_socket_dir};

    #[test]
    fn session_socket_path_format() {
        let path = session_socket_path("sess-1234-abcd").unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(filename, "sess-1234-abcd.sock");
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
}

