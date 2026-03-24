use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const MAX_BUFFER_BYTES: usize = 512;

const ALL_PROBES: [TerminalProbe; 3] = [
    TerminalProbe::DefaultForeground,
    TerminalProbe::DefaultBackground,
    TerminalProbe::DefaultCursorColor,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TerminalProbe {
    DefaultForeground,
    DefaultBackground,
    DefaultCursorColor,
}

#[derive(Debug, Default)]
struct TerminalProfile {
    default_foreground: Option<Vec<u8>>,
    default_background: Option<Vec<u8>>,
    default_cursor_color: Option<Vec<u8>>,
}

impl TerminalProfile {
    fn store_reply(&mut self, probe: TerminalProbe, reply: Vec<u8>) {
        match probe {
            TerminalProbe::DefaultForeground => self.default_foreground = Some(reply),
            TerminalProbe::DefaultBackground => self.default_background = Some(reply),
            TerminalProbe::DefaultCursorColor => self.default_cursor_color = Some(reply),
        }
    }

    fn get_reply(&self, probe: TerminalProbe) -> Option<&[u8]> {
        match probe {
            TerminalProbe::DefaultForeground => self.default_foreground.as_deref(),
            TerminalProbe::DefaultBackground => self.default_background.as_deref(),
            TerminalProbe::DefaultCursorColor => self.default_cursor_color.as_deref(),
        }
    }

    fn is_complete(&self) -> bool {
        self.default_foreground.is_some()
            && self.default_background.is_some()
            && self.default_cursor_color.is_some()
    }

    fn to_json(&self) -> serde_json::Value {
        use base64::Engine;
        let enc = base64::engine::general_purpose::STANDARD;
        serde_json::json!({
            "fg": self.default_foreground.as_ref().map(|b| enc.encode(b)),
            "bg": self.default_background.as_ref().map(|b| enc.encode(b)),
            "cursor": self.default_cursor_color.as_ref().map(|b| enc.encode(b)),
        })
    }

    fn from_json(value: &serde_json::Value) -> Self {
        use base64::Engine;
        let enc = base64::engine::general_purpose::STANDARD;
        let decode = |key: &str| -> Option<Vec<u8>> {
            value.get(key)?.as_str().and_then(|s| enc.decode(s).ok())
        };
        Self {
            default_foreground: decode("fg"),
            default_background: decode("bg"),
            default_cursor_color: decode("cursor"),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct TerminalProfileStore {
    profiles_by_session: HashMap<String, TerminalProfile>,
    pending_by_session: HashMap<String, HashSet<TerminalProbe>>,
    input_buffers: HashMap<String, Vec<u8>>,
    output_buffers: HashMap<String, Vec<u8>>,
    /// Hub-wide terminal profile learned from the first attached client.
    /// Used as fallback when a session has no local profile cached.
    hub_profile: TerminalProfile,
    /// Probes sent to a client at attach time, awaiting replies.
    hub_pending: HashSet<TerminalProbe>,
}

impl TerminalProfileStore {
    pub(crate) fn observe_input(&mut self, session_uuid: &str, peer_id: &str, data: &[u8]) {
        let key = format!("{session_uuid}:{peer_id}");
        let has_session_pending = self.pending_by_session.contains_key(session_uuid);
        let has_hub_pending = !self.hub_pending.is_empty();
        if !has_session_pending && !has_hub_pending {
            self.input_buffers.remove(&key);
            return;
        }

        let should_parse = {
            let buffer = self.input_buffers.entry(key.clone()).or_default();
            if buffer.is_empty() && !data.iter().any(|byte| *byte == ESC) {
                false
            } else {
                append_with_limit(buffer, data);
                true
            }
        };

        if !should_parse {
            return;
        }

        let sequences = {
            let buffer = self
                .input_buffers
                .get_mut(&key)
                .expect("buffer inserted above");
            extract_complete_osc_sequences(buffer)
        };

        let mut session_accepted = Vec::new();
        let mut hub_accepted = Vec::new();
        for seq in sequences {
            if let Some((probe, reply)) = classify_osc_reply(&seq) {
                let was_session_pending = self
                    .pending_by_session
                    .get_mut(session_uuid)
                    .map(|pending| pending.remove(&probe))
                    .unwrap_or(false);
                if was_session_pending {
                    session_accepted.push((probe, reply.clone()));
                }
                if self.hub_pending.remove(&probe) {
                    hub_accepted.push((probe, reply));
                }
            }
        }

        if self
            .pending_by_session
            .get(session_uuid)
            .is_some_and(HashSet::is_empty)
        {
            self.pending_by_session.remove(session_uuid);
        }
        if self
            .input_buffers
            .get(&key)
            .is_some_and(|buffer| buffer.is_empty())
        {
            self.input_buffers.remove(&key);
        }

        for (probe, reply) in session_accepted {
            self.profiles_by_session
                .entry(session_uuid.to_string())
                .or_default()
                .store_reply(probe, reply);
        }
        if !hub_accepted.is_empty() {
            for (probe, reply) in hub_accepted {
                self.hub_profile.store_reply(probe, reply);
            }
            self.save_hub_profile();
        }
    }

    pub(crate) fn observe_output(
        &mut self,
        session_uuid: &str,
        data: &[u8],
        track_pending: bool,
    ) -> Vec<TerminalProbe> {
        let buffer = self
            .output_buffers
            .entry(session_uuid.to_string())
            .or_default();
        if buffer.is_empty() && !data.iter().any(|byte| *byte == ESC) {
            return Vec::new();
        }
        append_with_limit(buffer, data);

        let mut probes = Vec::new();
        for seq in extract_complete_osc_sequences(buffer) {
            if let Some(probe) = classify_osc_query(&seq) {
                if track_pending {
                    self.pending_by_session
                        .entry(session_uuid.to_string())
                        .or_default()
                        .insert(probe);
                }
                probes.push(probe);
            }
        }
        probes
    }

    /// Process a terminal probe response from a client (e.g., TUI forwarding
    /// the outer terminal's reply). Matches replies against all sessions with
    /// pending probes and returns `(session_uuid, reply_bytes)` pairs for
    /// routing back to the PTYs that asked. Also updates the hub-level profile.
    pub(crate) fn route_probe_response(&mut self, data: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut buffer = data.to_vec();
        let sequences = extract_complete_osc_sequences(&mut buffer);
        let mut routed = Vec::new();
        let mut learned_any = false;

        for seq in sequences {
            let Some((probe, reply)) = classify_osc_reply(&seq) else {
                continue;
            };

            // Update hub profile for future headless fallback
            self.hub_profile.store_reply(probe, reply.clone());
            learned_any = true;

            // Find sessions waiting for this probe type and route the reply
            let mut matched_sessions = Vec::new();
            for (session_uuid, pending) in &mut self.pending_by_session {
                if pending.remove(&probe) {
                    matched_sessions.push(session_uuid.clone());
                }
            }

            for session_uuid in matched_sessions {
                // Also cache per-session
                self.profiles_by_session
                    .entry(session_uuid.clone())
                    .or_default()
                    .store_reply(probe, reply.clone());
                routed.push((session_uuid, reply.clone()));
            }

            // Clean up empty pending sets
            self.pending_by_session
                .retain(|_, pending| !pending.is_empty());
        }

        if learned_any {
            self.save_hub_profile();
        }

        routed
    }

    pub(crate) fn headless_reply(&self, session_uuid: &str, probe: TerminalProbe) -> Option<&[u8]> {
        self.profiles_by_session
            .get(session_uuid)
            .and_then(|p| p.get_reply(probe))
            .or_else(|| self.hub_profile.get_reply(probe))
    }

    /// Returns OSC probe bytes to inject into a client's output stream if the
    /// hub-level terminal profile is incomplete. Clears any stale pending state
    /// so disconnected clients don't block future probing.
    /// Marks returned probes as hub_pending so replies are captured by `observe_input`.
    pub(crate) fn start_hub_probing(&mut self) -> Option<Vec<u8>> {
        if self.hub_profile.is_complete() {
            return None;
        }
        // Clear stale pending probes from a previous client that disconnected
        // without replying. Re-probing is cheap (21 bytes) and idempotent.
        self.hub_pending.clear();

        let missing: Vec<TerminalProbe> = ALL_PROBES
            .iter()
            .copied()
            .filter(|p| self.hub_profile.get_reply(*p).is_none())
            .collect();

        if missing.is_empty() {
            return None;
        }

        let mut bytes = Vec::new();
        for probe in &missing {
            let code: &[u8] = match probe {
                TerminalProbe::DefaultForeground => b"10",
                TerminalProbe::DefaultBackground => b"11",
                TerminalProbe::DefaultCursorColor => b"12",
            };
            bytes.extend_from_slice(&[ESC, b']']);
            bytes.extend_from_slice(code);
            bytes.extend_from_slice(&[b';', b'?', BEL]);
        }

        for probe in missing {
            self.hub_pending.insert(probe);
        }

        Some(bytes)
    }

    /// Probe the spawning terminal directly for color information.
    ///
    /// Called at hub startup before the TUI takes over stdin/stdout.
    /// Writes OSC 10/11/12 queries to stdout, reads responses from stdin
    /// with a short timeout. Updates the hub profile and persists to disk.
    ///
    /// Skipped in test mode and when stdin is not a TTY.
    pub(crate) fn probe_spawning_terminal(&mut self) {
        use std::io::{Read, Write};

        if crate::env::is_test_mode() {
            return;
        }

        // Only probe if stdin is a TTY (not piped/redirected)
        if !atty::is(atty::Stream::Stdin) {
            log::debug!("[PTY-PROBE] Skipping boot probe: stdin is not a TTY");
            return;
        }

        // Send OSC 10/11/12 queries
        let probes = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07";
        if std::io::stdout().write_all(probes).is_err() {
            return;
        }
        let _ = std::io::stdout().flush();

        // Put stdin into raw mode briefly to read the response
        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if !was_raw {
            let _ = crossterm::terminal::enable_raw_mode();
        }

        // Read with a short timeout — terminal responds within milliseconds
        let mut response = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);

        // Use non-blocking reads with polling
        while std::time::Instant::now() < deadline {
            // Poll stdin for readability
            let mut pollfd = libc::pollfd {
                fd: 0, // stdin
                events: libc::POLLIN,
                revents: 0,
            };
            let remaining = deadline.duration_since(std::time::Instant::now());
            let timeout_ms = remaining.as_millis().min(50) as libc::c_int;

            let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if ready <= 0 {
                if !response.is_empty() {
                    break; // Got some data, no more coming
                }
                continue;
            }

            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            response.extend_from_slice(&buf[..n as usize]);

            // Check if we have all 3 responses
            let osc_count = response.iter().filter(|&&b| b == 0x07).count()
                + response.windows(2).filter(|w| w == &[0x1b, b'\\']).count();
            if osc_count >= 3 {
                break;
            }
        }

        if !was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }

        if response.is_empty() {
            log::debug!("[PTY-PROBE] No response from spawning terminal");
            return;
        }

        // Parse responses
        let mut buffer = response;
        let sequences = extract_complete_osc_sequences(&mut buffer);
        let mut learned = 0;
        for seq in sequences {
            if let Some((probe, reply)) = classify_osc_reply(&seq) {
                self.hub_profile.store_reply(probe, reply);
                learned += 1;
            }
        }

        if learned > 0 {
            self.save_hub_profile();
            log::info!(
                "[PTY-PROBE] Probed spawning terminal: learned {} color values (complete={})",
                learned,
                self.hub_profile.is_complete()
            );
        }
    }

    /// Load a previously persisted hub profile from disk.
    /// Called at hub startup so headless sessions can use it immediately.
    /// Skipped in test mode to avoid cross-test pollution.
    pub(crate) fn load_hub_profile(&mut self) {
        if crate::env::is_test_mode() {
            return;
        }
        let Some(path) = Self::hub_profile_path() else {
            return;
        };
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let value: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                log::warn!(
                    "[PTY-PROBE] Failed to parse hub profile {}: {}",
                    path.display(),
                    e
                );
                return;
            }
        };
        self.hub_profile = TerminalProfile::from_json(&value);
        log::info!(
            "[PTY-PROBE] Loaded hub terminal profile from {} (complete={})",
            path.display(),
            self.hub_profile.is_complete()
        );
    }

    /// Persist the hub profile to disk so it survives hub restarts.
    /// Skipped in test mode to avoid cross-test pollution.
    fn save_hub_profile(&self) {
        if crate::env::is_test_mode() {
            return;
        }
        let Some(path) = Self::hub_profile_path() else {
            return;
        };
        let value = self.hub_profile.to_json();
        let json = match serde_json::to_string_pretty(&value) {
            Ok(j) => j,
            Err(e) => {
                log::warn!("[PTY-PROBE] Failed to serialize hub profile: {}", e);
                return;
            }
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, json) {
            log::warn!(
                "[PTY-PROBE] Failed to write hub profile to {}: {}",
                path.display(),
                e
            );
        } else {
            log::debug!(
                "[PTY-PROBE] Saved hub terminal profile to {}",
                path.display()
            );
        }
    }

    fn hub_profile_path() -> Option<PathBuf> {
        crate::env::data_dir().map(|d| d.join("terminal_profile.json"))
    }

    pub(crate) fn clear_session(&mut self, session_uuid: &str) {
        self.profiles_by_session.remove(session_uuid);
        self.pending_by_session.remove(session_uuid);
        self.output_buffers.remove(session_uuid);

        let prefix = format!("{session_uuid}:");
        self.input_buffers
            .retain(|key, _| !key.starts_with(&prefix));
    }
}

fn append_with_limit(buffer: &mut Vec<u8>, data: &[u8]) {
    if data.len() >= MAX_BUFFER_BYTES {
        buffer.clear();
        buffer.extend_from_slice(&data[data.len() - MAX_BUFFER_BYTES..]);
        return;
    }

    let overflow = buffer
        .len()
        .saturating_add(data.len())
        .saturating_sub(MAX_BUFFER_BYTES);
    if overflow > 0 {
        buffer.drain(..overflow);
    }
    buffer.extend_from_slice(data);
}

fn extract_complete_osc_sequences(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut sequences = Vec::new();
    let mut idx = 0usize;
    let mut remainder_start = None;

    while idx + 1 < buffer.len() {
        if buffer[idx] == ESC && buffer[idx + 1] == b']' {
            let start = idx;
            let mut scan = idx + 2;
            let mut end = None;

            while scan < buffer.len() {
                if buffer[scan] == BEL {
                    end = Some(scan + 1);
                    break;
                }
                if buffer[scan] == ESC && scan + 1 < buffer.len() && buffer[scan + 1] == b'\\' {
                    end = Some(scan + 2);
                    break;
                }
                scan += 1;
            }

            if let Some(end_idx) = end {
                sequences.push(buffer[start..end_idx].to_vec());
                idx = end_idx;
                continue;
            }

            remainder_start = Some(start);
            break;
        }

        idx += 1;
    }

    let remainder = if let Some(start) = remainder_start {
        buffer[start..].to_vec()
    } else if buffer.last() == Some(&ESC) {
        vec![ESC]
    } else {
        Vec::new()
    };

    buffer.clear();
    buffer.extend_from_slice(&remainder);
    sequences
}

fn osc_payload(seq: &[u8]) -> Option<&[u8]> {
    if seq.len() < 4 || seq[0] != ESC || seq[1] != b']' {
        return None;
    }

    if seq.ends_with(&[BEL]) {
        return Some(&seq[2..seq.len() - 1]);
    }
    if seq.len() >= 4 && seq.ends_with(&[ESC, b'\\']) {
        return Some(&seq[2..seq.len() - 2]);
    }

    None
}

/// Extract complete OSC query sequences from a data buffer.
///
/// Used by `forward_probe_to_tui` to extract just the query bytes
/// (e.g., `ESC]10;?BEL`) from PTY output that may contain mixed content.
pub(crate) fn extract_osc_queries_from_output(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let sequences = extract_complete_osc_sequences(buffer);
    sequences
        .into_iter()
        .filter(|seq| classify_osc_query(seq).is_some())
        .collect()
}

fn classify_osc_query(seq: &[u8]) -> Option<TerminalProbe> {
    let payload = osc_payload(seq)?;
    let (code, value) = payload.split_once(|b| *b == b';')?;
    if value != b"?" {
        return None;
    }
    probe_from_code(code)
}

fn classify_osc_reply(seq: &[u8]) -> Option<(TerminalProbe, Vec<u8>)> {
    let payload = osc_payload(seq)?;
    let (code, value) = payload.split_once(|b| *b == b';')?;
    let probe = probe_from_code(code)?;

    if value == b"?" || value.is_empty() {
        return None;
    }

    if !(value.starts_with(b"rgb:") || value.starts_with(b"#")) {
        return None;
    }

    Some((probe, seq.to_vec()))
}

fn probe_from_code(code: &[u8]) -> Option<TerminalProbe> {
    match code {
        b"10" => Some(TerminalProbe::DefaultForeground),
        b"11" => Some(TerminalProbe::DefaultBackground),
        b"12" => Some(TerminalProbe::DefaultCursorColor),
        _ => None,
    }
}

trait SplitOnceBytes {
    fn split_once<P>(&self, pred: P) -> Option<(&[u8], &[u8])>
    where
        P: FnMut(&u8) -> bool;
}

impl SplitOnceBytes for [u8] {
    fn split_once<P>(&self, mut pred: P) -> Option<(&[u8], &[u8])>
    where
        P: FnMut(&u8) -> bool,
    {
        let idx = self.iter().position(&mut pred)?;
        Some((&self[..idx], &self[idx + 1..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_osc_color_replies_from_split_input() {
        let mut store = TerminalProfileStore::default();

        store.observe_output("sess-1", b"\x1b]11;?\x07", true);
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:0000/");
        store.observe_input("sess-1", "browser-a", b"0000/0000\x07");

        assert_eq!(
            store.headless_reply("sess-1", TerminalProbe::DefaultBackground),
            Some(b"\x1b]11;rgb:0000/0000/0000\x07".as_slice())
        );
    }

    #[test]
    fn extracts_split_osc_queries_from_output() {
        let mut store = TerminalProfileStore::default();

        assert!(store
            .observe_output("sess-1", b"\x1b]10;?", true)
            .is_empty());

        let probes = store.observe_output("sess-1", b"\x07hello", true);
        assert_eq!(probes, vec![TerminalProbe::DefaultForeground]);
    }

    #[test]
    fn ignores_non_reply_osc_sequences() {
        let mut store = TerminalProfileStore::default();

        store.observe_output("sess-1", b"\x1b]11;?\x07", true);
        store.observe_input("sess-1", "browser-a", b"\x1b]11;?\x07");
        store.observe_input("sess-1", "browser-a", b"\x1b]11;not-a-color\x07");

        assert_eq!(
            store.headless_reply("sess-1", TerminalProbe::DefaultBackground),
            None
        );
    }

    #[test]
    fn ignores_replies_without_pending_probe() {
        let mut store = TerminalProfileStore::default();

        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:1111/2222/3333\x07");

        assert_eq!(
            store.headless_reply("sess-1", TerminalProbe::DefaultBackground),
            None
        );
    }

    #[test]
    fn clear_session_removes_cached_state() {
        let mut store = TerminalProfileStore::default();

        store.observe_output("sess-1", b"\x1b]11;?\x07", true);
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:1111/2222/3333\x07");

        assert!(store
            .headless_reply("sess-1", TerminalProbe::DefaultBackground)
            .is_some());

        store.clear_session("sess-1");

        assert_eq!(
            store.headless_reply("sess-1", TerminalProbe::DefaultBackground),
            None
        );
    }

    // --- Hub-level proactive probing tests ---

    #[test]
    fn start_hub_probing_returns_all_three_osc_queries() {
        let mut store = TerminalProfileStore::default();

        let bytes = store
            .start_hub_probing()
            .expect("should return probe bytes");

        // Should contain OSC 10;? 11;? 12;?
        assert!(bytes.windows(7).any(|w| w == b"\x1b]10;?\x07"));
        assert!(bytes.windows(7).any(|w| w == b"\x1b]11;?\x07"));
        assert!(bytes.windows(7).any(|w| w == b"\x1b]12;?\x07"));
        assert_eq!(bytes.len(), 21); // 3 probes × 7 bytes each
    }

    #[test]
    fn start_hub_probing_re_probes_after_stale_pending() {
        let mut store = TerminalProfileStore::default();

        // First call sets up pending probes
        assert!(store.start_hub_probing().is_some());
        // Second call clears stale pending and re-probes (client may have disconnected)
        assert!(
            store.start_hub_probing().is_some(),
            "should re-probe after stale pending"
        );
    }

    #[test]
    fn start_hub_probing_returns_none_when_complete() {
        let mut store = TerminalProfileStore::default();

        // Populate hub profile by probing and replying
        let _bytes = store.start_hub_probing();
        store.observe_input("sess-1", "browser-a", b"\x1b]10;rgb:aaaa/bbbb/cccc\x07");
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:1111/2222/3333\x07");
        store.observe_input("sess-1", "browser-a", b"\x1b]12;rgb:4444/5555/6666\x07");

        assert!(
            store.start_hub_probing().is_none(),
            "should be None when hub profile is complete"
        );
    }

    #[test]
    fn hub_probe_replies_captured_from_client_input() {
        let mut store = TerminalProfileStore::default();

        let _bytes = store.start_hub_probing();

        // Client responds to the probes
        store.observe_input(
            "sess-1",
            "browser-a",
            b"\x1b]10;rgb:aaaa/bbbb/cccc\x07\x1b]11;rgb:1111/2222/3333\x07\x1b]12;rgb:4444/5555/6666\x07",
        );

        // Hub profile should now be populated
        assert!(store.hub_pending.is_empty());
        assert!(store.hub_profile.is_complete());
    }

    #[test]
    fn headless_reply_falls_back_to_hub_profile() {
        let mut store = TerminalProfileStore::default();

        // Populate hub profile
        let _bytes = store.start_hub_probing();
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:1111/2222/3333\x07");

        // A different session with no local profile should get hub fallback
        assert_eq!(
            store.headless_reply("sess-new", TerminalProbe::DefaultBackground),
            Some(b"\x1b]11;rgb:1111/2222/3333\x07".as_slice())
        );
    }

    #[test]
    fn session_profile_takes_precedence_over_hub_profile() {
        let mut store = TerminalProfileStore::default();

        // Populate hub profile
        let _bytes = store.start_hub_probing();
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:1111/2222/3333\x07");

        // Populate session-local profile with a different color
        store.observe_output("sess-2", b"\x1b]11;?\x07", true);
        store.observe_input("sess-2", "browser-b", b"\x1b]11;rgb:ffff/ffff/ffff\x07");

        // Session-local should win
        assert_eq!(
            store.headless_reply("sess-2", TerminalProbe::DefaultBackground),
            Some(b"\x1b]11;rgb:ffff/ffff/ffff\x07".as_slice())
        );
    }

    #[test]
    fn clear_session_does_not_clear_hub_profile() {
        let mut store = TerminalProfileStore::default();

        // Populate hub profile via sess-1
        let _bytes = store.start_hub_probing();
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:1111/2222/3333\x07");

        store.clear_session("sess-1");

        // Hub profile should survive session cleanup
        assert_eq!(
            store.headless_reply("sess-other", TerminalProbe::DefaultBackground),
            Some(b"\x1b]11;rgb:1111/2222/3333\x07".as_slice())
        );
    }

    #[test]
    fn hub_probe_replies_split_across_chunks() {
        let mut store = TerminalProfileStore::default();
        let _bytes = store.start_hub_probing();

        // Reply arrives split across two chunks
        store.observe_input("sess-1", "browser-a", b"\x1b]11;rgb:0000/");
        store.observe_input("sess-1", "browser-a", b"0000/0000\x07");

        assert_eq!(
            store
                .hub_profile
                .get_reply(TerminalProbe::DefaultBackground),
            Some(b"\x1b]11;rgb:0000/0000/0000\x07".as_slice())
        );
    }

    #[test]
    fn start_hub_probing_only_probes_missing_entries() {
        let mut store = TerminalProfileStore::default();

        // First round: probe all 3
        let _bytes = store.start_hub_probing();
        // Reply to 2 of 3
        store.observe_input(
            "sess-1",
            "browser-a",
            b"\x1b]10;rgb:aaaa/bbbb/cccc\x07\x1b]11;rgb:1111/2222/3333\x07",
        );

        // hub_pending should still have cursor color
        assert!(
            store.hub_pending.is_empty()
                || store
                    .hub_pending
                    .contains(&TerminalProbe::DefaultCursorColor)
        );

        // Clear pending so we can re-probe for the missing one
        store.hub_pending.clear();
        let bytes = store
            .start_hub_probing()
            .expect("should probe for missing cursor color");
        assert!(bytes.windows(7).any(|w| w == b"\x1b]12;?\x07"));
        // Should NOT re-probe already-answered ones
        assert!(!bytes.windows(7).any(|w| w == b"\x1b]10;?\x07"));
        assert!(!bytes.windows(7).any(|w| w == b"\x1b]11;?\x07"));
    }
}
