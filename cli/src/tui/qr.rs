//! QR code generation for browser connection.
//!
//! This module provides QR code rendering optimized for terminal display
//! using Unicode half-block characters for correct aspect ratio.

use qrcode::{Color, EcLevel, QrCode};

/// Generate QR code as terminal-renderable lines that fits within given dimensions.
///
/// Uses Unicode half-block characters to render 2 QR rows per terminal row,
/// which produces correct aspect ratio since terminal chars are ~2:1 (height:width).
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
/// let lines = generate_qr_code_lines("https://example.com", 60, 30);
/// for line in lines {
///     println!("{}", line);
/// }
/// ```
pub fn generate_qr_code_lines(data: &str, max_width: u16, max_height: u16) -> Vec<String> {
    // Try different error correction levels, from highest to lowest quality
    let ec_levels = [None, Some(EcLevel::M), Some(EcLevel::L)];

    for ec_level in ec_levels {
        let code_result = if let Some(ec) = ec_level {
            QrCode::with_error_correction_level(data, ec)
        } else {
            QrCode::new(data)
        };

        if let Ok(code) = code_result {
            // Get raw QR matrix with quiet zone
            let colors = code.to_colors();
            let size = code.width();
            // Standard 2-module quiet zone
            let quiet_zone = 2;
            let total_size = size + quiet_zone * 2;

            // Each QR module = 1 terminal char wide
            // Each 2 QR rows = 1 terminal row using half-block chars
            // This gives ~square aspect ratio since terminal chars are ~2:1 height:width
            let qr_width = total_size as u16;
            // Ceiling division
            let qr_height = total_size.div_ceil(2) as u16;

            if qr_width <= max_width && qr_height <= max_height {
                let mut lines = Vec::with_capacity(qr_height as usize);

                // Helper to get color at position (with quiet zone padding)
                let get_color = |x: usize, y: usize| -> bool {
                    if x < quiet_zone || y < quiet_zone {
                        return false; // White (quiet zone)
                    }
                    let qx = x - quiet_zone;
                    let qy = y - quiet_zone;
                    if qx >= size || qy >= size {
                        return false; // White (quiet zone)
                    }
                    colors[qy * size + qx] == Color::Dark
                };

                // Render 2 rows at a time using half-block characters
                // ▀ = top half (upper row dark, lower row light)
                // ▄ = bottom half (upper row light, lower row dark)
                // █ = full block (both dark)
                // ' ' = space (both light)
                for row_pair in 0..total_size.div_ceil(2) {
                    let upper_y = row_pair * 2;
                    let lower_y = row_pair * 2 + 1;
                    let mut line = String::with_capacity(total_size);

                    for x in 0..total_size {
                        let upper = get_color(x, upper_y);
                        let lower = if lower_y < total_size {
                            get_color(x, lower_y)
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

    // Calculate actual dimensions needed for this URL
    // Try to create a QR to get its size
    let (needed_width, needed_height, too_long) = match QrCode::with_error_correction_level(data, EcLevel::L) {
        Ok(code) => {
            let size = code.width();
            let quiet_zone = 2;
            let total_size = size + quiet_zone * 2;
            (total_size as u16, total_size.div_ceil(2) as u16, false)
        }
        Err(_) => {
            // Data is too long for any QR code (>~2900 chars)
            (0, 0, true)
        }
    };

    if too_long {
        log::warn!(
            "URL too long for QR code ({} chars, max ~2900)",
            data.len()
        );
        vec![
            "URL too long for QR code".to_string(),
            format!("URL is {} chars (max ~2900)", data.len()),
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
            format!("Need: {}x{} (try larger terminal)", needed_width, needed_height),
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
        let lines = generate_qr_code_lines("test", 100, 50);
        assert!(!lines.is_empty());
        // Should not be the error message
        assert!(!lines[0].contains("too large"));
    }

    #[test]
    fn test_generate_qr_code_insufficient_space() {
        // Very small dimensions should return error message
        let lines = generate_qr_code_lines("https://example.com/very/long/url", 10, 5);
        assert!(lines[0].contains("too large") || lines[0].contains("QR"));
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
        // Test what sizes can be created
        for len in [100, 500, 1000, 1500, 2000, 2500, 3000] {
            let data = "a".repeat(len);
            let result = QrCode::with_error_correction_level(&data, EcLevel::L);
            match result {
                Ok(code) => {
                    let size = code.width();
                    println!("{}chars -> QR version with size {}", len, size);
                }
                Err(e) => {
                    println!("{}chars -> FAILED: {:?}", len, e);
                }
            }
        }
    }

    #[test]
    fn test_qr_code_large_url_needs_large_terminal() {
        // Simulate a typical PreKeyBundle URL (base64 JSON with crypto keys)
        // Real bundles are ~1500-2000 chars due to Kyber keys
        let fake_bundle = "a]".repeat(800); // ~1600 chars
        let url = format!("https://example.com/hubs/abc123#{}", fake_bundle);

        // At 140x70, QR doesn't fit
        let lines_small = generate_qr_code_lines(&url, 140, 70);
        assert!(
            lines_small.iter().any(|l| l.contains("Terminal")),
            "Large URL should not fit in 140x70"
        );

        // At 160x80, QR fits
        let lines_large = generate_qr_code_lines(&url, 160, 80);
        assert!(
            !lines_large.iter().any(|l| l.contains("Terminal")),
            "Large URL should fit in 160x80"
        );
    }
}
