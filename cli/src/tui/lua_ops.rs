//! Typed Lua compound action operations.
//!
//! Replaces stringly-typed JSON matching in `execute_lua_ops()` with a proper
//! Rust enum. Operations are parsed once from `serde_json::Value` via
//! [`LuaOp::parse`], then dispatched by variant in the runner.

// Rust guideline compliant 2026-02

/// A typed operation returned by Lua compound action dispatch.
///
/// Each variant corresponds to an `op` string in the JSON protocol between
/// Lua's `actions.lua` and the Rust TUI runner. Parsing happens once via
/// [`LuaOp::parse`]; the runner matches on variants instead of raw strings.
#[derive(Debug)]
pub enum LuaOp {
    /// Update the mode shadow (canonical state lives in Lua's `_tui_state.mode`).
    SetMode {
        /// The new mode name (e.g. "insert", "menu", "agents").
        mode: String,
    },

    /// Send a JSON message to Hub via the Lua client protocol.
    SendMsg {
        /// The message payload forwarded to `TuiRequest::LuaMessage`.
        data: serde_json::Value,
    },

    /// Request TUI shutdown.
    Quit,

    /// Switch focus to a specific agent and PTY.
    FocusTerminal {
        /// Agent session key. `None` clears the selection.
        agent_id: Option<String>,
        /// Positional index of the agent in `_tui_state.agents`.
        agent_index: Option<usize>,
        /// PTY index within the agent (0 = CLI, 1 = server).
        pty_index: usize,
    },

    /// Cache connection code data (QR + URL) for display.
    SetConnectionCode {
        /// The connection URL.
        url: String,
        /// QR code rendered as ASCII lines.
        qr_ascii: Vec<String>,
    },

    /// Clear cached connection code.
    ClearConnectionCode,

    /// Send an OS-level notification via OSC escape sequences.
    OscAlert {
        /// Notification title (control characters stripped before emission).
        title: String,
        /// Notification body (control characters stripped before emission).
        body: String,
    },
}

impl LuaOp {
    /// Parse a JSON op value into a typed `LuaOp`.
    ///
    /// Returns `None` for unrecognized or malformed ops (logged as warnings).
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        let op_name = value.get("op").and_then(|v| v.as_str())?;

        match op_name {
            "set_mode" => {
                let mode = value.get("mode").and_then(|v| v.as_str())?;
                Some(Self::SetMode {
                    mode: mode.to_string(),
                })
            }
            "send_msg" => {
                let data = value.get("data")?.clone();
                Some(Self::SendMsg { data })
            }
            "quit" => Some(Self::Quit),
            "focus_terminal" => {
                let agent_id = value
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let agent_index = value
                    .get("agent_index")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let pty_index = value
                    .get("pty_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                Some(Self::FocusTerminal {
                    agent_id,
                    agent_index,
                    pty_index,
                })
            }
            "set_connection_code" => {
                let url = value.get("url").and_then(|v| v.as_str())?;
                let qr_array = value.get("qr_ascii").and_then(|v| v.as_array())?;
                let qr_ascii: Vec<String> = qr_array
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                Some(Self::SetConnectionCode {
                    url: url.to_string(),
                    qr_ascii,
                })
            }
            "clear_connection_code" => Some(Self::ClearConnectionCode),
            "osc_alert" => {
                let title = value
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Alert")
                    .to_string();
                let body = value
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(Self::OscAlert { title, body })
            }
            _ => {
                log::warn!("Unknown Lua compound op: {op_name}");
                None
            }
        }
    }

    /// Parse a vec of JSON ops, skipping unrecognized ones.
    pub fn parse_vec(ops: Vec<serde_json::Value>) -> Vec<Self> {
        ops.iter().filter_map(Self::parse).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_set_mode() {
        let val = json!({"op": "set_mode", "mode": "insert"});
        let op = LuaOp::parse(&val).expect("should parse");
        assert!(matches!(op, LuaOp::SetMode { mode } if mode == "insert"));
    }

    #[test]
    fn parse_send_msg() {
        let val = json!({"op": "send_msg", "data": {"type": "resize"}});
        let op = LuaOp::parse(&val).expect("should parse");
        assert!(matches!(op, LuaOp::SendMsg { .. }));
    }

    #[test]
    fn parse_quit() {
        let val = json!({"op": "quit"});
        let op = LuaOp::parse(&val).expect("should parse");
        assert!(matches!(op, LuaOp::Quit));
    }

    #[test]
    fn parse_focus_terminal_with_agent() {
        let val = json!({
            "op": "focus_terminal",
            "agent_id": "abc-123",
            "agent_index": 2,
            "pty_index": 1
        });
        let op = LuaOp::parse(&val).expect("should parse");
        match op {
            LuaOp::FocusTerminal {
                agent_id,
                agent_index,
                pty_index,
            } => {
                assert_eq!(agent_id.as_deref(), Some("abc-123"));
                assert_eq!(agent_index, Some(2));
                assert_eq!(pty_index, 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_focus_terminal_clear() {
        let val = json!({"op": "focus_terminal"});
        let op = LuaOp::parse(&val).expect("should parse");
        match op {
            LuaOp::FocusTerminal {
                agent_id,
                agent_index,
                pty_index,
            } => {
                assert!(agent_id.is_none());
                assert!(agent_index.is_none());
                assert_eq!(pty_index, 0);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_set_connection_code() {
        let val = json!({
            "op": "set_connection_code",
            "url": "https://example.com",
            "qr_ascii": ["##", "##"]
        });
        let op = LuaOp::parse(&val).expect("should parse");
        match op {
            LuaOp::SetConnectionCode { url, qr_ascii } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(qr_ascii.len(), 2);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_clear_connection_code() {
        let val = json!({"op": "clear_connection_code"});
        let op = LuaOp::parse(&val).expect("should parse");
        assert!(matches!(op, LuaOp::ClearConnectionCode));
    }

    #[test]
    fn parse_osc_alert() {
        let val = json!({"op": "osc_alert", "title": "Hello", "body": "World"});
        let op = LuaOp::parse(&val).expect("should parse");
        match op {
            LuaOp::OscAlert { title, body } => {
                assert_eq!(title, "Hello");
                assert_eq!(body, "World");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_unknown_op_returns_none() {
        let val = json!({"op": "unknown_thing"});
        assert!(LuaOp::parse(&val).is_none());
    }

    #[test]
    fn parse_missing_op_returns_none() {
        let val = json!({"mode": "insert"});
        assert!(LuaOp::parse(&val).is_none());
    }

    #[test]
    fn parse_set_mode_missing_mode_returns_none() {
        let val = json!({"op": "set_mode"});
        assert!(LuaOp::parse(&val).is_none());
    }

    #[test]
    fn parse_vec_filters_invalid() {
        let ops = vec![
            json!({"op": "quit"}),
            json!({"op": "bogus"}),
            json!({"op": "set_mode", "mode": "menu"}),
        ];
        let parsed = LuaOp::parse_vec(ops);
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], LuaOp::Quit));
        assert!(matches!(parsed[1], LuaOp::SetMode { .. }));
    }
}
