//! QR code generation for browser connection.
//!
//! This module provides QR code rendering as ASCII art using Unicode
//! half-block characters (▀▄█). Works in any terminal.
//!
//! Uses `qrcodegen` crate which properly supports alphanumeric mode encoding,
//! allowing ~4296 chars capacity vs ~2953 in byte mode.

// Rust guideline compliant 2026-02

use qrcodegen::{QrCode, QrCodeEcc, QrSegment};

/// Connection code data containing URL and QR code ASCII art.
///
/// This struct is returned by the Hub when generating a connection code.
/// Both TUI and Browser clients use this to display the shareable link
/// with its QR code representation.
#[derive(Debug, Clone)]
pub struct ConnectionCodeData {
    /// The shareable URL containing the hub ID and encoded PreKeyBundle.
    pub url: String,
    /// ASCII art lines of the QR code using Unicode half-blocks.
    pub qr_ascii: Vec<String>,
    /// QR code width in terminal columns.
    pub qr_width: u16,
    /// QR code height in terminal rows.
    pub qr_height: u16,
}

/// Build mixed-mode QR segments: byte for URL, alphanumeric for Base32 bundle.
///
/// Simple split on `#`:
/// - Everything up to and including `#` → byte mode (small, any chars allowed)
/// - Bundle after `#` → alphanumeric mode (large Base32 data, ~4296 char capacity)
///
/// This is efficient because the bulk of the data (the ~2900 char Base32 bundle)
/// gets the higher-capacity alphanumeric encoding, while the short URL portion
/// (~30 chars) uses flexible byte mode.
fn build_mixed_mode_segments(
    data: &str,
    is_alphanumeric_char: &impl Fn(char) -> bool,
) -> Option<Vec<QrSegment>> {
    let hash_pos = data.find('#')?;
    let (url_with_hash, bundle) = data.split_at(hash_pos + 1); // Include # in first part

    // Bundle must be alphanumeric for this optimization to work
    if !bundle.chars().all(is_alphanumeric_char) {
        return None;
    }

    let seg1 = QrSegment::make_bytes(url_with_hash.as_bytes());
    let seg2 = QrSegment::make_alphanumeric(bundle);
    Some(vec![seg1, seg2])
}

/// Generate QR code as terminal-renderable lines that fits within given dimensions.
///
/// Uses Unicode half-block characters to render 2 QR rows per terminal row,
/// which produces correct aspect ratio since terminal chars are ~2:1 (height:width).
///
/// For uppercase alphanumeric URLs (like our Base32-encoded bundles), this uses
/// QR alphanumeric mode which has ~4296 char capacity vs ~2953 in byte mode.
///
/// # Arguments
///
/// * `data` - The data to encode in the QR code
/// * `max_width` - Maximum width in terminal columns
/// * `max_height` - Maximum height in terminal rows
///
/// # Returns
///
/// A vector of strings, each representing one terminal row of the QR code.
/// If the QR code cannot fit in the given dimensions, returns an error message
/// instead.
///
/// # Example
///
/// ```ignore
/// let lines = generate_qr_code_lines("HTTPS://EXAMPLE.COM", 60, 30);
/// for line in lines {
///     println!("{}", line);
/// }
/// ```
pub fn generate_qr_code_lines(data: &str, max_width: u16, max_height: u16) -> Vec<String> {
    // QR alphanumeric charset: 0-9, A-Z, space, $ % * + - . / :
    // Note: # is NOT alphanumeric, so URLs with fragments need mixed-mode encoding
    let is_alphanumeric_char =
        |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || " $%*+-./:".contains(c);

    // Try different error correction levels, from highest to lowest quality
    let ec_levels = [QrCodeEcc::Quartile, QrCodeEcc::Medium, QrCodeEcc::Low];

    for ec_level in ec_levels {
        // Build segments with mixed-mode encoding:
        // - URL up to and including # : byte mode (flexible, small)
        // - Base32 bundle after # : alphanumeric mode (efficient, high capacity)
        let code_result = build_mixed_mode_segments(data, &is_alphanumeric_char)
            .and_then(|segments| {
                QrCode::encode_segments_advanced(
                    &segments,
                    ec_level,
                    qrcodegen::Version::MIN,
                    qrcodegen::Version::MAX,
                    None,
                    true,
                )
                .ok()
            })
            .map(Ok)
            .unwrap_or_else(|| QrCode::encode_text(data, ec_level));

        if let Ok(code) = code_result {
            let size = code.size() as usize;
            // Standard 2-module quiet zone
            let quiet_zone = 2usize;
            let total_size = size + quiet_zone * 2;

            // Each QR module = 1 terminal char wide
            // Each 2 QR rows = 1 terminal row using half-block chars
            // This gives ~square aspect ratio since terminal chars are ~2:1 height:width
            let qr_width = total_size as u16;
            // Ceiling division
            let qr_height = ((total_size + 1) / 2) as u16;

            if qr_width <= max_width && qr_height <= max_height {
                let mut lines = Vec::with_capacity(qr_height as usize);

                // Helper to get module color at position (with quiet zone padding)
                let get_module = |x: usize, y: usize| -> bool {
                    if x < quiet_zone || y < quiet_zone {
                        return false; // White (quiet zone)
                    }
                    let qx = (x - quiet_zone) as i32;
                    let qy = (y - quiet_zone) as i32;
                    if qx >= size as i32 || qy >= size as i32 {
                        return false; // White (quiet zone)
                    }
                    code.get_module(qx, qy)
                };

                // Render 2 rows at a time using half-block characters
                // ▀ = top half (upper row dark, lower row light)
                // ▄ = bottom half (upper row light, lower row dark)
                // █ = full block (both dark)
                // ' ' = space (both light)
                for row_pair in 0..((total_size + 1) / 2) {
                    let upper_y = row_pair * 2;
                    let lower_y = row_pair * 2 + 1;
                    let mut line = String::with_capacity(total_size);

                    for x in 0..total_size {
                        let upper = get_module(x, upper_y);
                        let lower = if lower_y < total_size {
                            get_module(x, lower_y)
                        } else {
                            false // Padding row is white
                        };

                        // Use 1 char per module - half-blocks handle the vertical compression
                        let ch = match (upper, lower) {
                            (true, true) => '█',
                            (true, false) => '▀',
                            (false, true) => '▄',
                            (false, false) => ' ',
                        };
                        line.push(ch);
                    }
                    lines.push(line);
                }

                log::debug!(
                    "QR code fits with ec={:?} -> {}x{} (max: {}x{})",
                    ec_level,
                    qr_width,
                    qr_height,
                    max_width,
                    max_height
                );
                return lines;
            }
        }
    }

    // Calculate actual dimensions needed for this data (using same mixed-mode logic)
    let (needed_width, needed_height, too_long) = {
        let code_result = build_mixed_mode_segments(data, &is_alphanumeric_char)
            .and_then(|segments| {
                QrCode::encode_segments_advanced(
                    &segments,
                    QrCodeEcc::Low,
                    qrcodegen::Version::MIN,
                    qrcodegen::Version::MAX,
                    None,
                    true,
                )
                .ok()
            })
            .map(Ok)
            .unwrap_or_else(|| QrCode::encode_text(data, QrCodeEcc::Low));

        match code_result {
            Ok(code) => {
                let size = code.size() as usize;
                let quiet_zone = 2;
                let total_size = size + quiet_zone * 2;
                (total_size as u16, ((total_size + 1) / 2) as u16, false)
            }
            Err(_) => {
                // Data is too long for any QR code
                (0, 0, true)
            }
        }
    };

    if too_long {
        log::warn!("URL too long for QR code ({} chars)", data.len());
        vec![
            "URL too long for QR code".to_string(),
            format!("URL is {} chars", data.len()),
            "".to_string(),
            "Press [c] to copy URL instead".to_string(),
        ]
    } else {
        log::warn!(
            "QR code too large for terminal (available: {}x{}, need: {}x{})",
            max_width,
            max_height,
            needed_width,
            needed_height
        );
        vec![
            "Terminal too small for QR code".to_string(),
            format!("Available: {}x{}", max_width, max_height),
            format!(
                "Need: {}x{} (try larger terminal)",
                needed_width, needed_height
            ),
            "".to_string(),
            "Press [c] to copy URL instead".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_qr_code_small_data() {
        let lines = generate_qr_code_lines("TEST", 100, 50);
        assert!(!lines.is_empty());
        // Should not be the error message
        assert!(!lines[0].contains("too"));
    }

    #[test]
    fn test_generate_qr_code_insufficient_space() {
        // Very small dimensions should return error message
        let lines = generate_qr_code_lines("HTTPS://EXAMPLE.COM/VERY/LONG/URL", 10, 5);
        assert!(lines[0].contains("too") || lines[0].contains("Terminal"));
    }

    #[test]
    fn test_generate_qr_code_uses_half_blocks() {
        let lines = generate_qr_code_lines("A", 100, 50);
        // Should contain at least some QR characters
        let all_text: String = lines.join("");
        let has_qr_chars = all_text.contains('█')
            || all_text.contains('▀')
            || all_text.contains('▄')
            || all_text.contains(' ');
        assert!(has_qr_chars);
    }

    #[test]
    fn test_qr_creation_for_various_sizes() {
        // Test what sizes can be created with alphanumeric mode
        for len in [100, 500, 1000, 1500, 2000, 2500, 3000, 3500, 4000] {
            // Use uppercase alphanumeric data
            let data: String = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
                .chars()
                .cycle()
                .take(len)
                .collect();
            let segments = QrSegment::make_alphanumeric(&data);
            let result = QrCode::encode_segments_advanced(
                &[segments],
                QrCodeEcc::Low,
                qrcodegen::Version::MIN,
                qrcodegen::Version::MAX,
                None,
                true,
            );
            match result {
                Ok(code) => {
                    let size = code.size();
                    println!("{}chars (alphanumeric) -> QR size {}", len, size);
                }
                Err(e) => {
                    println!("{}chars (alphanumeric) -> FAILED: {:?}", len, e);
                }
            }
        }
    }

    #[test]
    fn test_qr_code_large_url_needs_large_terminal() {
        // Simulate a typical PreKeyBundle URL (base64 JSON with crypto keys)
        // Real bundles are ~1500-2000 chars due to Kyber keys
        let fake_bundle = "A".repeat(1600);
        let url = format!("HTTPS://EXAMPLE.COM/HUBS/ABC123#{}", fake_bundle);

        // At 140x70, should work now with alphanumeric mode
        let lines_medium = generate_qr_code_lines(&url, 140, 70);
        println!(
            "1600 char URL at 140x70: {:?}",
            &lines_medium[0..2.min(lines_medium.len())]
        );

        // At 180x90, QR definitely fits
        let lines_large = generate_qr_code_lines(&url, 180, 90);
        assert!(
            !lines_large.iter().any(|l| l.contains("Terminal")),
            "1600 char alphanumeric URL should fit in 180x90"
        );
    }

    #[test]
    fn test_qr_code_with_base32_uppercase_url() {
        // Simulate the new uppercase Base32 URL format
        // ~2900 chars of Base32 (uppercase + digits only)
        let base32_bundle: String = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
            .chars()
            .cycle()
            .take(2901)
            .collect();
        let url = format!("HTTPS://BOTSTER.DEV/H/123#{}", base32_bundle);

        println!("Test URL length: {} chars", url.len());

        // Should fit in a reasonable terminal (180x90)
        let lines = generate_qr_code_lines(&url, 180, 90);

        // Should NOT show error message
        let has_error = lines
            .iter()
            .any(|l| l.contains("too long") || l.contains("too small") || l.contains("Terminal"));

        if has_error {
            println!("Error lines: {:?}", lines);
        }

        assert!(
            !has_error,
            "Uppercase Base32 URL (~2900 chars) should generate QR code successfully"
        );

        // Should contain QR code characters
        let all_text: String = lines.join("");
        let has_qr_chars =
            all_text.contains('█') || all_text.contains('▀') || all_text.contains('▄');
        assert!(has_qr_chars, "Should contain QR code block characters");
    }
}
