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

    use crate::agent::pty::HubEventListener;
    use crate::hub::agent_handle::PtyHandle;
    use crate::terminal::AlacrittyParser;

    /// Create a session-backed PtyHandle for testing.
    ///
    /// Returns the handle and the shadow screen Arc for direct inspection.
    fn create_session_backed_pty(
        rows: u16,
        cols: u16,
    ) -> (PtyHandle, Arc<Mutex<AlacrittyParser<HubEventListener>>>) {
        let (event_tx, _rx) = broadcast::channel(64);
        let listener = HubEventListener::new(event_tx.clone());
        let listener_clone = listener.clone();
        let shadow_screen = Arc::new(Mutex::new(AlacrittyParser::new_with_listener(
            rows, cols, 1000, listener,
        )));
        let kitty_enabled = Arc::new(AtomicBool::new(false));
        let cursor_visible = Arc::new(AtomicBool::new(true));
        let resize_pending = Arc::new(AtomicBool::new(false));
        let session_connection = Arc::new(Mutex::new(None));

        let handle = PtyHandle::new_with_session(
            event_tx,
            Arc::clone(&shadow_screen),
            kitty_enabled,
            cursor_visible,
            resize_pending,
            true,
            None,
            session_connection,
            Arc::new(AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicI64::new(0)),
            rows,
            cols,
            listener_clone,
        );

        (handle, shadow_screen)
    }

    #[test]
    fn session_backed_handle_is_session_backed() {
        let (handle, _) = create_session_backed_pty(24, 80);
        assert!(handle.is_session_backed());
    }

    #[test]
    fn session_backed_handle_preserves_initial_dimensions() {
        let (handle, _) = create_session_backed_pty(59, 201);
        assert_eq!(handle.dims(), (59, 201));
    }

    #[test]
    fn snapshot_from_empty_shadow_screen() {
        let (handle, _) = create_session_backed_pty(24, 80);
        let snapshot = handle.get_snapshot();
        // Empty screen should still produce some bytes (cursor positioning, SGR reset)
        assert!(
            !snapshot.is_empty(),
            "even an empty screen produces a snapshot"
        );
        // But it should be small — no real content
        assert!(
            snapshot.len() < 500,
            "empty screen snapshot should be small, got {} bytes",
            snapshot.len()
        );
    }

    #[test]
    fn snapshot_after_feeding_content() {
        let (handle, shadow_screen) = create_session_backed_pty(24, 80);

        // Feed some visible content into the shadow screen
        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"Hello, World!\r\nSecond line here.\r\nThird line.\r\n");
        }

        let snapshot = handle.get_snapshot();
        // Should contain the text we fed
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

    /// The critical test: snapshot content must survive resize_direct.
    ///
    /// This was the bug: the forwarder's resize bounce called do_resize()
    /// which recreated the shadow screen parser, wiping the content that
    /// was just replayed from the session process.
    #[test]
    fn snapshot_survives_resize_direct() {
        let (handle, shadow_screen) = create_session_backed_pty(24, 80);

        // Simulate recovery: feed a snapshot into the shadow screen
        {
            let mut parser = shadow_screen.lock().unwrap();
            // Simulate typical terminal content (prompt, output, etc.)
            parser.process(b"\x1b[H"); // cursor home
            parser.process(b"user@host:~$ ls -la\r\n");
            parser.process(b"total 42\r\n");
            parser.process(b"drwxr-xr-x  5 user user  160 Mar 24 10:00 .\r\n");
            parser.process(b"drwxr-xr-x 30 user user  960 Mar 24 09:00 ..\r\n");
            parser.process(b"-rw-r--r--  1 user user 1234 Mar 24 10:00 file.txt\r\n");
            parser.process(b"user@host:~$ ");
        }

        // Verify content is there before resize
        let before_snapshot = handle.get_snapshot();
        let before_str = String::from_utf8_lossy(&before_snapshot);
        assert!(
            before_str.contains("file.txt"),
            "content should be present before resize"
        );

        // Simulate the forwarder's resize bounce (same dimensions)
        handle.resize_direct(24, 80);

        // Content must survive the resize
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

    /// Test resize to different dimensions preserves content via reflow.
    #[test]
    fn snapshot_survives_resize_to_different_dimensions() {
        let (handle, shadow_screen) = create_session_backed_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"IMPORTANT_CONTENT\r\n");
            parser.process(b"MORE_DATA_HERE\r\n");
        }

        let before = handle.get_snapshot();
        assert!(String::from_utf8_lossy(&before).contains("IMPORTANT_CONTENT"));

        // Resize to different dimensions (the bounce does cols-1 then cols)
        handle.resize_direct(24, 79);
        handle.resize_direct(24, 80);

        let after = handle.get_snapshot();
        assert!(
            String::from_utf8_lossy(&after).contains("IMPORTANT_CONTENT"),
            "content must survive resize bounce to different dimensions"
        );
    }

    /// Test alt-screen content survives resize.
    /// Claude Code runs in alt-screen mode.
    #[test]
    fn alt_screen_snapshot_survives_resize() {
        let (handle, shadow_screen) = create_session_backed_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            // Enter alt screen
            parser.process(b"\x1b[?1049h");
            // Write content in alt screen
            parser.process(b"\x1b[H"); // cursor home
            parser.process(b"ALT_SCREEN_CONTENT\r\n");
            parser.process(b"Line 2 in alt screen\r\n");
        }

        let before = handle.get_snapshot();
        let before_str = String::from_utf8_lossy(&before);
        assert!(
            before_str.contains("ALT_SCREEN_CONTENT"),
            "alt-screen content should be in snapshot before resize"
        );

        // Resize bounce
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
    fn get_snapshot_equals_get_snapshot_cached_for_session_backed() {
        let (handle, shadow_screen) = create_session_backed_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"Some content\r\n");
        }

        let snapshot = handle.get_snapshot();
        let cached = handle.get_snapshot_cached();
        assert_eq!(
            snapshot, cached,
            "get_snapshot and get_snapshot_cached should be identical for session-backed handles"
        );
    }

    #[test]
    fn subscribe_and_snapshot_returns_content() {
        let (handle, shadow_screen) = create_session_backed_pty(24, 80);

        {
            let mut parser = shadow_screen.lock().unwrap();
            parser.process(b"Snapshot test content\r\n");
        }

        let (snapshot, kitty, rows, cols, _rx) = handle.snapshot_and_subscribe_cached();
        assert!(
            String::from_utf8_lossy(&snapshot).contains("Snapshot test"),
            "snapshot_and_subscribe should return content"
        );
        assert!(!kitty);
        assert_eq!(rows, 24);
        assert_eq!(cols, 80);
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
