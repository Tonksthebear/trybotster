//! Offline Ghostty VT replay for crash fixtures (`.vtlast` / `.vtring`).
//!
//! Feeds captured PTY bytes through the same `TerminalParser::process` path the
//! session reader uses, so a SIGSEGV in `libghostty-vt` can be reproduced
//! without a live hub or agent.
//!
//! # Snapshot belt-and-suspenders
//!
//! Production attach **exports** a GHOSTSNP from the session and **imports** it
//! only on the client. Use [`SnapshotPhase`] to exercise:
//!
//! - pure VT (session death path we already hit)
//! - mid-stream export only (session attach side effect)
//! - mid-stream export → import on the same handle (stress Ghostty swap)
//! - client handoff: ring on A → export → import B → more VT on B
//!
//! # Example
//!
//! ```text
//! botster debug vt-replay --rows 70 --cols 226 \
//!   /tmp/botster-vt-min-repro/dir
//!
//! botster debug vt-replay --snapshot client --rows 70 --cols 226 \
//!   /tmp/botster-vt-min-repro/dir
//! ```

// Rust guideline compliant 2026-08

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::terminal::{TerminalParser, DEFAULT_SCROLLBACK_BYTES};

/// Chunk size used when streaming a ring file (matches session PTY read buffer).
pub const REPLAY_CHUNK_BYTES: usize = 4096;

/// When (if ever) to run snapshot export/import during replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapshotPhase {
    /// No snapshot calls (pure VT stream).
    #[default]
    None,
    /// After ring is fully fed, export only (session attach does this).
    ExportAfterRing,
    /// After ring is fully fed, export then import in-place on the same parser.
    RoundtripAfterRing,
    /// After `after_chunks` ring chunks, export then import in-place; then continue.
    RoundtripAfterChunks {
        /// Number of 4 KiB ring chunks to feed before the roundtrip.
        after_chunks: usize,
    },
    /// Feed ring on parser A, export, import into fresh parser B, feed last on B.
    ///
    /// Models browser/TUI: client imports GHOSTSNP then receives live VT.
    ClientHandoff,
}

/// Inputs for a VT replay run.
#[derive(Debug, Clone)]
pub struct VtReplayConfig {
    /// Terminal rows (session default paint was often 70).
    pub rows: u16,
    /// Terminal cols (browser attach often 226).
    pub cols: u16,
    /// Optional rolling ring (prior VT stream).
    pub ring_path: Option<PathBuf>,
    /// Last chunk that crashed (required unless only ring is replayed).
    pub last_path: Option<PathBuf>,
    /// Run mode_get polls after each write (session default).
    pub mode_poll: bool,
    /// Print progress to stderr after each chunk.
    pub verbose: bool,
    /// When true, skip feeding the ring and only feed `.vtlast`.
    pub last_only: bool,
    /// Snapshot export/import phase.
    pub snapshot: SnapshotPhase,
    /// Start at 24×80 then resize to target (spawn → attach shape).
    pub bootstrap_resize: bool,
}

impl Default for VtReplayConfig {
    fn default() -> Self {
        Self {
            rows: 70,
            cols: 226,
            ring_path: None,
            last_path: None,
            mode_poll: true,
            verbose: true,
            last_only: false,
            snapshot: SnapshotPhase::None,
            bootstrap_resize: false,
        }
    }
}

/// Resolve fixture paths from a file or crash-dump directory.
///
/// - Directory: looks for `*.vtlast` + optional `*.vtring` (or exact names).
/// - File ending in `.vtlast`: last only; sibling `.vtring` if present.
/// - Any other file: treated as raw VT bytes (last chunk).
pub fn resolve_fixture_paths(input: &Path) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    if input.is_dir() {
        let mut last = None;
        let mut ring = None;
        for entry in fs::read_dir(input)
            .with_context(|| format!("read crash dir {}", input.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".vtlast") {
                last = Some(path);
            } else if name.ends_with(".vtring") {
                ring = Some(path);
            }
        }
        if last.is_none() && ring.is_none() {
            bail!("no .vtlast or .vtring in directory {}", input.display());
        }
        return Ok((ring, last));
    }

    let name = input
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if name.ends_with(".vtlast") {
        let ring = input.with_extension("vtring");
        let ring = if ring.file_name().and_then(|n| n.to_str()) == Some("vtring") {
            None
        } else if ring.exists() {
            Some(ring)
        } else {
            let alt = input
                .to_string_lossy()
                .replacen(".vtlast", ".vtring", 1);
            let alt = PathBuf::from(alt);
            alt.exists().then_some(alt)
        };
        return Ok((ring, Some(input.to_path_buf())));
    }
    if name.ends_with(".vtring") {
        let last = PathBuf::from(input.to_string_lossy().replacen(".vtring", ".vtlast", 1));
        return Ok((Some(input.to_path_buf()), last.exists().then_some(last)));
    }

    Ok((None, Some(input.to_path_buf())))
}

/// Result summary after a successful (non-crashing) replay.
#[derive(Debug, Clone)]
pub struct VtReplayReport {
    /// Bytes fed from the ring.
    pub ring_bytes: usize,
    /// Chunks fed from the ring.
    pub ring_chunks: usize,
    /// Bytes fed from the last file.
    pub last_bytes: usize,
    /// Total `process()` calls.
    pub process_calls: usize,
    /// Mode polls run.
    pub mode_polls: usize,
    /// Snapshot exports performed.
    pub snapshot_exports: usize,
    /// Snapshot imports performed.
    pub snapshot_imports: usize,
    /// Bytes in the last exported GHOSTSNP (if any).
    pub last_snapshot_bytes: usize,
    /// Terminal cols after resize.
    pub cols: u16,
    /// Terminal rows after resize.
    pub rows: u16,
    /// Snapshot phase used.
    pub snapshot_phase: String,
}

/// Replay VT bytes into Ghostty with optional snapshot phases.
///
/// Progress is written to stderr **before** each risky call so a SIGSEGV
/// leaves a clear last line.
pub fn run_vt_replay(config: &VtReplayConfig) -> Result<VtReplayReport> {
    if config.ring_path.is_none() && config.last_path.is_none() {
        bail!("need at least one of ring_path or last_path");
    }

    let v = config.verbose;
    eprint_progress(
        v,
        &format!(
            "vt-replay: creating parser rows={} cols={} mode_poll={} snapshot={:?} bootstrap_resize={} scrollback={}",
            config.rows,
            config.cols,
            config.mode_poll,
            config.snapshot,
            config.bootstrap_resize,
            DEFAULT_SCROLLBACK_BYTES
        ),
    );

    let mut parser = if config.bootstrap_resize {
        eprint_progress(v, "vt-replay: bootstrap 24x80 then resize to target");
        let mut p = TerminalParser::new(24, 80, DEFAULT_SCROLLBACK_BYTES);
        p.resize(config.rows, config.cols);
        p
    } else {
        let mut p = TerminalParser::new(config.rows, config.cols, DEFAULT_SCROLLBACK_BYTES);
        p.resize(config.rows, config.cols);
        p
    };

    let mut report = VtReplayReport {
        ring_bytes: 0,
        ring_chunks: 0,
        last_bytes: 0,
        process_calls: 0,
        mode_polls: 0,
        snapshot_exports: 0,
        snapshot_imports: 0,
        last_snapshot_bytes: 0,
        cols: config.cols,
        rows: config.rows,
        snapshot_phase: format!("{:?}", config.snapshot),
    };

    let ring_bytes = if config.last_only {
        Vec::new()
    } else if let Some(ref ring_path) = config.ring_path {
        fs::read(ring_path).with_context(|| format!("read ring {}", ring_path.display()))?
    } else {
        Vec::new()
    };

    let last_bytes = if let Some(ref last_path) = config.last_path {
        fs::read(last_path).with_context(|| format!("read last {}", last_path.display()))?
    } else {
        Vec::new()
    };

    match config.snapshot {
        SnapshotPhase::ClientHandoff => {
            run_client_handoff(config, &ring_bytes, &last_bytes, &mut report)?;
        }
        other => {
            run_session_shaped(config, other, &ring_bytes, &last_bytes, &mut parser, &mut report)?;
        }
    }

    eprint_progress(
        v,
        &format!(
            "vt-replay: completed without crash process_calls={} ring_bytes={} last_bytes={} exports={} imports={} snap_bytes={}",
            report.process_calls,
            report.ring_bytes,
            report.last_bytes,
            report.snapshot_exports,
            report.snapshot_imports,
            report.last_snapshot_bytes
        ),
    );
    Ok(report)
}

fn run_session_shaped(
    config: &VtReplayConfig,
    phase: SnapshotPhase,
    ring: &[u8],
    last: &[u8],
    parser: &mut TerminalParser,
    report: &mut VtReplayReport,
) -> Result<()> {
    let v = config.verbose;
    let chunks: Vec<&[u8]> = ring.chunks(REPLAY_CHUNK_BYTES).collect();
    let mid_after = match phase {
        SnapshotPhase::RoundtripAfterChunks { after_chunks } => Some(after_chunks),
        _ => None,
    };

    if !ring.is_empty() {
        eprint_progress(
            v,
            &format!(
                "vt-replay: feeding ring ({} bytes, {} chunks of {})",
                ring.len(),
                chunks.len(),
                REPLAY_CHUNK_BYTES
            ),
        );
    }

    for (i, chunk) in chunks.iter().enumerate() {
        feed_chunk(config, parser, report, "ring", i, chunk)?;

        if mid_after == Some(i + 1) {
            eprint_progress(
                v,
                &format!("vt-replay: snapshot roundtrip AFTER ring_chunk={}", i),
            );
            snapshot_roundtrip(config, parser, report)?;
        }
    }

    match phase {
        SnapshotPhase::ExportAfterRing if !ring.is_empty() || !last.is_empty() => {
            eprint_progress(v, "vt-replay: snapshot EXPORT only after ring (session attach shape)");
            snapshot_export_only(config, parser, report)?;
        }
        SnapshotPhase::RoundtripAfterRing => {
            eprint_progress(v, "vt-replay: snapshot ROUNDTRIP after ring (in-place import)");
            snapshot_roundtrip(config, parser, report)?;
        }
        _ => {}
    }

    if !last.is_empty() {
        eprint_progress(
            v,
            &format!(
                "vt-replay: feeding LAST ({} bytes) — crash expected here if fixture is fatal",
                last.len()
            ),
        );
        eprint_progress(
            v,
            &format!(
                "vt-replay: BEFORE process last len={} hex_head={}",
                last.len(),
                crate::session::vt_crash_dump::hex_preview(last, 64)
            ),
        );
        parser.process(last);
        report.process_calls += 1;
        report.last_bytes = last.len();
        if config.mode_poll {
            eprint_progress(v, "vt-replay: BEFORE mode_poll after last");
            poll_modes(parser);
            report.mode_polls += 1;
            eprint_progress(v, "vt-replay: AFTER mode_poll after last");
        }
        eprint_progress(
            v,
            &format!("vt-replay: AFTER process last len={}", last.len()),
        );
    }

    Ok(())
}

/// Client-shaped: ring on A → export → import B → last on B.
fn run_client_handoff(
    config: &VtReplayConfig,
    ring: &[u8],
    last: &[u8],
    report: &mut VtReplayReport,
) -> Result<()> {
    let v = config.verbose;
    eprint_progress(
        v,
        "vt-replay: CLIENT HANDOFF — parser A feeds ring, export, import B, feed last on B",
    );

    let mut parser_a = if config.bootstrap_resize {
        let mut p = TerminalParser::new(24, 80, DEFAULT_SCROLLBACK_BYTES);
        p.resize(config.rows, config.cols);
        p
    } else {
        let mut p = TerminalParser::new(config.rows, config.cols, DEFAULT_SCROLLBACK_BYTES);
        p.resize(config.rows, config.cols);
        p
    };

    for (i, chunk) in ring.chunks(REPLAY_CHUNK_BYTES).enumerate() {
        feed_chunk(config, &mut parser_a, report, "ring_A", i, chunk)?;
    }

    eprint_progress(v, "vt-replay: BEFORE snapshot_export on parser A");
    let blob = parser_a
        .snapshot_export()
        .map_err(|e| anyhow::anyhow!("snapshot_export(A): {e}"))?;
    report.snapshot_exports += 1;
    report.last_snapshot_bytes = blob.len();
    eprint_progress(
        v,
        &format!(
            "vt-replay: AFTER snapshot_export on A bytes={} magic_ok={}",
            blob.len(),
            blob.starts_with(crate::ghostty_vt::GHOSTSNP_MAGIC)
        ),
    );

    eprint_progress(v, "vt-replay: creating parser B and BEFORE snapshot_import");
    let mut parser_b = TerminalParser::new(config.rows, config.cols, DEFAULT_SCROLLBACK_BYTES);
    parser_b
        .snapshot_import(&blob)
        .map_err(|e| anyhow::anyhow!("snapshot_import(B): {e}"))?;
    report.snapshot_imports += 1;
    eprint_progress(v, "vt-replay: AFTER snapshot_import on B");

    // Optional: also re-apply target size after import (attach resize race).
    eprint_progress(
        v,
        &format!(
            "vt-replay: resize B to {}x{} after import",
            config.cols, config.rows
        ),
    );
    parser_b.resize(config.rows, config.cols);

    if !last.is_empty() {
        eprint_progress(
            v,
            &format!(
                "vt-replay: BEFORE process last on B len={} (post-import live VT)",
                last.len()
            ),
        );
        parser_b.process(last);
        report.process_calls += 1;
        report.last_bytes = last.len();
        if config.mode_poll {
            poll_modes(&parser_b);
            report.mode_polls += 1;
        }
        eprint_progress(v, "vt-replay: AFTER process last on B");
    } else if !ring.is_empty() {
        // No separate last — re-feed tail of ring on B as "live" after import.
        let tail = ring.len().saturating_sub(REPLAY_CHUNK_BYTES);
        let live = &ring[tail..];
        eprint_progress(
            v,
            &format!(
                "vt-replay: BEFORE process ring_tail on B len={} (no .vtlast)",
                live.len()
            ),
        );
        parser_b.process(live);
        report.process_calls += 1;
        report.last_bytes = live.len();
        eprint_progress(v, "vt-replay: AFTER process ring_tail on B");
    }

    Ok(())
}

fn feed_chunk(
    config: &VtReplayConfig,
    parser: &mut TerminalParser,
    report: &mut VtReplayReport,
    label: &str,
    index: usize,
    chunk: &[u8],
) -> Result<()> {
    let v = config.verbose;
    eprint_progress(
        v,
        &format!(
            "vt-replay: BEFORE process {label}_chunk={} offset={} len={}",
            index,
            index * REPLAY_CHUNK_BYTES,
            chunk.len()
        ),
    );
    parser.process(chunk);
    report.process_calls += 1;
    if label.starts_with("ring") {
        report.ring_chunks += 1;
        report.ring_bytes += chunk.len();
    }
    if config.mode_poll {
        poll_modes(parser);
        report.mode_polls += 1;
    }
    eprint_progress(
        v,
        &format!(
            "vt-replay: AFTER process {label}_chunk={} len={}",
            index,
            chunk.len()
        ),
    );
    Ok(())
}

fn snapshot_export_only(
    config: &VtReplayConfig,
    parser: &mut TerminalParser,
    report: &mut VtReplayReport,
) -> Result<()> {
    let v = config.verbose;
    eprint_progress(v, "vt-replay: BEFORE snapshot_export");
    let blob = parser
        .snapshot_export()
        .map_err(|e| anyhow::anyhow!("snapshot_export: {e}"))?;
    report.snapshot_exports += 1;
    report.last_snapshot_bytes = blob.len();
    eprint_progress(
        v,
        &format!(
            "vt-replay: AFTER snapshot_export bytes={} magic_ok={}",
            blob.len(),
            blob.starts_with(crate::ghostty_vt::GHOSTSNP_MAGIC)
        ),
    );
    Ok(())
}

fn snapshot_roundtrip(
    config: &VtReplayConfig,
    parser: &mut TerminalParser,
    report: &mut VtReplayReport,
) -> Result<()> {
    snapshot_export_only(config, parser, report)?;
    let v = config.verbose;
    // Re-export for import payload (export_only already counted one export;
    // roundtrip needs the blob — re-export cheap for fixtures).
    let blob = parser
        .snapshot_export()
        .map_err(|e| anyhow::anyhow!("snapshot_export(roundtrip): {e}"))?;
    report.snapshot_exports += 1;
    report.last_snapshot_bytes = blob.len();
    eprint_progress(
        v,
        &format!(
            "vt-replay: BEFORE snapshot_import bytes={}",
            blob.len()
        ),
    );
    parser
        .snapshot_import(&blob)
        .map_err(|e| anyhow::anyhow!("snapshot_import: {e}"))?;
    report.snapshot_imports += 1;
    eprint_progress(v, "vt-replay: AFTER snapshot_import");
    // Attach often resizes around snapshot.
    parser.resize(config.rows, config.cols);
    eprint_progress(
        v,
        &format!(
            "vt-replay: AFTER post-import resize {}x{}",
            config.cols, config.rows
        ),
    );
    Ok(())
}

fn poll_modes(parser: &TerminalParser) {
    let _ = parser.cursor_hidden();
    let _ = parser.kitty_enabled();
    let _ = parser.bracketed_paste();
    let _ = parser.mouse_mode();
    let _ = parser.alt_screen_active();
    let _ = parser.focus_reporting();
    let _ = parser.application_cursor();
}

fn eprint_progress(verbose: bool, msg: &str) {
    if !verbose {
        return;
    }
    let _ = writeln!(io::stderr(), "{msg}");
    let _ = io::stderr().flush();
}

/// Parse CLI `--snapshot` value.
pub fn parse_snapshot_phase(s: &str) -> Result<SnapshotPhase> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s == "none" || s == "off" {
        return Ok(SnapshotPhase::None);
    }
    if s == "export" || s == "export-after-ring" || s == "attach" {
        return Ok(SnapshotPhase::ExportAfterRing);
    }
    if s == "roundtrip" || s == "roundtrip-after-ring" || s == "import" {
        return Ok(SnapshotPhase::RoundtripAfterRing);
    }
    if s == "client" || s == "handoff" || s == "client-handoff" {
        return Ok(SnapshotPhase::ClientHandoff);
    }
    if let Some(rest) = s.strip_prefix("after-chunks=") {
        let n: usize = rest
            .parse()
            .with_context(|| format!("invalid after-chunks value: {rest}"))?;
        return Ok(SnapshotPhase::RoundtripAfterChunks { after_chunks: n });
    }
    if let Some(rest) = s.strip_prefix("every=") {
        // Treat as roundtrip after first N chunks only (one stress point).
        let n: usize = rest
            .parse()
            .with_context(|| format!("invalid every value: {rest}"))?;
        return Ok(SnapshotPhase::RoundtripAfterChunks { after_chunks: n });
    }
    bail!(
        "unknown --snapshot value '{s}' (use: none|export|roundtrip|client|after-chunks=N)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_directory_finds_vtlast() {
        let dir = tempfile::tempdir().unwrap();
        let last = dir.path().join("sess-x.vtlast");
        let ring = dir.path().join("sess-x.vtring");
        fs::write(&last, b"abc").unwrap();
        fs::write(&ring, b"zzzz").unwrap();
        let (r, l) = resolve_fixture_paths(dir.path()).unwrap();
        assert_eq!(l.unwrap(), last);
        assert_eq!(r.unwrap(), ring);
    }

    #[test]
    fn replay_harmless_bytes_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let last = dir.path().join("t.vtlast");
        let payload = b"hello\n\x1b[0mworld\n";
        fs::write(&last, payload).unwrap();
        let report = run_vt_replay(&VtReplayConfig {
            rows: 24,
            cols: 80,
            ring_path: None,
            last_path: Some(last),
            mode_poll: true,
            verbose: false,
            last_only: true,
            snapshot: SnapshotPhase::None,
            bootstrap_resize: false,
        })
        .expect("harmless VT should not crash");
        assert_eq!(report.last_bytes, payload.len());
        assert_eq!(report.process_calls, 1);
    }

    #[test]
    fn snapshot_roundtrip_harmless_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let ring = dir.path().join("t.vtring");
        let last = dir.path().join("t.vtlast");
        // Enough VT to export a non-empty screen, then more.
        let mut stream = Vec::new();
        for _ in 0..100 {
            stream.extend_from_slice(b"line of text\r\n");
        }
        fs::write(&ring, &stream).unwrap();
        fs::write(&last, b"\x1b[0mdone\n").unwrap();
        let report = run_vt_replay(&VtReplayConfig {
            rows: 24,
            cols: 80,
            ring_path: Some(ring),
            last_path: Some(last),
            mode_poll: true,
            verbose: false,
            last_only: false,
            snapshot: SnapshotPhase::RoundtripAfterRing,
            bootstrap_resize: true,
        })
        .expect("roundtrip on harmless VT should succeed");
        assert!(report.snapshot_exports >= 1);
        assert_eq!(report.snapshot_imports, 1);
        assert!(report.last_snapshot_bytes > 0);
    }

    #[test]
    fn parse_snapshot_phase_values() {
        assert_eq!(parse_snapshot_phase("none").unwrap(), SnapshotPhase::None);
        assert_eq!(
            parse_snapshot_phase("export").unwrap(),
            SnapshotPhase::ExportAfterRing
        );
        assert_eq!(
            parse_snapshot_phase("client").unwrap(),
            SnapshotPhase::ClientHandoff
        );
        assert_eq!(
            parse_snapshot_phase("after-chunks=3").unwrap(),
            SnapshotPhase::RoundtripAfterChunks { after_chunks: 3 }
        );
    }

    /// Subprocess regression: real Botster capture that used to exit 139 (SIGSEGV).
    ///
    /// RED before silent-degrade pin; GREEN after vendor pin with no log on
    /// page-pressure hyperlink drop. Does **not** soft-skip: missing fixture
    /// or binary is a fail (reviewer false-green fix). Build the package bin
    /// first (`cargo build` / `./test.sh`) so `target/debug/botster` exists;
    /// lib tests do not set `CARGO_BIN_EXE_*`.
    #[test]
    fn botster_vt_crash_min_fixture_does_not_sigsegv() {
        use std::process::Command;

        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/vt_crash_min");
        assert!(
            fixture.join("x.vtring").is_file(),
            "missing VT crash fixture at {} (commit cli/testdata/vt_crash_min)",
            fixture.display()
        );

        let bin = resolve_botster_bin_for_vt_replay();
        assert!(
            bin.is_file(),
            "botster binary not found at {} — run `cargo build` in cli/ first \
             (lib tests do not set CARGO_BIN_EXE_botster)",
            bin.display()
        );

        let status = Command::new(&bin)
            .args([
                "debug",
                "vt-replay",
                "--quiet",
                "--rows",
                "70",
                "--cols",
                "226",
            ])
            .arg(&fixture)
            .status()
            .unwrap_or_else(|e| panic!("spawn {} debug vt-replay: {e}", bin.display()));

        // Unix: signal 11 -> wait status often reported as 139.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = status.signal() {
                panic!("vt-replay died on signal {sig} (SEGV would be 11)");
            }
        }
        assert!(
            status.success(),
            "vt-replay must exit 0 on min crash fixture, got {status:?} (bin={})",
            bin.display()
        );
    }

    fn resolve_botster_bin_for_vt_replay() -> PathBuf {
        if let Some(p) = option_env!("CARGO_BIN_EXE_botster") {
            return PathBuf::from(p);
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for rel in ["target/debug/botster", "target/release/botster"] {
            let candidate = manifest.join(rel);
            if candidate.is_file() {
                return candidate;
            }
        }
        manifest.join("target/debug/botster")
    }
}
