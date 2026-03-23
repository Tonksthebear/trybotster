use std::collections::{HashMap, HashSet};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const MAX_BUFFER_BYTES: usize = 512;

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
}

#[derive(Debug, Default)]
pub(crate) struct TerminalProfileStore {
    profiles_by_session: HashMap<String, TerminalProfile>,
    pending_by_session: HashMap<String, HashSet<TerminalProbe>>,
    input_buffers: HashMap<String, Vec<u8>>,
    output_buffers: HashMap<String, Vec<u8>>,
}

impl TerminalProfileStore {
    pub(crate) fn observe_input(&mut self, session_uuid: &str, peer_id: &str, data: &[u8]) {
        let key = format!("{session_uuid}:{peer_id}");
        if !self.pending_by_session.contains_key(session_uuid) {
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

        let mut accepted = Vec::new();
        for seq in sequences {
            if let Some((probe, reply)) = classify_osc_reply(&seq) {
                let was_pending = self
                    .pending_by_session
                    .get_mut(session_uuid)
                    .map(|pending| pending.remove(&probe))
                    .unwrap_or(false);
                if was_pending {
                    accepted.push((probe, reply));
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

        for (probe, reply) in accepted {
            self.profiles_by_session
                .entry(session_uuid.to_string())
                .or_default()
                .store_reply(probe, reply);
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

    pub(crate) fn headless_reply(&self, session_uuid: &str, probe: TerminalProbe) -> Option<&[u8]> {
        self.profiles_by_session.get(session_uuid)?.get_reply(probe)
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
}
