use std::collections::{HashMap, HashSet};


const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const MAX_BUFFER_BYTES: usize = 512;

#[allow(dead_code)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ObservedProbe {
    pub probe: TerminalProbe,
    pub query: Vec<u8>,
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

}

#[derive(Debug, Default)]
pub(crate) struct TerminalProfileStore {
    input_buffers: HashMap<String, Vec<u8>>,
    peer_input_buffers: HashMap<String, Vec<u8>>,
    output_buffers: HashMap<String, Vec<u8>>,
    /// Hub-wide terminal profile learned from the boot probe or first client.
    /// Used to answer probes for headless sessions (no live client attached).
    hub_profile: TerminalProfile,
    /// Probes sent to a client at attach time, awaiting replies.
    hub_pending: HashSet<TerminalProbe>,
}

impl TerminalProfileStore {
    #[allow(dead_code)]
    pub(crate) fn hub_profile_is_complete(&self) -> bool {
        self.hub_profile.is_complete()
    }

    pub(crate) fn describe_hub_profile(&self) -> String {
        format!(
            "fg={} bg={} cursor={} complete={}",
            format_optional_seq(self.hub_profile.get_reply(TerminalProbe::DefaultForeground)),
            format_optional_seq(self.hub_profile.get_reply(TerminalProbe::DefaultBackground)),
            format_optional_seq(self.hub_profile.get_reply(TerminalProbe::DefaultCursorColor)),
            self.hub_profile.is_complete()
        )
    }

    fn store_hub_reply(&mut self, probe: TerminalProbe, reply: Vec<u8>) {
        self.hub_profile.store_reply(probe, reply);
        log::info!(
            "[PTY-PROBE] Updated hub cache from {}: {}",
            probe.label(),
            self.describe_hub_profile()
        );
    }

    pub(crate) fn observe_peer_input(&mut self, peer_id: &str, data: &[u8]) -> Vec<Vec<u8>> {
        let should_parse = {
            let buffer = self.peer_input_buffers.entry(peer_id.to_string()).or_default();
            if buffer.is_empty() && !data.iter().any(|byte| *byte == ESC) {
                false
            } else {
                append_with_limit(buffer, data);
                true
            }
        };
        if !should_parse {
            return Vec::new();
        }

        let sequences = {
            let buffer = self
                .peer_input_buffers
                .get_mut(peer_id)
                .expect("peer buffer inserted above");
            extract_complete_osc_sequences(buffer)
        };

        let mut learned = Vec::new();
        for seq in sequences {
            if let Some((probe, reply)) = classify_osc_reply(&seq) {
                self.store_hub_reply(probe, reply.clone());
                learned.push(reply);
            }
        }

        if self
            .peer_input_buffers
            .get(peer_id)
            .is_some_and(|buffer| buffer.is_empty())
        {
            self.peer_input_buffers.remove(peer_id);
        }

        learned
    }

    pub(crate) fn observe_input(&mut self, session_uuid: &str, peer_id: &str, data: &[u8]) {
        let key = format!("{session_uuid}:{peer_id}");
        let has_hub_pending = !self.hub_pending.is_empty();
        if !has_hub_pending {
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

        let mut hub_accepted = Vec::new();
        for seq in sequences {
            if let Some((probe, reply)) = classify_osc_reply(&seq) {
                if self.hub_pending.remove(&probe) {
                    hub_accepted.push((probe, reply));
                }
            }
        }

        if self
            .input_buffers
            .get(&key)
            .is_some_and(|buffer| buffer.is_empty())
        {
            self.input_buffers.remove(&key);
        }

        if !hub_accepted.is_empty() {
            for (probe, reply) in hub_accepted {
                self.store_hub_reply(probe, reply);
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn observe_output(
        &mut self,
        session_uuid: &str,
        data: &[u8],
    ) -> Vec<ObservedProbe> {
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
                probes.push(ObservedProbe { probe, query: seq });
            }
        }
        probes
    }

    #[allow(dead_code)]
    pub(crate) fn headless_reply(&self, session_uuid: &str, probe: TerminalProbe) -> Option<&[u8]> {
        let _ = session_uuid;
        self.hub_profile.get_reply(probe)
    }

    /// Build a shared color cache from the boot probe results.
    ///
    /// Returns a shared `Arc<Mutex<HashMap>>` that all `HubEventListener`
    /// instances reference. When a `ColorRequest` fires, the listener looks
    /// up the cached RGB value and formats the response immediately.
    pub(crate) fn shared_color_cache(&self) -> std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, alacritty_terminal::vte::ansi::Rgb>>> {
        /// Foreground dynamic color index in alacritty's color table.
        const IDX_FOREGROUND: usize = 256;
        /// Background dynamic color index in alacritty's color table.
        const IDX_BACKGROUND: usize = 257;
        /// Cursor dynamic color index in alacritty's color table.
        const IDX_CURSOR: usize = 258;

        let mut colors = std::collections::HashMap::new();
        for (probe, index) in [
            (TerminalProbe::DefaultForeground, IDX_FOREGROUND),
            (TerminalProbe::DefaultBackground, IDX_BACKGROUND),
            (TerminalProbe::DefaultCursorColor, IDX_CURSOR),
        ] {
            if let Some(reply) = self.hub_profile.get_reply(probe) {
                if let Some(rgb) = parse_rgb_from_osc_reply(reply) {
                    colors.insert(index, rgb);
                }
            }
        }
        std::sync::Arc::new(std::sync::Mutex::new(colors))
    }

    /// Returns OSC probe bytes to inject into a client's output stream if the
    /// hub-level terminal profile is incomplete. Clears any stale pending state
    /// so disconnected clients don't block future probing.
    /// Marks returned probes as hub_pending so replies are captured by `observe_input`.
    #[allow(dead_code)]
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
        use std::io::Write;

        if crate::env::is_test_mode() {
            return;
        }

        // Only probe if stdin is a TTY (not piped/redirected)
        if !atty::is(atty::Stream::Stdin) {
            log::debug!("[PTY-PROBE] Skipping boot probe: stdin is not a TTY");
            return;
        }

        // Put stdin into raw mode before sending the queries so terminal
        // replies are not echoed by the tty line discipline.
        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if !was_raw {
            let _ = crossterm::terminal::enable_raw_mode();
        }

        // Send OSC 10/11/12 queries
        let probes = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07";
        if std::io::stdout().write_all(probes).is_err() {
            if !was_raw {
                let _ = crossterm::terminal::disable_raw_mode();
            }
            return;
        }
        let _ = std::io::stdout().flush();

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
            log::info!(
                "[PTY-PROBE] Probed spawning terminal: learned {} color values; {}",
                learned,
                self.describe_hub_profile()
            );
        }
    }

    pub(crate) fn clear_session(&mut self, session_uuid: &str) {
        self.output_buffers.remove(session_uuid);

        let prefix = format!("{session_uuid}:");
        self.input_buffers
            .retain(|key, _| !key.starts_with(&prefix));
    }

}

impl TerminalProbe {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DefaultForeground => "foreground",
            Self::DefaultBackground => "background",
            Self::DefaultCursorColor => "cursor",
        }
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

/// Strip OSC 10/11/12 query bytes from PTY output while preserving all other
/// output and buffering incomplete query fragments across chunks.
pub(crate) fn strip_osc_queries_from_output(buffer: &mut Vec<u8>, data: &[u8]) -> Vec<u8> {
    append_with_limit(buffer, data);
    if buffer.is_empty() {
        return Vec::new();
    }

    let input = std::mem::take(buffer);
    let mut output = Vec::with_capacity(input.len());
    let mut idx = 0usize;

    while idx < input.len() {
        if input[idx] == ESC {
            if idx + 1 >= input.len() {
                buffer.extend_from_slice(&input[idx..]);
                break;
            }

            if input[idx + 1] == b']' {
                let mut scan = idx + 2;
                let mut end = None;

                while scan < input.len() {
                    if input[scan] == BEL {
                        end = Some(scan + 1);
                        break;
                    }
                    if input[scan] == ESC {
                        if scan + 1 >= input.len() {
                            break;
                        }
                        if input[scan + 1] == b'\\' {
                            end = Some(scan + 2);
                            break;
                        }
                    }
                    scan += 1;
                }

                let Some(end_idx) = end else {
                    buffer.extend_from_slice(&input[idx..]);
                    break;
                };

                let seq = &input[idx..end_idx];
                if classify_osc_query(seq).is_none() {
                    output.extend_from_slice(seq);
                }
                idx = end_idx;
                continue;
            }
        }

        output.push(input[idx]);
        idx += 1;
    }

    output
}

pub(crate) fn describe_probe_sequences(data: &[u8]) -> Vec<String> {
    let mut buffer = data.to_vec();
    extract_complete_osc_sequences(&mut buffer)
        .into_iter()
        .filter_map(|seq| {
            if let Some(probe) = classify_osc_query(&seq) {
                return Some(format!(
                    "query:{}:{}",
                    probe.label(),
                    format_seq(&seq)
                ));
            }
            if let Some((probe, reply)) = classify_osc_reply(&seq) {
                return Some(format!(
                    "reply:{}:{}",
                    probe.label(),
                    format_seq(&reply)
                ));
            }
            None
        })
        .collect()
}

fn format_optional_seq(data: Option<&[u8]>) -> String {
    data.map(format_seq)
        .unwrap_or_else(|| "<none>".to_string())
}

fn format_seq(data: &[u8]) -> String {
    data.iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect()
}

fn classify_osc_query(seq: &[u8]) -> Option<TerminalProbe> {
    let payload = osc_payload(seq)?;
    let (code, value) = payload.split_once_by(|b| *b == b';')?;
    if value != b"?" {
        return None;
    }
    probe_from_code(code)
}

fn classify_osc_reply(seq: &[u8]) -> Option<(TerminalProbe, Vec<u8>)> {
    let payload = osc_payload(seq)?;
    let (code, value) = payload.split_once_by(|b| *b == b';')?;
    let probe = probe_from_code(code)?;

    if value == b"?" || value.is_empty() {
        return None;
    }

    if !(value.starts_with(b"rgb:") || value.starts_with(b"#")) {
        return None;
    }

    Some((probe, seq.to_vec()))
}

/// Parse `rgb:RRRR/GGGG/BBBB` from a cached OSC reply into alacritty's `Rgb`.
///
/// The cached reply is a full OSC sequence like `ESC]10;rgb:1010/0f0f/0f0f BEL`.
/// Extracts the rgb: value and converts the 16-bit color components to 8-bit.
fn parse_rgb_from_osc_reply(reply: &[u8]) -> Option<alacritty_terminal::vte::ansi::Rgb> {
    let payload = osc_payload(reply)?;
    let (_code, value) = payload.split_once_by(|b| *b == b';')?;
    let rgb_str = value.strip_prefix(b"rgb:")?;
    let rgb_str = std::str::from_utf8(rgb_str).ok()?;
    let mut parts = rgb_str.split('/');
    let r = u16::from_str_radix(parts.next()?, 16).ok()?;
    let g = u16::from_str_radix(parts.next()?, 16).ok()?;
    let b = u16::from_str_radix(parts.next()?, 16).ok()?;
    // Terminal colors use 16-bit components (RRRR); convert to 8-bit.
    Some(alacritty_terminal::vte::ansi::Rgb {
        r: (r >> 8) as u8,
        g: (g >> 8) as u8,
        b: (b >> 8) as u8,
    })
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
    fn split_once_by<P>(&self, pred: P) -> Option<(&[u8], &[u8])>
    where
        P: FnMut(&u8) -> bool;
}

impl SplitOnceBytes for [u8] {
    fn split_once_by<P>(&self, mut pred: P) -> Option<(&[u8], &[u8])>
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

        // Set up hub_pending so observe_input will process the reply.
        let _probes = store.start_hub_probing();
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
            .observe_output("sess-1", b"\x1b]10;?")
            .is_empty());

        let probes = store.observe_output("sess-1", b"\x07hello");
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].probe, TerminalProbe::DefaultForeground);
        assert_eq!(probes[0].query, b"\x1b]10;?\x07");
    }

    #[test]
    fn ignores_non_reply_osc_sequences() {
        let mut store = TerminalProfileStore::default();

        store.observe_output("sess-1", b"\x1b]11;?\x07");
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
    fn observe_peer_input_updates_hub_fallback() {
        let mut store = TerminalProfileStore::default();

        let learned = store.observe_peer_input(
            "browser-a",
            b"\x1b]11;rgb:1111/2222/3333\x07",
        );

        assert_eq!(learned, vec![b"\x1b]11;rgb:1111/2222/3333\x07".to_vec()]);
        assert_eq!(
            store.headless_reply("any-session", TerminalProbe::DefaultBackground),
            Some(b"\x1b]11;rgb:1111/2222/3333\x07".as_slice())
        );
    }

    #[test]
    fn clear_session_removes_cached_state() {
        let mut store = TerminalProfileStore::default();

        store.observe_output("sess-1", b"\x1b]11;?\x07");
        assert!(store.output_buffers.get("sess-1").is_none() || store.output_buffers.get("sess-1").unwrap().is_empty());

        store.clear_session("sess-1");

        // After clear, output buffers for that session are gone.
        assert!(!store.output_buffers.contains_key("sess-1"));
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
    fn strip_osc_queries_preserves_other_output() {
        let mut buffer = Vec::new();
        let output = strip_osc_queries_from_output(
            &mut buffer,
            b"before\x1b]11;?\x07after\x1b]9;not-a-probe\x07",
        );

        assert_eq!(output, b"beforeafter\x1b]9;not-a-probe\x07");
        assert!(buffer.is_empty());
    }

    #[test]
    fn strip_osc_queries_handles_split_sequences() {
        let mut buffer = Vec::new();

        let first = strip_osc_queries_from_output(&mut buffer, b"ab\x1b]11;?");
        let second = strip_osc_queries_from_output(&mut buffer, b"\x07cd");

        assert_eq!(first, b"ab");
        assert_eq!(second, b"cd");
        assert!(buffer.is_empty());
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
