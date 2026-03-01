//! MCP stdio server bridge.
//!
//! Connects to a running hub via Unix socket, subscribes to the "mcp"
//! channel, and translates between MCP JSON-RPC on stdin/stdout and
//! the hub's internal socket protocol.
//!
//! Launched by Claude Code as: `botster mcp-serve --socket /path/to/hub.sock`

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
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

/// Run the MCP bridge synchronously, blocking on a tokio runtime.
pub fn run(socket_path: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { run_async(socket_path).await })
}

/// Core async event loop: connects to hub, subscribes to "mcp" channel,
/// and translates MCP JSON-RPC (stdin/stdout) to/from hub socket frames.
async fn run_async(socket_path: &str) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Connect to the hub's Unix domain socket, retrying on failure.
    // Retries handle the race between Claude Code restarting this process and
    // the hub briefly being unavailable (see CONNECT_RETRIES / CONNECT_RETRY_BASE_MS).
    let stream = {
        let mut last_err: Option<std::io::Error> = None;
        let mut conn: Option<tokio::net::UnixStream> = None;
        for attempt in 0..CONNECT_RETRIES {
            if attempt > 0 {
                let delay = std::time::Duration::from_millis(attempt as u64 * CONNECT_RETRY_BASE_MS);
                tokio::time::sleep(delay).await;
            }
            match tokio::net::UnixStream::connect(socket_path).await {
                Ok(s) => {
                    conn = Some(s);
                    break;
                }
                Err(e) => {
                    eprintln!(
                        "[botster-mcp] connect attempt {}/{} failed: {e}",
                        attempt + 1,
                        CONNECT_RETRIES
                    );
                    last_err = Some(e);
                }
            }
        }
        conn.with_context(|| {
            format!(
                "Failed to connect to hub socket after {CONNECT_RETRIES} attempts: {socket_path}: {}",
                last_err.map_or_else(|| "unknown error".to_owned(), |e| e.to_string())
            )
        })?
    };
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
    frame_tx.send(sub_frame.encode())?;

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

    // Spawn stdin reader on a blocking thread (stdin is synchronous)
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
                Ok(_) => {} // skip empty lines
                Err(_) => break,
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
    let mut pending_calls: HashMap<String, Value> = HashMap::new();
    let mut call_counter: u64 = 0;

    let mut stdout = io::stdout();

    loop {
        tokio::select! {
            // MCP JSON-RPC message from Claude on stdin
            msg = stdin_rx.recv() => {
                let Some(line) = msg else { break };

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
                        frame_tx.send(req_frame.encode())?;

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
                        frame_tx.send(req_frame.encode())?;

                        if let Some(id) = id {
                            pending_calls.insert(call_id, id);
                        }
                    }

                    "prompts/list" => {
                        let req_frame = Frame::Json(json!({
                            "subscriptionId": SUB_ID,
                            "type": "prompts_list"
                        }));
                        frame_tx.send(req_frame.encode())?;

                        if let Some(id) = id {
                            pending_calls.insert("prompts_list".to_string(), id);
                        }
                    }

                    "prompts/get" => {
                        let params = parsed.get("params").cloned().unwrap_or(json!({}));
                        let prompt_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
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
                        frame_tx.send(req_frame.encode())?;

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
                let Some(hub_msg) = msg else { break };

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
                        let call_id = hub_msg.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
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
    }

    write_task.abort();
    read_task.abort();
    Ok(())
}
