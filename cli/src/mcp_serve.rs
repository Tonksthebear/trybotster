//! MCP stdio server bridge.
//!
//! Connects to a running hub via Unix socket, subscribes to the "mcp"
//! channel, and translates between MCP JSON-RPC on stdin/stdout and
//! the hub's internal socket protocol.
//!
//! Launched by Claude Code as: `botster mcp-serve --socket /path/to/hub.sock`

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

use crate::socket::framing::{Frame, FrameDecoder};

/// Subscription ID used for the MCP channel on the hub socket.
const SUB_ID: &str = "mcp_bridge";

/// How many times to retry connecting to the hub socket on startup.
///
/// Claude Code kills and restarts `botster mcp-serve` when it receives a
/// `notifications/tools/list_changed` event (e.g., after a plugin hot-reload).
/// The new process may race with the hub briefly holding the socket fd — a
/// few retries with a short delay cover the timing window reliably.
const CONNECT_RETRIES: u32 = 5;

/// Base delay between socket connect retries, in milliseconds.
///
/// Each attempt waits `attempt * CONNECT_RETRY_BASE_MS` (linear backoff),
/// so retries span ~0ms, ~300ms, ~600ms, ~900ms, ~1200ms — ~3 s total.
const CONNECT_RETRY_BASE_MS: u64 = 300;

/// How many times to retry reconnecting to the hub socket after a mid-session
/// disconnect (hub restart, transient error, etc.).
const RECONNECT_RETRIES: u32 = 10;

/// Fixed delay between reconnect attempts in milliseconds.
///
/// The hub may take several seconds to restart; 10 attempts × 1 s gives a
/// ~10 s window which is comfortably longer than a typical hub restart.
const RECONNECT_RETRY_MS: u64 = 1_000;

/// Reason a single MCP session ended.
enum SessionExit {
    /// Claude Code closed stdin — exit the process immediately.
    StdinClosed,
    /// Hub socket dropped (read returned EOF or write failed) — reconnect.
    HubDisconnected,
}

/// Run the MCP bridge synchronously, blocking on a tokio runtime.
pub fn run(socket_path: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { run_async(socket_path).await })
}

/// Connect to the hub socket, retrying on failure.
///
/// When `linear_backoff` is true, each attempt waits `attempt * delay_ms`
/// (startup behaviour). When false, each attempt waits a fixed `delay_ms`
/// (mid-session reconnect behaviour).
async fn connect_to_hub(
    socket_path: &str,
    retries: u32,
    delay_ms: u64,
    linear_backoff: bool,
) -> Result<tokio::net::UnixStream> {
    if retries == 0 {
        return Err(anyhow::anyhow!(
            "connect_to_hub called with retries=0: {socket_path}"
        ));
    }
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..retries {
        if attempt > 0 {
            let delay = if linear_backoff {
                std::time::Duration::from_millis(attempt as u64 * delay_ms)
            } else {
                std::time::Duration::from_millis(delay_ms)
            };
            tokio::time::sleep(delay).await;
        }
        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                eprintln!(
                    "[botster-mcp] connect attempt {}/{} failed: {e}",
                    attempt + 1,
                    retries
                );
                last_err = Some(e);
            }
        }
    }
    Err(anyhow::anyhow!(
        "Failed to connect to hub socket after {retries} attempts: {socket_path}: {}",
        last_err.map_or_else(|| "unknown error".to_owned(), |e| e.to_string())
    ))
}

/// Outer reconnect loop: connects to hub, runs a session, and reconnects on
/// hub disconnect. Only exits when stdin closes (Claude Code is gone).
async fn run_async(socket_path: &str) -> Result<()> {
    // Spawn stdin reader once — it lives for the lifetime of the process.
    // Between hub reconnects, buffered lines continue accumulating here.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_blocking(move || {
        let stdin = io::stdin();
        let reader = stdin.lock();
        for line in reader.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if stdin_tx.send(l).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let mut first_connect = true;
    loop {
        let stream = if first_connect {
            first_connect = false;
            connect_to_hub(socket_path, CONNECT_RETRIES, CONNECT_RETRY_BASE_MS, true).await?
        } else {
            eprintln!("[botster-mcp] Hub socket disconnected, reconnecting...");
            match connect_to_hub(socket_path, RECONNECT_RETRIES, RECONNECT_RETRY_MS, false).await {
                Ok(s) => {
                    eprintln!("[botster-mcp] Reconnected to hub");
                    s
                }
                Err(e) => {
                    eprintln!(
                        "[botster-mcp] Failed to reconnect after {RECONNECT_RETRIES} attempts: {e}"
                    );
                    return Err(e);
                }
            }
        };

        match run_session(stream, &mut stdin_rx).await? {
            SessionExit::StdinClosed => return Ok(()),
            SessionExit::HubDisconnected => {} // loop → reconnect
        }
    }
}

/// Run a single MCP session over an already-connected hub stream.
///
/// Returns `Ok(SessionExit::StdinClosed)` when Claude Code closes stdin —
/// the caller should exit the process.
///
/// Returns `Ok(SessionExit::HubDisconnected)` when the hub socket drops —
/// the caller should reconnect and call this again with a fresh stream.
///
/// Returns `Err(...)` only for unrecoverable errors (e.g. stdout write failure,
/// which implies Claude Code is also gone).
async fn run_session(
    stream: tokio::net::UnixStream,
    stdin_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<SessionExit> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (read_half, write_half) = stream.into_split();

    // Channel for sending encoded frames to the hub
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    // Channel for receiving decoded JSON messages from the hub
    let (hub_msg_tx, mut hub_msg_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();

    // Subscribe to the MCP channel immediately, passing the caller's full
    // context so the hub can identify and exclude the calling agent.
    // Reuses the same resolution logic as `botster context` (env vars + context.json).
    let caller_context = crate::commands::context::build();
    let sub_frame = Frame::Json(json!({
        "type": "subscribe",
        "channel": "mcp",
        "subscriptionId": SUB_ID,
        "params": { "context": caller_context }
    }));
    // The write task has not been spawned yet — frame_rx is still in scope
    // and the send cannot fail. expect() makes the invariant explicit.
    frame_tx
        .send(sub_frame.encode())
        .expect("channel alive: write task not yet spawned");

    // Spawn hub socket writer task
    let write_task = tokio::spawn(async move {
        let mut writer = tokio::io::BufWriter::new(write_half);
        while let Some(data) = frame_rx.recv().await {
            if writer.write_all(&data).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    // Spawn hub socket reader task
    let read_task = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(read_half);
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    match decoder.feed(&buf[..n]) {
                        Ok(frames) => {
                            for frame in frames {
                                if let Frame::Json(v) = frame {
                                    if hub_msg_tx.send(v).is_err() {
                                        return;
                                    }
                                }
                                // Non-JSON frames are ignored (MCP is JSON-only)
                            }
                        }
                        Err(e) => {
                            eprintln!("[botster-mcp] Frame decode error: {e}");
                            break;
                        }
                    }
                }
            }
        }
    });

    // Pending requests: maps a lookup key to the original JSON-RPC id.
    //
    // Tools:
    //   "tools_list"          — pending tools/list response
    //   "call_{n}"            — pending tools/call response
    //
    // Prompts:
    //   "prompts_list"        — pending prompts/list response
    //   "prompt_get_{n}"      — pending prompts/get response
    //
    // Cleared automatically on return — stale requests from the previous
    // session will never get a response from a freshly reconnected hub.
    let mut pending_calls: HashMap<String, Value> = HashMap::new();
    let mut call_counter: u64 = 0;

    let mut stdout = io::stdout();

    let exit = loop {
        tokio::select! {
            // MCP JSON-RPC message from Claude on stdin
            msg = stdin_rx.recv() => {
                let Some(line) = msg else { break SessionExit::StdinClosed };

                let parsed: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[botster-mcp] Invalid JSON from stdin: {e}");
                        continue;
                    }
                };

                let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let id = parsed.get("id").cloned();

                match method {
                    "initialize" => {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "protocolVersion": "2025-03-26",
                                "capabilities": {
                                    "tools": {
                                        "listChanged": true
                                    },
                                    "prompts": {
                                        "listChanged": true
                                    }
                                },
                                "serverInfo": {
                                    "name": "botster-hub",
                                    "version": env!("CARGO_PKG_VERSION")
                                }
                            }
                        });
                        writeln!(stdout, "{}", response)?;
                        stdout.flush()?;
                    }

                    "notifications/initialized" => {
                        // Notification — no response required
                    }

                    "tools/list" => {
                        let req_frame = Frame::Json(json!({
                            "subscriptionId": SUB_ID,
                            "type": "tools_list"
                        }));
                        if frame_tx.send(req_frame.encode()).is_err() {
                            break SessionExit::HubDisconnected;
                        }

                        if let Some(id) = id {
                            pending_calls.insert("tools_list".to_string(), id);
                        }
                    }

                    "tools/call" => {
                        let params = parsed.get("params").cloned().unwrap_or(json!({}));
                        let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                        call_counter += 1;
                        let call_id = format!("call_{call_counter}");

                        let req_frame = Frame::Json(json!({
                            "subscriptionId": SUB_ID,
                            "type": "tool_call",
                            "call_id": call_id,
                            "name": tool_name,
                            "arguments": arguments
                        }));
                        if frame_tx.send(req_frame.encode()).is_err() {
                            break SessionExit::HubDisconnected;
                        }

                        if let Some(id) = id {
                            pending_calls.insert(call_id, id);
                        }
                    }

                    "prompts/list" => {
                        let req_frame = Frame::Json(json!({
                            "subscriptionId": SUB_ID,
                            "type": "prompts_list"
                        }));
                        if frame_tx.send(req_frame.encode()).is_err() {
                            break SessionExit::HubDisconnected;
                        }

                        if let Some(id) = id {
                            pending_calls.insert("prompts_list".to_string(), id);
                        }
                    }

                    "prompts/get" => {
                        let params = parsed.get("params").cloned().unwrap_or(json!({}));
                        let prompt_name =
                            params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                        call_counter += 1;
                        // Use a distinct prefix so prompt and tool pending keys never collide.
                        let call_id = format!("prompt_get_{call_counter}");

                        let req_frame = Frame::Json(json!({
                            "subscriptionId": SUB_ID,
                            "type": "prompt_get",
                            "call_id": call_id,
                            "name": prompt_name,
                            "arguments": arguments
                        }));
                        if frame_tx.send(req_frame.encode()).is_err() {
                            break SessionExit::HubDisconnected;
                        }

                        if let Some(id) = id {
                            pending_calls.insert(call_id, id);
                        }
                    }

                    _ => {
                        // Unknown method — respond with error if it has an id (request),
                        // silently ignore if it's a notification (no id)
                        if let Some(id) = id {
                            let err_response = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32601,
                                    "message": format!("Method not found: {method}")
                                }
                            });
                            writeln!(stdout, "{}", err_response)?;
                            stdout.flush()?;
                        }
                    }
                }
            }

            // Message from the hub socket
            msg = hub_msg_rx.recv() => {
                let Some(hub_msg) = msg else { break SessionExit::HubDisconnected };

                let msg_type = hub_msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match msg_type {
                    "subscribed" => {
                        eprintln!("[botster-mcp] Connected to hub, MCP channel subscribed");
                    }

                    "tools_list" => {
                        if let Some(jsonrpc_id) = pending_calls.remove("tools_list") {
                            let tools = hub_msg.get("tools").cloned().unwrap_or(json!([]));

                            let mcp_tools: Vec<Value> = tools
                                .as_array()
                                .map(|arr| {
                                    arr.iter()
                                        .map(|t| {
                                            json!({
                                                "name": t.get("name").unwrap_or(&json!("")),
                                                "description": t.get("description").unwrap_or(&json!("")),
                                                "inputSchema": t.get("input_schema").unwrap_or(&json!({"type": "object"}))
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();

                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": jsonrpc_id,
                                "result": {
                                    "tools": mcp_tools
                                }
                            });
                            writeln!(stdout, "{}", response)?;
                            stdout.flush()?;
                        }
                    }

                    "tool_result" => {
                        let call_id =
                            hub_msg.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
                        if let Some(jsonrpc_id) = pending_calls.remove(call_id) {
                            let content = hub_msg.get("content").cloned().unwrap_or(json!([]));
                            let is_error = hub_msg
                                .get("is_error")
                                .and_then(|e| e.as_bool())
                                .unwrap_or(false);

                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": jsonrpc_id,
                                "result": {
                                    "content": content,
                                    "isError": is_error
                                }
                            });
                            writeln!(stdout, "{}", response)?;
                            stdout.flush()?;
                        }
                    }

                    "tools_list_changed" => {
                        // Proactive notification to Claude that tools have changed
                        let notification = json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/tools/list_changed"
                        });
                        writeln!(stdout, "{}", notification)?;
                        stdout.flush()?;
                    }

                    "prompts_list" => {
                        if let Some(jsonrpc_id) = pending_calls.remove("prompts_list") {
                            let prompts = hub_msg.get("prompts").cloned().unwrap_or(json!([]));

                            // Map internal prompt shape to MCP wire shape.
                            // Internal: { name, description, arguments: [{ name, description, required }] }
                            // MCP:      { name, description, arguments: [{ name, description, required }] }
                            // The shapes match — pass through directly.
                            let response = json!({
                                "jsonrpc": "2.0",
                                "id": jsonrpc_id,
                                "result": {
                                    "prompts": prompts
                                }
                            });
                            writeln!(stdout, "{}", response)?;
                            stdout.flush()?;
                        }
                    }

                    "prompt_result" => {
                        let call_id = hub_msg
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        if let Some(jsonrpc_id) = pending_calls.remove(call_id) {
                            let is_error = hub_msg
                                .get("is_error")
                                .and_then(|e| e.as_bool())
                                .unwrap_or(false);

                            let response = if is_error {
                                // Error path: return a standard JSON-RPC error
                                let content =
                                    hub_msg.get("content").cloned().unwrap_or(json!([]));
                                let err_text = content
                                    .as_array()
                                    .and_then(|a| a.first())
                                    .and_then(|c| c.get("text"))
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("prompt error");
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": jsonrpc_id,
                                    "error": {
                                        "code": -32603,
                                        "message": err_text
                                    }
                                })
                            } else {
                                let description =
                                    hub_msg.get("description").cloned().unwrap_or(json!(""));
                                let messages =
                                    hub_msg.get("messages").cloned().unwrap_or(json!([]));
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": jsonrpc_id,
                                    "result": {
                                        "description": description,
                                        "messages": messages
                                    }
                                })
                            };
                            writeln!(stdout, "{}", response)?;
                            stdout.flush()?;
                        }
                    }

                    "prompts_list_changed" => {
                        // Proactive notification to Claude that prompts have changed
                        let notification = json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/prompts/list_changed"
                        });
                        writeln!(stdout, "{}", notification)?;
                        stdout.flush()?;
                    }

                    _ => {
                        // Ignore other hub messages (e.g. heartbeats, PTY data)
                    }
                }
            }
        }
    };

    write_task.abort();
    read_task.abort();

    // If the hub disconnected, synthesize JSON-RPC errors for every in-flight
    // request so Claude Code doesn't hang waiting for responses that will
    // never arrive. The hub has no memory of the old session; pending calls
    // from before the disconnect will never be fulfilled.
    if matches!(exit, SessionExit::HubDisconnected) {
        for (_, jsonrpc_id) in pending_calls.drain() {
            let err = json!({
                "jsonrpc": "2.0",
                "id": jsonrpc_id,
                "error": {
                    "code": -32000,
                    "message": "MCP server disconnected from hub — reconnecting"
                }
            });
            writeln!(stdout, "{}", err)?;
            stdout.flush()?;
        }
    }

    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::{run_session, SessionExit, SUB_ID};
    use crate::socket::framing::{Frame, FrameDecoder};
    use tokio::io::AsyncReadExt;
    use tokio::sync::mpsc::unbounded_channel;

    /// Create a connected Unix socket pair. The first element is used as the
    /// mcp-serve side (passed to `run_session`), the second as the hub side
    /// (controlled by the test).
    fn socket_pair() -> (tokio::net::UnixStream, tokio::net::UnixStream) {
        tokio::net::UnixStream::pair().expect("UnixStream::pair")
    }

    /// When the hub closes its socket, `run_session` must return
    /// `HubDisconnected` — not exit the process — so the caller can reconnect.
    ///
    /// This is the primary scenario the reconnect fix addresses: before the
    /// fix, `hub_msg_rx.recv()` returning `None` caused a silent `break` that
    /// propagated `Ok(())` all the way out, terminating the process.
    #[tokio::test]
    async fn test_hub_eof_returns_hub_disconnected() {
        let (mcp_side, hub_side) = socket_pair();
        let (_stdin_tx, mut stdin_rx) = unbounded_channel::<String>();

        // Drop hub side immediately — delivers EOF to the session's read task.
        drop(hub_side);

        let exit = run_session(mcp_side, &mut stdin_rx).await.unwrap();
        assert!(
            matches!(exit, SessionExit::HubDisconnected),
            "hub EOF must return HubDisconnected, not exit the process"
        );
    }

    /// When stdin closes (Claude Code exits), `run_session` must return
    /// `StdinClosed` so the outer loop exits cleanly without reconnecting.
    #[tokio::test]
    async fn test_stdin_eof_returns_stdin_closed() {
        let (mcp_side, _hub_side) = socket_pair();
        let (stdin_tx, mut stdin_rx) = unbounded_channel::<String>();

        // Drop the sender — makes stdin_rx.recv() return None immediately.
        // _hub_side stays alive so hub_msg_rx never fires first.
        drop(stdin_tx);

        let exit = run_session(mcp_side, &mut stdin_rx).await.unwrap();
        assert!(
            matches!(exit, SessionExit::StdinClosed),
            "stdin EOF must return StdinClosed"
        );
    }

    /// A subscribe frame is sent to the hub at the start of every session.
    ///
    /// After a hub reconnect the hub needs a fresh subscription — this
    /// verifies the frame arrives on the hub side before any other
    /// communication and carries the expected fields.
    #[tokio::test]
    async fn test_subscribe_frame_sent_on_session_start() {
        let (mcp_side, hub_side) = socket_pair();
        let (_stdin_tx, mut stdin_rx) = unbounded_channel::<String>();

        // Hub: read the first frame, then return (dropping hub_side causes
        // the session's read task to see EOF and exit with HubDisconnected).
        let hub_task = tokio::spawn(async move {
            let mut first_frame: Option<serde_json::Value> = None;
            let mut reader = tokio::io::BufReader::new(hub_side);
            let mut decoder = FrameDecoder::new();
            let mut buf = [0u8; 4096];
            'read: loop {
                match reader.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(frames) = decoder.feed(&buf[..n]) {
                            for frame in frames {
                                if let Frame::Json(v) = frame {
                                    first_frame = Some(v);
                                    break 'read;
                                }
                            }
                        }
                    }
                }
            }
            first_frame
        });

        // Session exits once hub_task drops hub_side.
        let exit = run_session(mcp_side, &mut stdin_rx).await.unwrap();
        assert!(matches!(exit, SessionExit::HubDisconnected));

        let frame = hub_task
            .await
            .unwrap()
            .expect("hub must receive the subscribe frame");
        assert_eq!(frame["type"], "subscribe", "first frame must be a subscribe");
        assert_eq!(frame["channel"], "mcp");
        assert_eq!(frame["subscriptionId"], SUB_ID);
    }

    /// `stdin_rx` is shared across sessions — messages buffered between
    /// sessions are delivered to the next session.
    ///
    /// This is the core correctness property for reconnects: Claude Code does
    /// not restart mcp-serve on hub disconnects, so all in-flight stdin
    /// messages must survive into the next session and not be silently dropped.
    #[tokio::test]
    async fn test_stdin_rx_shared_across_sessions() {
        let (stdin_tx, mut stdin_rx) = unbounded_channel::<String>();

        // Session 1: hub disconnects immediately.
        let (mcp_side1, hub_side1) = socket_pair();
        drop(hub_side1);
        let exit1 = run_session(mcp_side1, &mut stdin_rx).await.unwrap();
        assert!(matches!(exit1, SessionExit::HubDisconnected));

        // Send a notification *after* session 1 ended. Session 2 must see it.
        // notifications/initialized requires no response, so the session
        // processes it silently and continues until stdin closes.
        stdin_tx
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string())
            .unwrap();

        // Close stdin — after processing the one queued message, session 2
        // will see None from stdin_rx and return StdinClosed.
        drop(stdin_tx);

        // Session 2: hub stays open for the duration so hub_msg_rx never
        // fires. Only stdin drives the exit.
        let (mcp_side2, _hub_side2) = socket_pair();
        let exit2 = run_session(mcp_side2, &mut stdin_rx).await.unwrap();
        assert!(
            matches!(exit2, SessionExit::StdinClosed),
            "session 2 must exit via StdinClosed after processing the buffered message"
        );
    }
}
