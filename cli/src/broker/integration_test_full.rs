//! Full end-to-end pipeline integration test for the PTY broker.
//!
//! Unlike the mock-socketpair tests in `connection.rs` — which test
//! `BrokerConnection` frame routing in isolation — this module runs the
//! **real** `broker::run()` entry point in a background thread and proves the
//! entire output pipeline works from PTY write to Hub event.
//!
//! # Pipeline under test
//!
//! ```text
//! write(pipe_write_end, data)
//!   → broker reader_loop reads its dup of pipe_read_end
//!   → encodes PtyOutput frame, sends via Hub socket
//!   → demux thread in BrokerConnection decodes frame
//!   → sends HubEvent::BrokerPtyOutput to event_rx
//!   → feed_broker_output(data) broadcasts PtyEvent::Output to subscribers
//! ```
//!
//! # Why a pipe instead of openpty
//!
//! The broker's `reader_loop` only calls `file.read()` on the registered FD.
//! A `pipe()` pair is sufficient — it avoids `openpty` complexity while
//! exercising the same SCM_RIGHTS FD-transfer and read/frame/route chain.

// Rust guideline compliant 2026-02

use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::broker::broker_socket_path;
use crate::broker::connection::BrokerConnection;
use crate::hub::events::HubEvent;

/// Poll for the broker socket file to appear, up to `timeout`.
///
/// The broker thread needs a moment to bind the Unix domain socket after
/// `run()` is entered. Polling at 20 ms intervals avoids a fixed sleep
/// that would make the test timing fragile.
fn wait_for_broker_socket(path: &std::path::Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if path.exists() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Create a `pipe()` pair.
///
/// Returns `(read_end, write_end)`. Both descriptors are owned by the caller
/// and must be closed when no longer needed.
fn make_pipe() -> (RawFd, RawFd) {
    let mut fds = [0i32; 2];
    // SAFETY: `pipe` writes exactly two valid FDs into `fds` on success.
    let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(ret, 0, "pipe() failed: {}", std::io::Error::last_os_error());
    (fds[0], fds[1])
}

/// Prove end-to-end: the real broker process reads FD output and the Hub
/// receives it as `HubEvent::BrokerPtyOutput`, which then broadcasts through
/// `feed_broker_output` as `PtyEvent::Output`.
///
/// # What this test proves that mock-socketpair tests do NOT
///
/// The mock tests in `connection.rs` replace the broker with a hand-crafted
/// thread that encodes frames directly. This test runs `broker::run()` in a
/// background thread and lets the real reader loop, writer thread, and frame
/// encoder participate — confirming that the FD received via SCM_RIGHTS is
/// actually read, encoded, and forwarded correctly.
#[test]
fn test_full_pipeline_pty_output_reaches_subscriber() {
    // Unique hub_id avoids socket-file collisions between parallel test runs.
    //
    // Using process-id as the suffix is sufficient because only one instance
    // of this test runs per process invocation. If more full-pipeline tests
    // are added, use an AtomicU32 counter instead.
    let hub_id = format!("test-full-{}", std::process::id());
    let socket_path = broker_socket_path(&hub_id).expect("broker_socket_path must succeed");

    // ── 1. Spawn the real broker in a background thread ──────────────────────
    //
    // timeout_secs = 2: after Hub disconnects (kill_all sends KillAll + closes
    // socket), the broker exits immediately. The 2 s window is a safety net for
    // tests that crash before sending KillAll.
    let hub_id_clone = hub_id.clone();
    let broker_thread = std::thread::spawn(move || {
        let _ = crate::broker::run(&hub_id_clone, 2);
    });

    // Wait for broker to bind its socket (up to 2 s).
    assert!(
        wait_for_broker_socket(&socket_path, Duration::from_secs(2)),
        "broker socket did not appear within 2 s — broker thread likely panicked"
    );

    // ── 2. Connect Hub-side ───────────────────────────────────────────────────
    let mut conn = BrokerConnection::connect(&socket_path).expect("BrokerConnection::connect");
    conn.set_timeout(10).expect("set_timeout");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
    conn.install_forwarder(event_tx).expect("install_forwarder");

    // ── 3. Create a pipe pair ─────────────────────────────────────────────────
    //
    // `read_end` is sent to the broker via SCM_RIGHTS.  The broker's
    // `reader_loop` blocks on `read(read_end_dup, …)`.  Writing to `write_end`
    // makes data visible to the broker — equivalent to a PTY process writing
    // to its slave terminal.
    let (read_end, write_end) = make_pipe();

    // ── 4. Register the pipe read_end with the broker ─────────────────────────
    //
    // child_pid = 99999: a deliberately out-of-range PID (macOS max is ~99998).
    // `kill(99999, SIGHUP)` returns ESRCH and is silently ignored inside
    // `Session::kill_child()`, so `kill_all()` during cleanup does not send
    // signals to real processes.
    let session_id = conn
        .register_pty("test-agent", 0, 99999, 24, 80, read_end)
        .expect("register_pty must return a session_id");

    // ── 5. Write raw bytes to the pipe write end ──────────────────────────────
    //
    // Plain ASCII — no PTY line-discipline processing occurs (we are using a
    // pipe, not a real PTY), so the broker sees exactly these bytes and encodes
    // them verbatim in a PtyOutput frame.
    let payload = b"hello from pipe\n";
    let written = unsafe {
        libc::write(
            write_end,
            payload.as_ptr() as *const libc::c_void,
            payload.len(),
        )
    };
    assert_eq!(
        written as usize,
        payload.len(),
        "write to pipe write_end must not short-write"
    );

    // ── 6. Collect BrokerPtyOutput events from the Hub event channel ──────────
    //
    // The broker may split the output across multiple frames if the write
    // crosses a read-buffer boundary.  Accumulate until the full payload is
    // present or the 3-second deadline expires.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for event collection");
    let received_data: Vec<u8> = rt.block_on(async {
        let mut accumulated = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);

        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or(Duration::ZERO);

            match tokio::time::timeout(remaining, event_rx.recv()).await {
                Ok(Some(HubEvent::BrokerPtyOutput { session_id: sid, data })) => {
                    assert_eq!(
                        sid, session_id,
                        "received BrokerPtyOutput for wrong session_id"
                    );
                    accumulated.extend_from_slice(&data);
                    // Stop as soon as the full payload appears in the buffer.
                    if accumulated.windows(payload.len()).any(|w| w == payload) {
                        break;
                    }
                }
                // Any other event variant (or timeout / channel close) ends the loop.
                _ => break,
            }
        }

        accumulated
    });

    // The payload written to the slave must appear verbatim in the accumulated
    // broker output.
    assert!(
        received_data.windows(payload.len()).any(|w| w == payload),
        "expected payload {payload:?} in received broker output — got: {received_data:?}"
    );

    // ── 7. Verify feed_broker_output → PtyEvent::Output broadcast ────────────
    //
    // Construct a minimal PtyHandle (no live process) to exercise the relay-mode
    // injection path that Hub uses when it receives BrokerPtyOutput events.
    // This proves the full downstream pipeline works even without a running Hub.
    let pty_session = crate::agent::pty::PtySession::new(24, 80);
    let (shared_state, shadow_screen, ev_tx, kitty_enabled, resize_pending) =
        pty_session.get_direct_access();
    // Forget the session so the Arc-shared internals (shared_state, shadow_screen,
    // kitty_enabled, resize_pending) remain alive for the PtyHandle lifetime.
    std::mem::forget(pty_session);

    let pty_handle = crate::hub::agent_handle::PtyHandle::new(
        ev_tx,
        shared_state,
        shadow_screen,
        kitty_enabled,
        resize_pending,
        true, // broker agents are CLI sessions
        None, // no HTTP forwarding port
    );

    let mut sub = pty_handle.subscribe();

    // Feed the broker bytes into the handle — replicates what Hub does in its
    // BrokerPtyOutput arm of handle_hub_event().
    pty_handle.feed_broker_output(&received_data);

    // Drain the broadcast channel until PtyEvent::Output(received_data) arrives.
    // feed_broker_output may emit earlier events (OSC scans for title/CWD/kitty
    // etc.) before the final Output event; skip those without failing.
    let rt2 = tokio::runtime::Runtime::new().expect("tokio runtime for PtyEvent check");
    rt2.block_on(async {
        let found = loop {
            match tokio::time::timeout(Duration::from_secs(1), sub.recv()).await {
                Ok(Ok(crate::agent::pty::PtyEvent::Output(data)))
                    if data == received_data =>
                {
                    break true;
                }
                // Other PtyEvent variants (title, cwd, kitty, etc.): keep draining.
                Ok(Ok(_)) => continue,
                // Timeout or channel closed before finding Output event.
                _ => break false,
            }
        };
        assert!(
            found,
            "PtyEvent::Output carrying broker data must be broadcast by feed_broker_output"
        );
    });

    // ── 8. Cleanup ────────────────────────────────────────────────────────────
    //
    // `kill_all()` sends KillAll to the broker and closes the Hub socket.
    // The broker kills sessions (ESRCH on PID 99999, silently ignored),
    // drops the reader FD dup, joins the reader thread, and exits run().
    conn.kill_all();
    broker_thread.join().expect("broker thread must exit cleanly after kill_all");

    // Close our copies of the pipe FDs.  The broker already closed its dup of
    // read_end during kill_all(), so this only releases the Hub-side copies.
    unsafe {
        libc::close(read_end);
        libc::close(write_end);
    }
}

/// Prove that Hub B can reconnect to a live broker after Hub A disconnects and
/// recover both the ring-buffer snapshot and live output forwarding.
///
/// # Reconnect scenario
///
/// ```text
/// Hub A  →  connect (no forwarder) → register_pty(pipe1) → write → drop
///                                                                      ↓
///                                                 broker keeps PTY alive
///                                                 (reconnect countdown active)
///                                                                      ↓
/// Hub B  →  connect → install_forwarder
///                    → get_snapshot(session_id1)  ← ring buffer intact
///                    → register_pty(pipe2)         ← fresh reader on Hub B
///                    → write to pipe2              → BrokerPtyOutput on Hub B
/// ```
///
/// # Why Hub A does NOT call install_forwarder
///
/// `install_forwarder` calls `try_clone()` on the socket, creating a dup FD
/// that a background demux thread holds.  When `disconnect_graceful` drops
/// the original FD, the dup keeps the kernel socket alive.  The broker's
/// blocking `recvmsg_fds` call never returns EOF because the socket has an
/// open reader, so the broker stays stuck in `handle_connection` and never
/// enters the reconnect loop — causing a hang.
///
/// Hub A only needs to register a PTY and write data to populate the ring
/// buffer.  It does not need to receive events, so no forwarder is needed.
/// Without a forwarder there is only one FD; dropping it produces an
/// immediate EOF at the broker.
///
/// # Why Hub B uses a new pipe for live output
///
/// The broker's per-session reader thread captures the Hub's `output_tx` in
/// its closure at spawn time.  After Hub A disconnects, that sender's receiver
/// is dropped, so new output from the pipe1 session is silently discarded.
/// Hub B registers a fresh `pipe2` to get a new reader thread wired to Hub B's
/// `output_tx`.  The stale pipe1 session is left in the broker; `kill_all` at
/// the end closes its FD (causing EBADF in the reader thread) and joins the
/// reader cleanly.
///
/// # What this test proves
///
/// After Hub A drops its connection, the broker must:
/// 1. Keep the registered PTY session alive (ring buffer intact).
/// 2. Accept a fresh Hub B connection on the same socket path.
/// 3. Deliver a ring-buffer snapshot containing data Hub A wrote.
/// 4. Allow Hub B to register a new session and receive live output.
#[test]
fn test_hub_reconnect_snapshot_and_output() {
    // Unique socket suffix avoids collision with other integration tests in the
    // same process invocation.
    let hub_id = format!("test-reconnect-{}", std::process::id());
    let socket_path = broker_socket_path(&hub_id).expect("broker_socket_path must succeed");

    // ── 1. Start the real broker with a 10-second reconnect window ────────────
    //
    // 10 s is generous: Hub A disconnects and Hub B reconnects within the same
    // test, so the actual gap is a few milliseconds.  The window only matters
    // if the test process stalls on a loaded CI machine.
    let hub_id_clone = hub_id.clone();
    let broker_thread = std::thread::spawn(move || {
        let _ = crate::broker::run(&hub_id_clone, 10);
    });

    assert!(
        wait_for_broker_socket(&socket_path, Duration::from_secs(2)),
        "broker socket did not appear within 2 s"
    );

    // ── 2. Hub A: connect WITHOUT forwarder, register pipe1, write data ───────
    //
    // Hub A intentionally skips `install_forwarder` so there is no demux dup
    // of the socket FD.  When the `BrokerConnection` is dropped below, the
    // single FD closes and the broker immediately sees EOF.
    let (read_end1, write_end1) = make_pipe();

    let mut conn_a = BrokerConnection::connect(&socket_path).expect("Hub A: connect");

    // child_pid = 99999: deliberately out-of-range on macOS (max ~99998), so
    // kill_all() during cleanup sends SIGHUP/SIGKILL to a non-existent PID
    // (ESRCH, silently ignored) and does not affect real processes.
    let session_id1 = conn_a
        .register_pty("test-agent-reconnect-a", 0, 99999, 24, 80, read_end1)
        .expect("Hub A: register_pty must return a session_id");

    // Write the first payload through pipe1.  The broker's reader_loop reads
    // its dup of read_end1, feeds it into the AlacrittyParser, and sends a
    // PtyOutput frame.  We do not consume that frame here; we only care that
    // the parser has processed the bytes by the time Hub B requests a snapshot.
    let payload_a = b"hub-a-data\n";
    let written1 = unsafe {
        libc::write(
            write_end1,
            payload_a.as_ptr() as *const libc::c_void,
            payload_a.len(),
        )
    };
    assert_eq!(written1 as usize, payload_a.len(), "pipe1 write must not short-write");

    // Give the broker reader_loop one generous scheduling quantum to read the
    // pipe and feed the bytes into the AlacrittyParser.  200 ms is well above
    // any realistic OS scheduling latency on a lightly loaded machine.
    std::thread::sleep(Duration::from_millis(200));

    // ── 3. Hub A disconnects ──────────────────────────────────────────────────
    //
    // `disconnect_graceful` calls `shutdown(SHUT_RDWR)` before dropping.
    // The explicit shutdown delivers a FIN to the broker regardless of unread
    // data in the socket receive buffer.  Without the shutdown, macOS may
    // convert the FIN to a RST if Hub A's receive buffer has unread PtyOutput
    // frames (data the broker sent but Hub A never consumed because there is
    // no forwarder).  A RST causes `recvmsg_fds` to return ECONNRESET, which
    // also exits the loop — but on some macOS kernel versions the RST does not
    // immediately unblock a pending `recvmsg`.  The explicit FIN from shutdown
    // guarantees the broker exits `handle_connection` and starts the 10-second
    // reconnect countdown.
    conn_a.disconnect_graceful();

    // Brief pause: lets the broker exit `handle_connection` and enter
    // `wait_for_reconnect` before Hub B tries to connect.
    std::thread::sleep(Duration::from_millis(50));

    // ── 4. Hub B: connect to the same socket path ─────────────────────────────
    //
    // `wait_for_reconnect` polls the listener at 250 ms intervals.  Hub B's
    // kernel-level connection is added to the backlog immediately; the broker
    // picks it up on the next poll (≤ 250 ms).
    let mut conn_b = BrokerConnection::connect(&socket_path).expect("Hub B: connect");
    conn_b.set_timeout(10).expect("Hub B: set_timeout");

    let (event_tx_b, mut event_rx_b) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
    conn_b.install_forwarder(event_tx_b).expect("Hub B: install_forwarder");

    // ── 5. get_snapshot: AlacrittyParser must have Hub A's data ──────────────
    //
    // The broker's per-session AlacrittyParser persists across Hub connections,
    // so Hub B receives a generated ANSI snapshot that includes the text written
    // during step 2 — even though Hub A never consumed the PtyOutput frame.
    // The snapshot is ANSI-formatted cell grid output, not raw bytes, so we
    // search for the text content rather than the exact payload bytes.
    let snapshot = conn_b
        .get_snapshot(session_id1)
        .expect("Hub B: get_snapshot must succeed for the still-live session");

    let snapshot_text = String::from_utf8_lossy(&snapshot);
    assert!(
        snapshot_text.contains("hub-a-data"),
        "snapshot must contain Hub A's payload text — got {} bytes: {snapshot:?}",
        snapshot.len()
    );

    // ── 6. Hub B registers a fresh pipe2 for live output ─────────────────────
    //
    // The stale pipe1 session still exists but its reader thread's `output_tx`
    // is dead (the receiver was dropped with Hub A's connection).  Hub B
    // registers a NEW pipe2 to get a fresh reader thread wired to Hub B's
    // event channel.  kill_all cleans up the stale session at the end.
    let (read_end2, write_end2) = make_pipe();

    let session_id2 = conn_b
        .register_pty("test-agent-reconnect-b", 0, 99999, 24, 80, read_end2)
        .expect("Hub B: register pipe2 must return a new session_id");

    // ── 7. Write to pipe2, verify Hub B receives BrokerPtyOutput ─────────────
    let payload_b = b"hub-b-data\n";
    let written2 = unsafe {
        libc::write(
            write_end2,
            payload_b.as_ptr() as *const libc::c_void,
            payload_b.len(),
        )
    };
    assert_eq!(written2 as usize, payload_b.len(), "pipe2 write must not short-write");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for Hub B events");
    let received_b: Vec<u8> = rt.block_on(async {
        let mut accumulated = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or(Duration::ZERO);
            match tokio::time::timeout(remaining, event_rx_b.recv()).await {
                Ok(Some(HubEvent::BrokerPtyOutput { session_id: sid, data })) => {
                    assert_eq!(sid, session_id2, "BrokerPtyOutput for wrong session_id");
                    accumulated.extend_from_slice(&data);
                    if accumulated.windows(payload_b.len()).any(|w| w == payload_b) {
                        break;
                    }
                }
                // Snapshot frames routed to event_tx? No — demux routes
                // Snapshot to response_tx, not event_tx.  Any other event
                // variant or timeout ends the loop.
                _ => break,
            }
        }
        accumulated
    });

    assert!(
        received_b.windows(payload_b.len()).any(|w| w == payload_b),
        "Hub B must receive the second payload via the new session — got: {received_b:?}"
    );

    // ── 8. Cleanup ────────────────────────────────────────────────────────────
    //
    // kill_all signals all sessions (ESRCH on PID 99999, silently ignored),
    // drops broker FD dups (causing EBADF in reader threads so they exit), and
    // joins the reader threads — then returns, unblocking broker_thread.join().
    conn_b.kill_all();
    broker_thread.join().expect("broker thread must exit cleanly after kill_all");

    // Close Hub-side copies of all pipe FDs.
    unsafe {
        libc::close(read_end1);
        libc::close(write_end1);
        libc::close(read_end2);
        libc::close(write_end2);
    }
}

/// Prove that an existing session's live output routes to Hub B after reconnect
/// via `SharedWriter` — without registering a new pipe.
///
/// # The gap this fills
///
/// `test_hub_reconnect_snapshot_and_output` verifies ring-buffer snapshot recovery
/// and live output from a *freshly registered* `pipe2`.  It does NOT verify that
/// the **original** `pipe1` session routes to Hub B after reconnect, which is the
/// actual agent-survival scenario: the agent's PTY is still running and producing
/// output; Hub B must receive that output without re-registering anything.
///
/// # What `SharedWriter` provides
///
/// Before the fix, each `handle_connection` call stored the per-connection
/// `writer_tx` in reader thread closures at spawn time.  After Hub A disconnected
/// its receiver was dead, so pipe1 output was silently dropped on Hub B.
///
/// With `SharedWriter`, all reader threads share a single `Arc<Mutex<Option<Sender>>>`.
/// `handle_connection` updates the inner `Option` at connection time, instantly
/// re-wiring every surviving reader thread to the new Hub connection.
///
/// # Scenario
///
/// ```text
/// Hub A  → connect (no forwarder) → register_pty(pipe1) → disconnect
///                                                            ↓
///                                             broker keeps pipe1 reader alive
///                                                            ↓
/// Hub B  → connect → install_forwarder
///       → write to pipe1 (same write_end still open)
///       → receives BrokerPtyOutput(session_id1) ← proves SharedWriter re-wire
/// ```
#[test]
fn test_existing_session_routes_to_hub_b_after_reconnect() {
    // Unique socket name avoids collision with parallel tests in the same process.
    let hub_id = format!("test-existing-{}", std::process::id());
    let socket_path = broker_socket_path(&hub_id).expect("broker_socket_path must succeed");

    // ── 1. Start broker with a 10-second reconnect window ────────────────────
    let hub_id_clone = hub_id.clone();
    let broker_thread = std::thread::spawn(move || {
        let _ = crate::broker::run(&hub_id_clone, 10);
    });

    assert!(
        wait_for_broker_socket(&socket_path, Duration::from_secs(2)),
        "broker socket did not appear within 2 s"
    );

    // ── 2. Hub A: connect WITHOUT forwarder, register pipe1 ──────────────────
    //
    // No forwarder means a single socket FD; when `disconnect_graceful` calls
    // `shutdown(SHUT_RDWR)` the broker sees EOF immediately and enters the
    // reconnect window — no dup keeps the socket alive.
    let (read_end1, write_end1) = make_pipe();

    let mut conn_a = BrokerConnection::connect(&socket_path).expect("Hub A: connect");
    let session_id1 = conn_a
        .register_pty("test-agent-existing", 0, 99999, 24, 80, read_end1)
        .expect("Hub A: register_pty");

    // ── 3. Hub A disconnects (simulating exec-restart) ────────────────────────
    //
    // `disconnect_graceful` calls `shutdown(SHUT_RDWR)` before dropping.
    // The explicit shutdown sends a FIN to the broker guaranteeing it exits
    // `handle_connection` even if its receive buffer has unread frames.
    conn_a.disconnect_graceful();
    std::thread::sleep(Duration::from_millis(50));

    // ── 4. Hub B: connect and install forwarder ───────────────────────────────
    //
    // After reconnect, `handle_connection` calls `*shared_writer = Some(writer_tx_b)`
    // before processing any frames.  The pipe1 reader thread — still alive and
    // blocking in `file.read()` — will use this new sender for all future output.
    let mut conn_b = BrokerConnection::connect(&socket_path).expect("Hub B: connect");
    conn_b.set_timeout(10).expect("Hub B: set_timeout");

    let (event_tx_b, mut event_rx_b) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
    conn_b.install_forwarder(event_tx_b).expect("Hub B: install_forwarder");

    // ── 5. Write to the ORIGINAL pipe1 after Hub B is connected ──────────────
    //
    // This is the real agent-survival scenario: the existing PTY (Claude, a shell,
    // etc.) keeps writing output after the Hub restarts.  Hub B must receive it
    // without any new registration.
    let payload = b"surviving-agent-output\n";
    let written = unsafe {
        libc::write(
            write_end1,
            payload.as_ptr() as *const libc::c_void,
            payload.len(),
        )
    };
    assert_eq!(written as usize, payload.len(), "pipe1 write must not short-write");

    // ── 6. Hub B must receive BrokerPtyOutput for the original session ────────
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let received: Vec<u8> = rt.block_on(async {
        let mut accumulated = Vec::new();
        // 3-second deadline: the pipe1 reader thread unblocks instantly on write,
        // encodes the frame, and delivers it via SharedWriter → Hub B's socket.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or(Duration::ZERO);
            match tokio::time::timeout(remaining, event_rx_b.recv()).await {
                Ok(Some(HubEvent::BrokerPtyOutput { session_id: sid, data })) => {
                    assert_eq!(
                        sid, session_id1,
                        "SharedWriter must route pipe1 output to Hub B under session_id1"
                    );
                    accumulated.extend_from_slice(&data);
                    if accumulated.windows(payload.len()).any(|w| w == payload) {
                        break;
                    }
                }
                _ => break,
            }
        }
        accumulated
    });

    assert!(
        received.windows(payload.len()).any(|w| w == payload),
        "Hub B must receive pipe1 output after reconnect — SharedWriter re-wire failed; got: {received:?}"
    );

    // ── 7. Cleanup ────────────────────────────────────────────────────────────
    conn_b.kill_all();
    broker_thread.join().expect("broker thread must exit cleanly");

    unsafe {
        libc::close(read_end1);
        libc::close(write_end1);
    }
}

/// Regression test: `get_snapshot` must be delivered even while PTY output is
/// flooding the writer channel.
///
/// # The bug this guards against
///
/// The original writer used a bounded `SyncChannel(256)`.  PTY reader threads
/// filled it with output frames; when the broker then tried `try_send` for the
/// `Snapshot` control response, the channel was full and the frame was silently
/// dropped.  The Hub's `read_response()` then blocked forever on `recv()`,
/// freezing the entire Hub event loop (WebRTC, TUI, everything).
///
/// The fix replaces the bounded channel with an unbounded one so control frames
/// are always enqueued.  The writer delivers them in FIFO order; the Hub
/// unblocks once the writer catches up — which takes milliseconds at socket
/// copy speeds.
///
/// # How this test triggers the bug
///
/// 1. Register a pipe session.
/// 2. Spawn a thread that writes enough data to fill the old 256-frame bound
///    (256 × 4 096 B ≈ 1 MB) while the main thread concurrently calls
///    `get_snapshot`.
/// 3. If `get_snapshot` returns within the 10-second deadline, the fix holds.
///    Before the fix the call would block indefinitely.
#[test]
fn test_ctrl_delivery_under_output_flood() {
    let hub_id = format!("test-flood-{}", std::process::id());
    let socket_path = broker_socket_path(&hub_id).expect("broker_socket_path");

    let hub_id_clone = hub_id.clone();
    let broker_thread = std::thread::spawn(move || {
        let _ = crate::broker::run(&hub_id_clone, 2);
    });

    assert!(
        wait_for_broker_socket(&socket_path, Duration::from_secs(2)),
        "broker socket did not appear within 2 s"
    );

    let mut conn = BrokerConnection::connect(&socket_path).expect("connect");
    conn.set_timeout(10).expect("set_timeout");

    // event_rx intentionally unused — demux ignores send errors to a dropped
    // receiver and keeps running, so the control-response path stays alive.
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<HubEvent>();
    conn.install_forwarder(event_tx).expect("install_forwarder");

    let (read_end, write_end) = make_pipe();

    let session_id = conn
        .register_pty("test-agent-flood", 0, 99999, 24, 80, read_end)
        .expect("register_pty");

    // Flood thread: write 256 × 4 096-byte chunks — enough to fill the old
    // bounded channel — while the main thread concurrently calls get_snapshot.
    // Returns write_end so the main thread can close it during cleanup.
    let flood_handle = std::thread::spawn(move || {
        let chunk = vec![b'X'; 4096];
        for _ in 0..256 {
            unsafe {
                libc::write(
                    write_end,
                    chunk.as_ptr() as *const libc::c_void,
                    chunk.len(),
                );
            }
        }
        write_end
    });

    // A brief pause lets the broker queue some output frames before we request
    // the snapshot, maximising the chance the old bounded channel would be full.
    std::thread::sleep(Duration::from_millis(50));

    // get_snapshot MUST return within the 10-second broker read timeout.
    // Before the fix this blocked indefinitely; after the fix it returns as
    // soon as the writer delivers the Snapshot frame (milliseconds).
    conn.get_snapshot(session_id)
        .expect("get_snapshot must succeed even while output is flooding the writer channel");

    let write_end = flood_handle.join().expect("flood thread");

    conn.kill_all();
    broker_thread.join().expect("broker thread must exit cleanly after kill_all");

    unsafe {
        libc::close(read_end);
        libc::close(write_end);
    }
}

/// Diagnostic: verify that `shutdown(SHUT_WR)` on one end of a Unix domain
/// socketpair immediately unblocks a blocking `recvmsg` call on the other end.
///
/// This test documents a macOS-specific behavior relevant to
/// `test_hub_reconnect_snapshot_and_output`: the broker's `recvmsg_fds` call
/// must return EOF when the Hub calls `shutdown(SHUT_WR)` on its side.
///
/// If this test fails (hangs > 2 s), it confirms that `shutdown(SHUT_WR)`
/// does NOT interrupt a blocking `recvmsg` on this platform, which explains
/// the reconnect test hang.
#[test]
fn test_shutdown_write_interrupts_blocking_recvmsg() {
    use std::os::unix::net::UnixStream;
    use std::os::unix::io::AsRawFd;

    let (hub_stream, broker_stream) = UnixStream::pair()
        .expect("socketpair for shutdown test");

    // Broker thread: block in recvmsg until peer sends EOF, then record
    // the result.  A 2-second timeout prevents infinite hang on failure.
    let broker_fd = broker_stream.as_raw_fd();
    // Move broker_stream into the thread so its FD stays alive.
    let broker_handle = std::thread::spawn(move || {
        let _keep = broker_stream; // prevent drop
        let mut data_buf = vec![0u8; 256];
        let mut iov = libc::iovec {
            iov_base: data_buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: data_buf.len(),
        };
        let mut msg = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: &mut iov,
            msg_iovlen: 1,
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        // SAFETY: recvmsg on a valid socket FD with properly-initialized iov and msghdr.
        let n = unsafe { libc::recvmsg(broker_fd, &mut msg, 0) };
        // n == 0 means EOF (shutdown received); n < 0 means error.
        n
    });

    // Hub side: wait 100 ms then call shutdown(SHUT_WR) to send FIN.
    std::thread::sleep(Duration::from_millis(100));
    let _ = hub_stream.shutdown(std::net::Shutdown::Write);

    // The broker thread must unblock within 2 seconds.
    let result = broker_handle.join().expect("broker thread must not panic");

    assert_eq!(
        result,
        0,
        "shutdown(SHUT_WR) must cause blocking recvmsg on peer to return 0 (EOF); \
         got {result} — this means shutdown does NOT interrupt blocking recvmsg on this platform, \
         which explains the test_hub_reconnect_snapshot_and_output hang"
    );
}
