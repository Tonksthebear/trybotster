use super::*;

impl Hub {
    pub(super) fn ice_candidate_preview(candidate: &str) -> String {
        const MAX: usize = 220;
        let single_line = candidate.replace('\n', " ").replace('\r', " ");
        let char_count = single_line.chars().count();
        if char_count <= MAX {
            return single_line;
        }
        let truncated: String = single_line.chars().take(MAX).collect();
        format!("{truncated}...<truncated,len={char_count}>")
    }

    pub(super) fn restty_fixture_dump_dir() -> Option<std::path::PathBuf> {
        let raw = std::env::var("BOTSTER_DUMP_RESTTY_FIXTURES").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed == "0"
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.eq_ignore_ascii_case("off")
        {
            return None;
        }

        if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
            return Some(std::env::temp_dir());
        }

        Some(std::path::PathBuf::from(trimmed))
    }

    pub(super) fn restty_fixture_stem(session_uuid: &str) -> String {
        let sanitized: String = session_uuid
            .chars()
            .map(|ch| match ch {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
                _ => '_',
            })
            .collect();
        format!("botster-restty-{sanitized}")
    }

    pub(super) fn restty_fixture_preview_hex(data: &[u8]) -> String {
        const LIMIT: usize = 24;
        let preview = data
            .iter()
            .take(LIMIT)
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("");
        if data.len() > LIMIT {
            format!("{preview}...")
        } else {
            preview
        }
    }

    pub(super) fn write_restty_fixture_file(path: &std::path::Path, data: &[u8]) {
        use std::io::Write;

        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!(
                "[ResttyFixture] Failed to create dump dir {}: {}",
                parent.display(),
                e
            );
            return;
        }

        match std::fs::File::create(path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(data) {
                    log::warn!("[ResttyFixture] Failed to write {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                log::warn!("[ResttyFixture] Failed to create {}: {}", path.display(), e);
            }
        }
    }

    pub(super) fn reset_restty_fixture_capture(
        session_uuid: &str,
        peer_id: &str,
        subscription_id: &str,
        rows: u16,
        cols: u16,
        snapshot_len: usize,
    ) {
        let Some(dir) = Self::restty_fixture_dump_dir() else {
            return;
        };

        let stem = Self::restty_fixture_stem(session_uuid);
        for index in 1..=Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT {
            let _ = std::fs::remove_file(dir.join(format!("{stem}-live-{index:04}.bin")));
        }

        let manifest = format!(
            "session_uuid={session_uuid}\npeer_id={peer_id}\nsubscription_id={subscription_id}\nrows={rows}\ncols={cols}\nsnapshot_len={snapshot_len}\nsnapshot_file={stem}-snapshot.bin\nlive_chunk_files={stem}-live-0001.bin..{stem}-live-{limit:04}.bin\nlive_chunk_format=raw post-snapshot PTY bytes after query filtering, before WebRTC prefix/encryption\n",
            limit = Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT,
        );
        let manifest_path = dir.join(format!("{stem}-manifest.txt"));
        Self::write_restty_fixture_file(&manifest_path, manifest.as_bytes());
        log::info!(
            "[ResttyFixture] Reset capture for session {} in {}",
            session_uuid,
            dir.display()
        );
    }

    pub(super) fn dump_restty_snapshot_fixture(session_uuid: &str, snapshot: &[u8]) {
        let Some(dir) = Self::restty_fixture_dump_dir() else {
            return;
        };

        let stem = Self::restty_fixture_stem(session_uuid);
        let path = dir.join(format!("{stem}-snapshot.bin"));
        Self::write_restty_fixture_file(&path, snapshot);
        log::info!(
            "[ResttyFixture] Wrote snapshot fixture {} ({} bytes, hex={})",
            path.display(),
            snapshot.len(),
            Self::restty_fixture_preview_hex(snapshot)
        );
    }

    pub(super) fn dump_restty_live_fixture_chunk(
        session_uuid: &str,
        chunk_index: usize,
        data: &[u8],
    ) {
        let Some(dir) = Self::restty_fixture_dump_dir() else {
            return;
        };
        if chunk_index >= Self::RESTTY_FIXTURE_LIVE_CHUNK_LIMIT {
            return;
        }

        let stem = Self::restty_fixture_stem(session_uuid);
        let path = dir.join(format!("{stem}-live-{:04}.bin", chunk_index + 1));
        Self::write_restty_fixture_file(&path, data);
        log::info!(
            "[ResttyFixture] Wrote live chunk {} for session {} ({} bytes, hex={})",
            chunk_index + 1,
            session_uuid,
            data.len(),
            Self::restty_fixture_preview_hex(data)
        );
    }
}
