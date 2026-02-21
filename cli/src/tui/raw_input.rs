//! Raw stdin reader and byte-to-descriptor parser.
//!
//! Replaces crossterm's event reader with direct stdin reading. This preserves
//! the original raw bytes so unbound keys can be forwarded to the PTY without
//! lossy re-encoding.
//!
//! # Architecture
//!
//! ```text
//! stdin (fd 0, raw mode) → RawInputReader.drain_events()
//!     ├→ Key { descriptor, raw_bytes }   → Lua keybinding lookup
//!     ├→ MouseScroll { direction }       → TuiAction::ScrollUp/Down
//!     └→ Unrecognized sequences          → forwarded as Key with empty descriptor
//! ```
//!
//! The byte-to-descriptor parser produces the **same descriptor format** that
//! Lua `keybindings.lua` already uses: `"ctrl+p"`, `"shift+pageup"`, `"a"`, etc.
//! Lua keybindings work unchanged.
//!
//! # ESC Ambiguity
//!
//! Byte `0x1B` is both the Escape key and the start of escape sequences.
//! We use a 25ms timeout: if no follow-up byte arrives within 25ms, we emit
//! `"escape"`. Otherwise we parse the escape sequence.

// Rust guideline compliant 2026-02

use std::time::{Duration, Instant};

/// Default timeout for disambiguating bare ESC from escape sequences.
const ESC_TIMEOUT: Duration = Duration::from_millis(25);

/// Events produced by the raw input reader.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// Keyboard input with descriptor for Lua lookup and original raw bytes.
    Key {
        /// Descriptor string matching Lua keybinding format (e.g., "ctrl+p", "up").
        descriptor: String,
        /// Original raw bytes from stdin — forwarded to PTY if unbound.
        raw_bytes: Vec<u8>,
    },
    /// Mouse scroll event (from SGR mouse encoding).
    MouseScroll {
        /// Scroll direction (up or down).
        direction: ScrollDirection,
    },
    /// Outer terminal gained focus (`CSI I`).
    FocusGained,
    /// Outer terminal lost focus (`CSI O`).
    FocusLost,
}

/// Mouse scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    /// Scroll up (towards history).
    Up,
    /// Scroll down (towards present).
    Down,
}

/// Raw stdin reader with byte-to-descriptor parsing.
///
/// Reads directly from stdin fd 0 (which is already in raw mode via crossterm).
/// Parses byte sequences into descriptors for Lua keybinding lookup while
/// preserving the original bytes for PTY passthrough.
#[derive(Debug)]
pub struct RawInputReader {
    /// Pending bytes from previous drain that didn't form a complete sequence.
    pending: Vec<u8>,
    /// When we last saw a bare ESC byte (for timeout disambiguation).
    esc_start: Option<Instant>,
    /// Timeout for ESC disambiguation.
    esc_timeout: Duration,
}

impl RawInputReader {
    /// Create a new raw input reader with default ESC timeout.
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            esc_start: None,
            esc_timeout: ESC_TIMEOUT,
        }
    }

    /// Drain all available input events (non-blocking).
    ///
    /// Reads all available bytes from stdin using `libc::poll` + `read`,
    /// then parses them into events. Returns `(events, stdin_dead)` where
    /// `stdin_dead` is true if stdin returned a permanent error (EIO).
    pub fn drain_events(&mut self) -> (Vec<InputEvent>, bool) {
        // Read available bytes from stdin (non-blocking)
        let stdin_dead = self.read_available();

        // Parse pending bytes into events
        (self.parse_events(), stdin_dead)
    }

    /// Non-blocking read of all available bytes from stdin.
    ///
    /// Uses `libc::poll` + `libc::read` directly on fd 0. We deliberately
    /// bypass `std::io::stdin()` because its internal `BufReader` maintains
    /// a separate 8 KB buffer that gets out of sync with `libc::poll` —
    /// poll checks the kernel fd while BufReader may have already consumed
    /// bytes into its own buffer, causing poll to report "no data" when
    /// there are actually bytes waiting in BufReader.
    ///
    /// Returns `true` if stdin has a permanent error (EIO, EOF) — the
    /// caller should stop polling stdin to avoid a tight spin loop.
    ///
    // NOTE: If we add more POSIX syscall usage, consider switching from
    // raw `libc` to the `nix` crate for safe Rust wrappers (Result<T, Errno>
    // instead of manual errno checking). Zellij uses this pattern.
    fn read_available(&mut self) -> bool {
        let mut buf = [0u8; 1024];

        loop {
            // Check if stdin has data available (0ms timeout = non-blocking)
            let mut pollfd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };

            let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
            if ready <= 0 || (pollfd.revents & libc::POLLIN) == 0 {
                // Check for error flags even when POLLIN is not set — a permanent
                // error (POLLERR/POLLHUP/POLLNVAL) means stdin is dead.
                if ready > 0
                    && (pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
                {
                    log::error!(
                        "stdin poll error (revents=0x{:x}), stdin is dead",
                        pollfd.revents
                    );
                    return true;
                }
                break;
            }

            // Read directly from fd 0 — bypasses Rust's BufReader to stay
            // in sync with libc::poll above.
            let n = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };

            if n <= 0 {
                // 0 = EOF, negative = error (EAGAIN/EINTR/etc.)
                if n == 0 {
                    log::error!("stdin EOF — stdin is dead");
                    return true;
                }
                let errno = std::io::Error::last_os_error();
                if errno.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if errno.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                log::error!("stdin read error: {errno}");
                return true;
            }

            let chunk = &buf[..n as usize];
            // Log raw stdin bytes containing ESC to help diagnose focus event issues.
            if chunk.contains(&0x1b) {
                log::debug!("[STDIN-RAW] {} bytes: {:?}", chunk.len(), chunk);
            }
            self.pending.extend_from_slice(chunk);
        }
        false
    }

    /// Parse pending bytes into events.
    fn parse_events(&mut self) -> Vec<InputEvent> {
        let mut events = Vec::new();

        // Handle ESC timeout: if we have a pending ESC and timeout elapsed,
        // emit it as a bare Escape key.
        if let Some(start) = self.esc_start {
            if self.pending.is_empty() && start.elapsed() >= self.esc_timeout {
                self.esc_start = None;
                events.push(InputEvent::Key {
                    descriptor: "escape".to_string(),
                    raw_bytes: vec![0x1b],
                });
                return events;
            }
        }

        while !self.pending.is_empty() {
            match self.pending[0] {
                0x1b => {
                    if self.pending.len() == 1 {
                        // Just ESC — need to wait for timeout or more bytes
                        if self.esc_start.is_none() {
                            self.esc_start = Some(Instant::now());
                        } else if self.esc_start.unwrap().elapsed() >= self.esc_timeout {
                            self.esc_start = None;
                            self.pending.remove(0);
                            events.push(InputEvent::Key {
                                descriptor: "escape".to_string(),
                                raw_bytes: vec![0x1b],
                            });
                        }
                        // Either way, stop processing — waiting for more data or timeout
                        break;
                    }

                    self.esc_start = None;
                    match self.pending[1] {
                        b'[' => {
                            // CSI sequence: ESC [
                            if let Some(event) = self.parse_csi() {
                                events.push(event);
                            } else {
                                // Incomplete CSI — wait for more bytes
                                break;
                            }
                        }
                        b'O' => {
                            // SS3 sequence: ESC O
                            if let Some(event) = self.parse_ss3() {
                                events.push(event);
                            } else {
                                // Incomplete — wait for more bytes
                                break;
                            }
                        }
                        b => {
                            // Alt+key: ESC followed by a regular byte
                            let raw = vec![0x1b, b];
                            let descriptor = if b >= 0x01 && b <= 0x1a {
                                // Alt+Ctrl+letter
                                let ch = (b + b'@') as char;
                                format!("ctrl+alt+{}", ch.to_ascii_lowercase())
                            } else if b >= 0x20 && b <= 0x7e {
                                format!("alt+{}", (b as char).to_ascii_lowercase())
                            } else {
                                String::new() // Unknown — will pass through
                            };
                            self.pending.drain(..2);
                            events.push(InputEvent::Key {
                                descriptor,
                                raw_bytes: raw,
                            });
                        }
                    }
                }
                // Control characters: 0x01-0x1A (Ctrl+A..Z), 0x1C-0x1F (Ctrl+\ ] ^ _)
                // 0x1B (ESC) is handled above; 0x09 (Tab), 0x0D (Enter) are special-cased.
                b @ (0x01..=0x1a | 0x1c..=0x1f) => {
                    let raw = vec![b];
                    let descriptor = match b {
                        0x09 => "tab".to_string(),       // Ctrl+I = Tab
                        0x0d => "enter".to_string(),     // Ctrl+M = Enter (CR)
                        _ => {
                            let ch = (b + b'@') as char;
                            format!("ctrl+{}", ch.to_ascii_lowercase())
                        }
                    };
                    self.pending.remove(0);
                    events.push(InputEvent::Key {
                        descriptor,
                        raw_bytes: raw,
                    });
                }
                // Backspace (DEL)
                0x7f => {
                    self.pending.remove(0);
                    events.push(InputEvent::Key {
                        descriptor: "backspace".to_string(),
                        raw_bytes: vec![0x7f],
                    });
                }
                // Space
                0x20 => {
                    self.pending.remove(0);
                    events.push(InputEvent::Key {
                        descriptor: "space".to_string(),
                        raw_bytes: vec![0x20],
                    });
                }
                // Printable ASCII
                b @ 0x21..=0x7e => {
                    self.pending.remove(0);
                    events.push(InputEvent::Key {
                        descriptor: (b as char).to_string(),
                        raw_bytes: vec![b],
                    });
                }
                // UTF-8 multi-byte start
                b @ 0x80..=0xff => {
                    if let Some((ch, len)) = self.try_parse_utf8() {
                        let raw = self.pending[..len].to_vec();
                        self.pending.drain(..len);
                        events.push(InputEvent::Key {
                            descriptor: ch.to_string(),
                            raw_bytes: raw,
                        });
                    } else if self.pending.len() < 4 {
                        // Incomplete UTF-8 — wait for more bytes
                        break;
                    } else {
                        // Invalid UTF-8 — forward the byte as-is
                        let raw = vec![b];
                        self.pending.remove(0);
                        events.push(InputEvent::Key {
                            descriptor: String::new(),
                            raw_bytes: raw,
                        });
                    }
                }
                // Null byte or other
                b => {
                    self.pending.remove(0);
                    events.push(InputEvent::Key {
                        descriptor: String::new(),
                        raw_bytes: vec![b],
                    });
                }
            }
        }

        events
    }

    /// Try to parse a UTF-8 character from the start of pending bytes.
    /// Returns the character and its byte length, or None if incomplete.
    fn try_parse_utf8(&self) -> Option<(char, usize)> {
        let bytes = &self.pending;
        if bytes.is_empty() {
            return None;
        }
        // Determine expected UTF-8 length from first byte
        let expected_len = match bytes[0] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return None, // Invalid UTF-8 start byte
        };
        if bytes.len() < expected_len {
            return None; // Incomplete
        }
        match std::str::from_utf8(&bytes[..expected_len]) {
            Ok(s) => s.chars().next().map(|c| (c, expected_len)),
            Err(_) => None,
        }
    }

    /// Parse a CSI sequence (ESC [ ...).
    /// Returns an event if the sequence is complete, None if more bytes needed.
    fn parse_csi(&mut self) -> Option<InputEvent> {
        // Minimum: ESC [ <final> = 3 bytes
        if self.pending.len() < 3 {
            return None;
        }

        // Find the final byte (0x40-0x7E range)
        let start = 2; // Skip ESC [
        let mut final_pos = None;
        for i in start..self.pending.len() {
            let b = self.pending[i];
            if (0x40..=0x7e).contains(&b) {
                final_pos = Some(i);
                break;
            }
            // Parameter bytes (0x30-0x3F) and intermediate bytes (0x20-0x2F) continue
            if !((0x20..=0x3f).contains(&b)) {
                // Unexpected byte — malformed sequence
                let raw = self.pending[..=i].to_vec();
                self.pending.drain(..=i);
                return Some(InputEvent::Key {
                    descriptor: String::new(),
                    raw_bytes: raw,
                });
            }
        }

        let final_pos = final_pos?; // Incomplete — no final byte yet
        let seq_len = final_pos + 1;
        let raw = self.pending[..seq_len].to_vec();
        let params = &self.pending[start..final_pos];
        let final_byte = self.pending[final_pos];

        // Check for SGR mouse encoding: ESC [ < ...
        if !params.is_empty() && params[0] == b'<' {
            return self.parse_sgr_mouse(seq_len);
        }

        // Parse parameters (semicolon-separated numbers)
        let param_str = std::str::from_utf8(params).unwrap_or("");
        let param_parts: Vec<&str> = param_str.split(';').collect();

        // Decode modifier from parameter (CSI 1;mod X format)
        let modifier = if param_parts.len() >= 2 {
            param_parts[1].parse::<u8>().unwrap_or(1)
        } else {
            1 // No modifier
        };

        // Focus reporting: CSI I (gained) / CSI O (lost) — no parameters.
        if params.is_empty() {
            if final_byte == b'I' {
                self.pending.drain(..seq_len);
                return Some(InputEvent::FocusGained);
            }
            if final_byte == b'O' {
                self.pending.drain(..seq_len);
                return Some(InputEvent::FocusLost);
            }
        }

        let modifier_prefix = modifier_to_prefix(modifier);

        let descriptor = match final_byte {
            b'A' => format!("{modifier_prefix}up"),
            b'B' => format!("{modifier_prefix}down"),
            b'C' => format!("{modifier_prefix}right"),
            b'D' => format!("{modifier_prefix}left"),
            b'H' => format!("{modifier_prefix}home"),
            b'F' => format!("{modifier_prefix}end"),
            b'Z' => "backtab".to_string(), // Shift+Tab always reported as backtab
            b'~' => {
                // Tilde-terminated sequences: CSI <number> ~ or CSI <number>;<mod> ~
                let key_num = param_parts.first().and_then(|s| s.parse::<u8>().ok());
                match key_num {
                    Some(1) => format!("{modifier_prefix}home"),
                    Some(2) => format!("{modifier_prefix}insert"),
                    Some(3) => format!("{modifier_prefix}delete"),
                    Some(4) => format!("{modifier_prefix}end"),
                    Some(5) => format!("{modifier_prefix}pageup"),
                    Some(6) => format!("{modifier_prefix}pagedown"),
                    Some(15) => format!("{modifier_prefix}f5"),
                    Some(17) => format!("{modifier_prefix}f6"),
                    Some(18) => format!("{modifier_prefix}f7"),
                    Some(19) => format!("{modifier_prefix}f8"),
                    Some(20) => format!("{modifier_prefix}f9"),
                    Some(21) => format!("{modifier_prefix}f10"),
                    Some(23) => format!("{modifier_prefix}f11"),
                    Some(24) => format!("{modifier_prefix}f12"),
                    _ => String::new(),
                }
            }
            b'P' => format!("{modifier_prefix}f1"),
            b'Q' => format!("{modifier_prefix}f2"),
            b'R' => format!("{modifier_prefix}f3"),
            b'S' => format!("{modifier_prefix}f4"),
            // Kitty keyboard protocol: CSI <codepoint> ; <modifier> u
            // Enabled dynamically when the inner PTY pushes Kitty mode.
            // Raw bytes are forwarded as-is — the outer terminal's Kitty
            // state mirrors the inner PTY's, so the encoding always matches
            // what the PTY app expects.
            b'u' => {
                let codepoint = param_parts.first().and_then(|s| s.parse::<u32>().ok());
                kitty_codepoint_to_descriptor(codepoint, &modifier_prefix)
            }
            _ => String::new(), // Unknown CSI — will pass through
        };

        self.pending.drain(..seq_len);
        Some(InputEvent::Key {
            descriptor,
            raw_bytes: raw,
        })
    }

    /// Parse an SGR mouse sequence: ESC [ < Cb ; Cx ; Cy M/m
    fn parse_sgr_mouse(&mut self, seq_len: usize) -> Option<InputEvent> {
        let raw = self.pending[..seq_len].to_vec();
        let final_byte = self.pending[seq_len - 1];

        // Extract parameters: ESC [ < params M
        // params between '<' and final byte
        let param_str = String::from_utf8_lossy(&self.pending[3..seq_len - 1]).to_string();
        let parts: Vec<&str> = param_str.split(';').collect();

        self.pending.drain(..seq_len);

        if parts.len() < 3 {
            // Malformed — forward as raw
            return Some(InputEvent::Key {
                descriptor: String::new(),
                raw_bytes: raw,
            });
        }

        let button = parts[0].parse::<u16>().unwrap_or(0);

        // Scroll wheel: button 64 = up, 65 = down (only on press, 'M')
        if final_byte == b'M' && (button == 64 || button == 65) {
            return Some(InputEvent::MouseScroll {
                direction: if button == 64 {
                    ScrollDirection::Up
                } else {
                    ScrollDirection::Down
                },
            });
        }

        // Other mouse events — ignore (don't forward to PTY)
        None
    }

    /// Parse an SS3 sequence (ESC O ...).
    fn parse_ss3(&mut self) -> Option<InputEvent> {
        // Minimum: ESC O <final> = 3 bytes
        if self.pending.len() < 3 {
            return None;
        }

        let final_byte = self.pending[2];
        let raw = self.pending[..3].to_vec();

        let descriptor = match final_byte {
            b'A' => "up".to_string(),
            b'B' => "down".to_string(),
            b'C' => "right".to_string(),
            b'D' => "left".to_string(),
            b'H' => "home".to_string(),
            b'F' => "end".to_string(),
            b'P' => "f1".to_string(),
            b'Q' => "f2".to_string(),
            b'R' => "f3".to_string(),
            b'S' => "f4".to_string(),
            _ => String::new(),
        };

        self.pending.drain(..3);
        Some(InputEvent::Key {
            descriptor,
            raw_bytes: raw,
        })
    }
}

/// Convert CSI modifier byte to descriptor prefix string.
///
/// Standard xterm modifier encoding: value = 1 + bitmask where
/// bit 0 = Shift, bit 1 = Alt, bit 2 = Ctrl.
///
/// Descriptor prefix order matches existing format: `ctrl+shift+alt+`.
fn modifier_to_prefix(modifier: u8) -> String {
    if modifier <= 1 {
        return String::new();
    }
    let bits = modifier - 1;
    let mut parts = Vec::new();
    if bits & 4 != 0 {
        parts.push("ctrl");
    }
    if bits & 1 != 0 {
        parts.push("shift");
    }
    if bits & 2 != 0 {
        parts.push("alt");
    }
    if parts.is_empty() {
        String::new()
    } else {
        let mut prefix = parts.join("+");
        prefix.push('+');
        prefix
    }
}

/// Map a Kitty keyboard protocol codepoint to a descriptor string.
///
/// With `DISAMBIGUATE_ESCAPE_CODES` (the only flag we request), the terminal
/// sends `CSI <unicode-codepoint> ; <modifier> u` for keys that would
/// otherwise be ambiguous — mainly modified keys like shift+enter, ctrl+letter.
/// Unmodified printable keys still arrive as raw bytes.
fn kitty_codepoint_to_descriptor(codepoint: Option<u32>, modifier_prefix: &str) -> String {
    match codepoint {
        Some(9) if modifier_prefix.contains("shift") => {
            let reduced = modifier_prefix.replace("shift+", "");
            format!("{reduced}backtab")
        }
        Some(9) => format!("{modifier_prefix}tab"),
        Some(13) => format!("{modifier_prefix}enter"),
        Some(27) => format!("{modifier_prefix}escape"),
        Some(32) => format!("{modifier_prefix}space"),
        Some(127) => format!("{modifier_prefix}backspace"),
        // Printable ASCII — the common case for ctrl+letter, shift+key, etc.
        Some(cp @ 0x21..=0x7e) => {
            let ch = char::from(cp as u8);
            if modifier_prefix.is_empty() {
                ch.to_string()
            } else {
                format!("{modifier_prefix}{}", ch.to_ascii_lowercase())
            }
        }
        // Any other Unicode codepoint (including Kitty PUA functional keys)
        Some(cp) => {
            match char::from_u32(cp) {
                Some(ch) if modifier_prefix.is_empty() => ch.to_string(),
                Some(ch) => format!("{modifier_prefix}{ch}"),
                None => String::new(),
            }
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a reader pre-loaded with bytes (for testing without stdin).
    fn reader_with_bytes(bytes: &[u8]) -> RawInputReader {
        let mut reader = RawInputReader::new();
        reader.pending.extend_from_slice(bytes);
        reader
    }

    /// Helper to extract the first Key event's descriptor.
    fn first_descriptor(events: &[InputEvent]) -> &str {
        match &events[0] {
            InputEvent::Key { descriptor, .. } => descriptor,
            _ => panic!("Expected Key event"),
        }
    }

    /// Helper to extract the first Key event's raw bytes.
    fn first_raw_bytes(events: &[InputEvent]) -> &[u8] {
        match &events[0] {
            InputEvent::Key { raw_bytes, .. } => raw_bytes,
            _ => panic!("Expected Key event"),
        }
    }

    // === Plain Characters ===

    #[test]
    fn test_plain_char() {
        let mut r = reader_with_bytes(b"a");
        let events = r.parse_events();
        assert_eq!(events.len(), 1);
        assert_eq!(first_descriptor(&events), "a");
        assert_eq!(first_raw_bytes(&events), b"a");
    }

    #[test]
    fn test_digit() {
        let mut r = reader_with_bytes(b"5");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "5");
    }

    #[test]
    fn test_space() {
        let mut r = reader_with_bytes(b" ");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "space");
        assert_eq!(first_raw_bytes(&events), &[0x20]);
    }

    #[test]
    fn test_multiple_chars() {
        let mut r = reader_with_bytes(b"abc");
        let events = r.parse_events();
        assert_eq!(events.len(), 3);
        assert_eq!(first_descriptor(&events), "a");
        assert_eq!(first_descriptor(&events[1..]), "b");
        assert_eq!(first_descriptor(&events[2..]), "c");
    }

    // === Control Characters ===

    #[test]
    fn test_ctrl_c() {
        let mut r = reader_with_bytes(&[0x03]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+c");
        assert_eq!(first_raw_bytes(&events), &[0x03]);
    }

    #[test]
    fn test_ctrl_p() {
        let mut r = reader_with_bytes(&[0x10]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+p");
    }

    #[test]
    fn test_ctrl_q() {
        let mut r = reader_with_bytes(&[0x11]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+q");
    }

    #[test]
    fn test_lf_is_ctrl_j() {
        // 0x0A (LF) = Ctrl+J — no special-casing, falls through to default.
        // NOTE: Ghostty sends 0x0A for shift+enter even when kitty keyboard
        // protocol is active. TuiRunner remaps this at the dispatch level
        // (not here) because it depends on outer_kitty_enabled state.
        // See: https://github.com/ghostty-org/ghostty/issues/1850
        let mut r = reader_with_bytes(&[0x0a]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+j");
    }

    #[test]
    fn test_ctrl_bracket() {
        // Ctrl+] = 0x1D
        let mut r = reader_with_bytes(&[0x1d]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+]");
    }

    #[test]
    fn test_tab() {
        let mut r = reader_with_bytes(&[0x09]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "tab");
    }

    #[test]
    fn test_enter() {
        let mut r = reader_with_bytes(&[0x0d]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "enter");
    }

    #[test]
    fn test_backspace() {
        let mut r = reader_with_bytes(&[0x7f]);
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "backspace");
    }

    // === Escape Key ===

    #[test]
    fn test_bare_escape_pending() {
        // Single ESC with no timeout elapsed — should not emit yet
        let mut r = reader_with_bytes(&[0x1b]);
        let events = r.parse_events();
        assert_eq!(events.len(), 0);
        assert!(r.esc_start.is_some());
    }

    #[test]
    fn test_bare_escape_after_timeout() {
        let mut r = reader_with_bytes(&[0x1b]);
        r.esc_start = Some(Instant::now() - Duration::from_millis(50));
        let events = r.parse_events();
        assert_eq!(events.len(), 1);
        assert_eq!(first_descriptor(&events), "escape");
    }

    // === Arrow Keys (CSI) ===

    #[test]
    fn test_arrow_up() {
        let mut r = reader_with_bytes(b"\x1b[A");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "up");
        assert_eq!(first_raw_bytes(&events), b"\x1b[A");
    }

    #[test]
    fn test_arrow_down() {
        let mut r = reader_with_bytes(b"\x1b[B");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "down");
    }

    #[test]
    fn test_arrow_right() {
        let mut r = reader_with_bytes(b"\x1b[C");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "right");
    }

    #[test]
    fn test_arrow_left() {
        let mut r = reader_with_bytes(b"\x1b[D");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "left");
    }

    // === Arrow Keys (SS3 / Application Cursor Mode) ===

    #[test]
    fn test_ss3_arrow_up() {
        let mut r = reader_with_bytes(b"\x1bOA");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "up");
        assert_eq!(first_raw_bytes(&events), b"\x1bOA");
    }

    #[test]
    fn test_ss3_home() {
        let mut r = reader_with_bytes(b"\x1bOH");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "home");
    }

    // === Modified Keys ===

    #[test]
    fn test_shift_up() {
        // ESC [ 1 ; 2 A
        let mut r = reader_with_bytes(b"\x1b[1;2A");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "shift+up");
    }

    #[test]
    fn test_ctrl_up() {
        // ESC [ 1 ; 5 A
        let mut r = reader_with_bytes(b"\x1b[1;5A");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+up");
    }

    #[test]
    fn test_ctrl_shift_up() {
        // ESC [ 1 ; 6 A
        let mut r = reader_with_bytes(b"\x1b[1;6A");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+shift+up");
    }

    #[test]
    fn test_shift_pageup() {
        // ESC [ 5 ; 2 ~
        let mut r = reader_with_bytes(b"\x1b[5;2~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "shift+pageup");
    }

    #[test]
    fn test_shift_pagedown() {
        // ESC [ 6 ; 2 ~
        let mut r = reader_with_bytes(b"\x1b[6;2~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "shift+pagedown");
    }

    #[test]
    fn test_shift_home() {
        // ESC [ 1 ; 2 H
        let mut r = reader_with_bytes(b"\x1b[1;2H");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "shift+home");
    }

    #[test]
    fn test_shift_end() {
        // ESC [ 1 ; 2 F
        let mut r = reader_with_bytes(b"\x1b[1;2F");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "shift+end");
    }

    // === Special Keys ===

    #[test]
    fn test_home() {
        let mut r = reader_with_bytes(b"\x1b[H");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "home");
    }

    #[test]
    fn test_end() {
        let mut r = reader_with_bytes(b"\x1b[F");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "end");
    }

    #[test]
    fn test_pageup() {
        let mut r = reader_with_bytes(b"\x1b[5~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "pageup");
    }

    #[test]
    fn test_pagedown() {
        let mut r = reader_with_bytes(b"\x1b[6~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "pagedown");
    }

    #[test]
    fn test_delete() {
        let mut r = reader_with_bytes(b"\x1b[3~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "delete");
    }

    #[test]
    fn test_insert() {
        let mut r = reader_with_bytes(b"\x1b[2~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "insert");
    }

    #[test]
    fn test_backtab() {
        let mut r = reader_with_bytes(b"\x1b[Z");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "backtab");
    }

    // === Function Keys ===

    #[test]
    fn test_f1_ss3() {
        let mut r = reader_with_bytes(b"\x1bOP");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "f1");
    }

    #[test]
    fn test_f5() {
        let mut r = reader_with_bytes(b"\x1b[15~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "f5");
    }

    #[test]
    fn test_f12() {
        let mut r = reader_with_bytes(b"\x1b[24~");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "f12");
    }

    // === Mouse Scroll ===

    #[test]
    fn test_sgr_scroll_up() {
        // ESC [ < 64 ; 10 ; 5 M
        let mut r = reader_with_bytes(b"\x1b[<64;10;5M");
        let events = r.parse_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            InputEvent::MouseScroll {
                direction: ScrollDirection::Up
            }
        ));
    }

    #[test]
    fn test_sgr_scroll_down() {
        let mut r = reader_with_bytes(b"\x1b[<65;10;5M");
        let events = r.parse_events();
        assert!(matches!(
            events[0],
            InputEvent::MouseScroll {
                direction: ScrollDirection::Down
            }
        ));
    }

    // === Alt Keys ===

    #[test]
    fn test_alt_a() {
        let mut r = reader_with_bytes(b"\x1ba");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "alt+a");
        assert_eq!(first_raw_bytes(&events), b"\x1ba");
    }

    // === UTF-8 ===

    #[test]
    fn test_utf8_char() {
        // '你' is 3 bytes: 0xE4 0xBD 0xA0
        let mut r = reader_with_bytes("你".as_bytes());
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "你");
        assert_eq!(first_raw_bytes(&events), "你".as_bytes());
    }

    // === Raw Bytes Preservation ===

    #[test]
    fn test_raw_bytes_preserved_for_ctrl() {
        let mut r = reader_with_bytes(&[0x10]); // Ctrl+P
        let events = r.parse_events();
        assert_eq!(first_raw_bytes(&events), &[0x10]);
    }

    #[test]
    fn test_raw_bytes_preserved_for_csi() {
        let mut r = reader_with_bytes(b"\x1b[1;2A"); // Shift+Up
        let events = r.parse_events();
        assert_eq!(first_raw_bytes(&events), b"\x1b[1;2A");
    }

    // === Modifier Prefix ===

    #[test]
    fn test_modifier_to_prefix() {
        assert_eq!(modifier_to_prefix(1), "");       // No modifier
        assert_eq!(modifier_to_prefix(2), "shift+");  // Shift
        assert_eq!(modifier_to_prefix(3), "alt+");     // Alt
        assert_eq!(modifier_to_prefix(5), "ctrl+");    // Ctrl
        assert_eq!(modifier_to_prefix(6), "ctrl+shift+"); // Ctrl+Shift
        assert_eq!(modifier_to_prefix(7), "ctrl+alt+");   // Ctrl+Alt
        assert_eq!(modifier_to_prefix(8), "ctrl+shift+alt+"); // Ctrl+Shift+Alt
    }

    // === Unrecognized Sequences ===

    #[test]
    fn test_unrecognized_csi_passes_through() {
        // Unknown CSI final byte — should emit Key with empty descriptor
        let mut r = reader_with_bytes(b"\x1b[99x");
        let events = r.parse_events();
        assert_eq!(events.len(), 1);
        // Raw bytes preserved even for unknown sequences
        assert_eq!(first_raw_bytes(&events), b"\x1b[99x");
    }

    // === Kitty Keyboard Protocol (CSI <codepoint> ; <modifier> u) ===

    #[test]
    fn test_kitty_shift_enter() {
        // CSI 13 ; 2 u = shift+enter
        let mut r = reader_with_bytes(b"\x1b[13;2u");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "shift+enter");
    }

    #[test]
    fn test_kitty_ctrl_q() {
        // CSI 113 ; 5 u = ctrl+q (113 = 'q')
        let mut r = reader_with_bytes(b"\x1b[113;5u");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+q");
    }

    #[test]
    fn test_kitty_ctrl_shift_p() {
        // CSI 112 ; 6 u = ctrl+shift+p (112 = 'p', 6 = ctrl+shift)
        let mut r = reader_with_bytes(b"\x1b[112;6u");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+shift+p");
    }

    #[test]
    fn test_kitty_plain_enter() {
        // CSI 13 u = enter (no modifier)
        let mut r = reader_with_bytes(b"\x1b[13u");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "enter");
    }

    #[test]
    fn test_kitty_shift_tab() {
        // CSI 9 ; 2 u = shift+tab → backtab
        let mut r = reader_with_bytes(b"\x1b[9;2u");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "backtab");
    }

    #[test]
    fn test_kitty_ctrl_space() {
        // CSI 32 ; 5 u = ctrl+space
        let mut r = reader_with_bytes(b"\x1b[32;5u");
        let events = r.parse_events();
        assert_eq!(first_descriptor(&events), "ctrl+space");
    }

    #[test]
    fn test_kitty_shift_enter_raw_bytes() {
        // Kitty shift+enter (CSI 13 ; 2 u) — raw bytes preserved for PTY passthrough.
        // When Kitty is active, the PTY app understands this encoding natively.
        let mut r = reader_with_bytes(b"\x1b[13;2u");
        let events = r.parse_events();
        assert_eq!(first_raw_bytes(&events), b"\x1b[13;2u");
    }

    #[test]
    fn test_kitty_ctrl_q_raw_bytes() {
        // Kitty ctrl+q (CSI 113 ; 5 u) — raw bytes preserved.
        let mut r = reader_with_bytes(b"\x1b[113;5u");
        let events = r.parse_events();
        assert_eq!(first_raw_bytes(&events), b"\x1b[113;5u");
    }

    #[test]
    fn test_kitty_alt_a_raw_bytes() {
        // Kitty alt+a (CSI 97 ; 3 u) — raw bytes preserved.
        let mut r = reader_with_bytes(b"\x1b[97;3u");
        let events = r.parse_events();
        assert_eq!(first_raw_bytes(&events), b"\x1b[97;3u");
    }
}
