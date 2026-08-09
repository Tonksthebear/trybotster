//! Pre-Ghostty VT crash dumps for hard session deaths (SIGSEGV / abort).
//!
//! The session process writes these **before** each `ghostty_terminal_vt_write`
//! so a native crash still leaves the last bytes on disk. Hub logs the artifact
//! paths and a short hex preview when it reaps `signal=11` / `signal=6`.
//!
//! # Artifacts (under `sessions_socket_dir()`)
//!
//! | File | Contents |
//! |------|----------|
//! | `<uuid>.vtlast` | Raw bytes of the last PTY chunk about to be parsed |
//! | `<uuid>.vtmeta` | Text: seq, len, hex preview, printable preview |
//! | `<uuid>.vtring` | Rolling ring of recent VT output (default 256 KiB) |
//!
//! Disable with `BOTSTER_SESSION_VT_DUMP=0`.

// Rust guideline compliant 2026-08

use std::collections::VecDeque;
use std::fmt::Write as FmtWrite;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{sessions_socket_dir, write_session_progress};

/// Default ring capacity for recent VT bytes kept on disk.
pub const VT_RING_CAP_BYTES: usize = 256 * 1024;

/// Max bytes of hex dump written into `.vtmeta` (keeps the file readable).
const VT_META_HEX_CAP: usize = 512;

/// Max full ring rewrite rate when VT is streaming hard.
const VT_RING_FLUSH_MIN_INTERVAL_MS: u128 = 100;

/// Whether VT crash dumps are enabled for this process.
#[must_use]
pub fn vt_crash_dump_enabled() -> bool {
    match std::env::var("BOTSTER_SESSION_VT_DUMP") {
        Ok(v) => {
            let v = v.trim();
            !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
        }
        // Default on — we are hunting a Ghostty SIGSEGV.
        Err(_) => true,
    }
}

/// Path helpers for VT crash artifacts.
#[must_use]
pub fn session_vtlast_path(session_uuid: &str) -> Option<PathBuf> {
    sessions_socket_dir()
        .ok()
        .map(|d| d.join(format!("{session_uuid}.vtlast")))
}

/// Text metadata for the last VT chunk.
#[must_use]
pub fn session_vtmeta_path(session_uuid: &str) -> Option<PathBuf> {
    sessions_socket_dir()
        .ok()
        .map(|d| d.join(format!("{session_uuid}.vtmeta")))
}

/// Rolling binary ring of recent VT output.
#[must_use]
pub fn session_vtring_path(session_uuid: &str) -> Option<PathBuf> {
    sessions_socket_dir()
        .ok()
        .map(|d| d.join(format!("{session_uuid}.vtring")))
}

/// Hex-encode up to `cap` bytes; append `...(+N more)` when truncated.
#[must_use]
pub fn hex_preview(data: &[u8], cap: usize) -> String {
    if data.is_empty() {
        return String::from("(empty)");
    }
    let take = data.len().min(cap);
    let mut out = String::with_capacity(take * 3);
    for (i, b) in data.iter().take(take).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{b:02x}");
    }
    if data.len() > take {
        let _ = write!(out, " ...(+{} more)", data.len() - take);
    }
    out
}

/// Printable ASCII preview (non-printable → `.`).
#[must_use]
pub fn ascii_preview(data: &[u8], cap: usize) -> String {
    data.iter()
        .take(cap)
        .map(|b| {
            let c = *b as char;
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '.'
            }
        })
        .collect()
}

/// In-process VT dump state for one session reader.
pub struct VtCrashDumper {
    session_uuid: String,
    enabled: bool,
    ring: VecDeque<u8>,
    ring_cap: usize,
    seq: u64,
    last_ring_flush: std::time::Instant,
}

impl std::fmt::Debug for VtCrashDumper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtCrashDumper")
            .field("session_uuid", &self.session_uuid)
            .field("enabled", &self.enabled)
            .field("ring_len", &self.ring.len())
            .field("ring_cap", &self.ring_cap)
            .field("seq", &self.seq)
            .finish()
    }
}

impl VtCrashDumper {
    /// Create a dumper for `session_uuid`.
    #[must_use]
    pub fn new(session_uuid: impl Into<String>) -> Self {
        Self {
            session_uuid: session_uuid.into(),
            enabled: vt_crash_dump_enabled(),
            ring: VecDeque::with_capacity(8192),
            ring_cap: VT_RING_CAP_BYTES,
            seq: 0,
            last_ring_flush: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
        }
    }

    /// Whether dumps are active.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Record a PTY chunk **before** Ghostty `process()` / `vt_write`.
    ///
    /// Always overwrites `.vtlast` + `.vtmeta`. Rewrites `.vtring` at least
    /// every [`VT_RING_FLUSH_MIN_INTERVAL_MS`] so SEGV still leaves context.
    pub fn record_before_vt_write(&mut self, data: &[u8]) {
        if !self.enabled {
            return;
        }
        self.seq = self.seq.saturating_add(1);
        self.push_ring(data);
        self.write_last_chunk(data);
        self.write_meta(data, "before_vt_write");
        let due = self.last_ring_flush.elapsed().as_millis() >= VT_RING_FLUSH_MIN_INTERVAL_MS;
        if due || data.len() >= 512 {
            self.flush_ring();
        }
        write_session_progress(
            &self.session_uuid,
            &format!(
                "before_vt_write seq={} chunk={} ring={} hex={}",
                self.seq,
                data.len(),
                self.ring.len(),
                hex_preview(data, 48)
            ),
        );
    }

    /// Mark successful return from Ghostty `process()`.
    pub fn record_after_vt_write(&mut self, chunk_len: usize) {
        if !self.enabled {
            return;
        }
        write_session_progress(
            &self.session_uuid,
            &format!(
                "after_vt_write seq={} chunk={} ring={}",
                self.seq,
                chunk_len,
                self.ring.len()
            ),
        );
    }

    /// Mark entry into mode_poll after VT write (second Ghostty hot path).
    pub fn record_before_mode_poll(&mut self) {
        if !self.enabled {
            return;
        }
        write_session_progress(
            &self.session_uuid,
            &format!("before_mode_poll seq={}", self.seq),
        );
    }

    fn push_ring(&mut self, data: &[u8]) {
        for &b in data {
            if self.ring.len() >= self.ring_cap {
                self.ring.pop_front();
            }
            self.ring.push_back(b);
        }
    }

    fn write_last_chunk(&self, data: &[u8]) {
        let Some(path) = session_vtlast_path(&self.session_uuid) else {
            return;
        };
        let _ = std::fs::write(path, data);
    }

    fn write_meta(&self, data: &[u8], phase: &str) {
        let Some(path) = session_vtmeta_path(&self.session_uuid) else {
            return;
        };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let body = format!(
            "ts={ts}\n\
             phase={phase}\n\
             seq={}\n\
             chunk_len={}\n\
             ring_len={}\n\
             ring_cap={}\n\
             hex={}\n\
             ascii={}\n",
            self.seq,
            data.len(),
            self.ring.len(),
            self.ring_cap,
            hex_preview(data, VT_META_HEX_CAP),
            ascii_preview(data, 160),
        );
        let _ = std::fs::write(path, body);
    }

    fn flush_ring(&mut self) {
        let Some(path) = session_vtring_path(&self.session_uuid) else {
            return;
        };
        let bytes: Vec<u8> = self.ring.iter().copied().collect();
        let _ = std::fs::write(path, bytes);
        self.last_ring_flush = std::time::Instant::now();
    }
}

/// Hub-side: read and summarize crash artifacts for logging after a hard death.
#[derive(Debug, Clone)]
pub struct VtCrashArtifactSummary {
    /// Path to last chunk binary.
    pub vtlast_path: Option<PathBuf>,
    /// Path to text metadata.
    pub vtmeta_path: Option<PathBuf>,
    /// Path to ring buffer.
    pub vtring_path: Option<PathBuf>,
    /// Last-chunk length if readable.
    pub last_chunk_len: Option<usize>,
    /// Hex preview of last chunk.
    pub last_hex: Option<String>,
    /// Full `.vtmeta` body when small enough.
    pub meta_body: Option<String>,
    /// Ring file size on disk.
    pub ring_bytes: Option<usize>,
}

/// Load VT crash artifacts for hub logging after SIGSEGV/ABRT.
#[must_use]
pub fn summarize_vt_crash_artifacts(session_uuid: &str) -> VtCrashArtifactSummary {
    let vtlast_path = session_vtlast_path(session_uuid);
    let vtmeta_path = session_vtmeta_path(session_uuid);
    let vtring_path = session_vtring_path(session_uuid);

    let (last_chunk_len, last_hex) = vtlast_path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .map(|bytes| (Some(bytes.len()), Some(hex_preview(&bytes, 128))))
        .unwrap_or((None, None));

    let meta_body = vtmeta_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            if s.len() > 4096 {
                format!("{}…(truncated)", &s[..4096])
            } else {
                s
            }
        });

    let ring_bytes = vtring_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() as usize);

    VtCrashArtifactSummary {
        vtlast_path,
        vtmeta_path,
        vtring_path,
        last_chunk_len,
        last_hex,
        meta_body,
        ring_bytes,
    }
}

/// Whether this OS status should dump VT crash artifact details.
#[must_use]
pub fn signal_warrants_vt_dump(signal: Option<i32>) -> bool {
    matches!(signal, Some(libc::SIGSEGV) | Some(libc::SIGABRT) | Some(libc::SIGBUS) | Some(libc::SIGILL))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_preview_truncates() {
        let data: Vec<u8> = (0u8..20).collect();
        let s = hex_preview(&data, 4);
        assert!(s.starts_with("00 01 02 03"));
        assert!(s.contains("+16 more"));
    }

    #[test]
    fn ascii_preview_dots_controls() {
        let s = ascii_preview(b"ab\x1b[c", 16);
        assert_eq!(s, "ab.[c");
    }

    #[test]
    fn dumper_writes_artifacts_when_enabled() {
        std::env::set_var("BOTSTER_SESSION_VT_DUMP", "1");
        let uuid = format!(
            "sess-test-vtdump-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut dumper = VtCrashDumper::new(&uuid);
        assert!(dumper.enabled());
        dumper.record_before_vt_write(b"\x1b]0;title\x07hello");
        dumper.record_after_vt_write(12);

        let last = session_vtlast_path(&uuid).expect("path");
        let meta = session_vtmeta_path(&uuid).expect("path");
        assert!(last.exists(), "vtlast should exist");
        assert!(meta.exists(), "vtmeta should exist");
        let meta_body = std::fs::read_to_string(&meta).unwrap();
        assert!(meta_body.contains("before_vt_write"));
        assert!(meta_body.contains("hex="));

        // cleanup
        let _ = std::fs::remove_file(last);
        let _ = std::fs::remove_file(meta);
        if let Some(p) = session_vtring_path(&uuid) {
            let _ = std::fs::remove_file(p);
        }
        if let Some(p) = super::super::session_progress_path(&uuid).ok() {
            let _ = std::fs::remove_file(p);
        }
        std::env::remove_var("BOTSTER_SESSION_VT_DUMP");
    }

    #[test]
    fn signal_warrants_dump_for_segv() {
        assert!(signal_warrants_vt_dump(Some(libc::SIGSEGV)));
        assert!(!signal_warrants_vt_dump(Some(libc::SIGTERM)));
        assert!(!signal_warrants_vt_dump(None));
    }
}
