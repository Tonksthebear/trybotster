//! QR code generation for browser connection.
//!
//! This module provides QR code rendering optimized for terminal display.
//! Supports two rendering modes:
//!
//! 1. **Kitty graphics protocol** - Renders QR as an inline PNG image.
//!    Works in Ghostty, Kitty, and other modern terminals.
//!
//! 2. **Unicode half-block fallback** - Uses ▀▄█ characters for terminals
//!    without graphics support. Requires larger terminal for Kyber keys.
//!
//! Uses `qrcodegen` crate which properly supports alphanumeric mode encoding,
//! allowing ~4296 chars capacity vs ~2953 in byte mode.

// Rust guideline compliant 2026-01

use base64::Engine;
use image::{GrayImage, ImageEncoder, Luma};
use qrcodegen::{QrCode, QrCodeEcc, QrSegment};

/// Connection code data containing URL and QR code PNG.
///
/// This struct is returned by the Hub when generating a connection code.
/// Both TUI and Browser clients use this to display the shareable link
/// with its QR code representation.
#[derive(Debug, Clone)]
pub struct ConnectionCodeData {
    /// The shareable URL containing the hub ID and encoded PreKeyBundle.
    pub url: String,
    /// Raw PNG bytes of the QR code image.
    pub qr_png: Vec<u8>,
}

/// Result of QR code generation for terminal display.
#[derive(Debug)]
pub enum QrRenderResult {
    /// QR rendered as inline image via Kitty graphics protocol.
    /// Contains the escape sequence to display the image.
    KittyImage {
        /// Escape sequence containing the image data.
        escape_sequence: String,
        /// Width of image in terminal cells (columns).
        width_cells: u16,
        /// Height of image in terminal cells (rows).
        height_cells: u16,
    },
    /// QR rendered as text lines using Unicode half-blocks.
    TextLines(Vec<String>),
    /// QR code could not be generated (data too long or terminal too small).
    Error {
        /// Error message lines to display.
        lines: Vec<String>,
        /// Whether the terminal is too small (vs data too long).
        terminal_too_small: bool,
    },
}

/// Generate QR code as raw PNG bytes.
///
/// Creates a QR code image suitable for display in any context (terminal,
/// browser, etc.). Uses optimal mixed-mode encoding for URLs with Base32
/// fragments.
///
/// # Arguments
///
/// * `data` - The data to encode in the QR code
/// * `module_size` - Pixels per QR module (4 recommended for good quality/size balance)
///
/// # Returns
///
/// Raw PNG bytes on success, or an error message if generation fails.
///
/// # Errors
///
/// Returns an error if the data is too long to encode in a QR code.
pub fn generate_qr_png(data: &str, module_size: u8) -> Result<Vec<u8>, String> {
    let code = generate_qr_code(data)
        .ok_or_else(|| format!("Data too long for QR code ({} chars)", data.len()))?;

    let size = code.size() as u32;
    let quiet_zone = 2u32;
    let total_modules = size + quiet_zone * 2;
    let img_size = total_modules * module_size as u32;

    // Create grayscale image (white background)
    let mut img = GrayImage::from_pixel(img_size, img_size, Luma([255u8]));

    // Draw QR modules
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x as i32, y as i32) {
                // Draw black module
                let px = (x + quiet_zone) * module_size as u32;
                let py = (y + quiet_zone) * module_size as u32;
                for dy in 0..module_size as u32 {
                    for dx in 0..module_size as u32 {
                        img.put_pixel(px + dx, py + dy, Luma([0u8]));
                    }
                }
            }
        }
    }

    // Encode as PNG
    let mut png_bytes = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        encoder
            .write_image(
                img.as_raw(),
                img_size,
                img_size,
                image::ExtendedColorType::L8,
            )
            .map_err(|e| format!("PNG encoding failed: {}", e))?;
    }

    Ok(png_bytes)
}

/// Build Kitty graphics protocol escape sequence from existing PNG bytes.
///
/// Takes pre-generated PNG bytes (e.g., from `generate_qr_png`) and creates the
/// terminal escape sequence for displaying it via Kitty graphics protocol.
///
/// # Arguments
///
/// * `png_bytes` - Raw PNG image bytes
/// * `max_cols` - Maximum terminal columns available for display
/// * `max_rows` - Maximum terminal rows available for display
///
/// # Returns
///
/// Tuple of (escape_sequence, width_cells, height_cells) on success, or None if
/// the PNG cannot be parsed or is too large.
pub fn build_kitty_escape_from_png(
    png_bytes: &[u8],
    max_cols: u16,
    max_rows: u16,
) -> Option<(String, u16, u16)> {
    // Decode PNG to get dimensions
    let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(png_bytes)).ok()?;
    use image::ImageDecoder;
    let (img_width, img_height) = decoder.dimensions();

    // Scale QR to fill available space while maintaining square aspect ratio.
    // Terminal cells are ~2:1 (height:width), so for a square image:
    // - display_rows determines the visual height
    // - display_cols should be ~2x display_rows to appear square
    //
    // Use all available space - Kitty will scale the high-res PNG up.
    let display_rows = max_rows;
    let display_cols = max_cols.min(display_rows * 2);

    if display_cols == 0 || display_rows == 0 {
        return None;
    }

    // Build Kitty escape sequence
    let b64_data = Engine::encode(&base64::engine::general_purpose::STANDARD, png_bytes);
    let escape_sequence = build_kitty_escape_sequence(&b64_data, img_width, display_cols, display_rows);

    Some((escape_sequence, display_cols, display_rows))
}

/// Generate QR code as PNG image and encode for Kitty graphics protocol.
///
/// Returns the escape sequence to display the image inline, or None if generation fails.
/// The caller specifies max display dimensions; the image scales to fit while maintaining
/// aspect ratio. High-res PNG ensures scannability even when scaled down.
///
/// # Arguments
///
/// * `data` - The data to encode in the QR code
/// * `module_size` - Pixels per QR module in the generated PNG (4 recommended for quality)
/// * `max_cols` - Maximum terminal columns available for display
/// * `max_rows` - Maximum terminal rows available for display
pub fn generate_qr_kitty_image(
    data: &str,
    module_size: u8,
    max_cols: u16,
    max_rows: u16,
) -> Option<QrRenderResult> {
    let code = generate_qr_code(data)?;
    let size = code.size() as u32;
    let quiet_zone = 2u32;
    let total_modules = size + quiet_zone * 2;
    let img_size = total_modules * module_size as u32;

    // Scale QR to fill available space while maintaining square aspect ratio.
    // Terminal cells are ~2:1 (height:width), so for a square image:
    // - display_rows determines the visual height
    // - display_cols should be ~2x display_rows to appear square
    //
    // Use all available space - Kitty will scale the high-res PNG up.
    let display_rows = max_rows;
    let display_cols = max_cols.min(display_rows * 2);

    // Create grayscale image (white background)
    let mut img = GrayImage::from_pixel(img_size, img_size, Luma([255u8]));

    // Draw QR modules
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x as i32, y as i32) {
                // Draw black module
                let px = (x + quiet_zone) * module_size as u32;
                let py = (y + quiet_zone) * module_size as u32;
                for dy in 0..module_size as u32 {
                    for dx in 0..module_size as u32 {
                        img.put_pixel(px + dx, py + dy, Luma([0u8]));
                    }
                }
            }
        }
    }

    // Encode as PNG
    let mut png_bytes = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        encoder
            .write_image(
                img.as_raw(),
                img_size,
                img_size,
                image::ExtendedColorType::L8,
            )
            .ok()?;
    }

    // Build Kitty graphics protocol escape sequence with calculated cell sizing
    let b64_data = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    let escape_sequence =
        build_kitty_escape_sequence(&b64_data, img_size, display_cols, display_rows);

    Some(QrRenderResult::KittyImage {
        escape_sequence,
        width_cells: display_cols,
        height_cells: display_rows,
    })
}

/// Escape sequence to delete all Kitty graphics images.
/// Call this when closing the QR modal to clean up.
pub fn kitty_delete_images() -> &'static str {
    "\x1b_Ga=d\x1b\\"
}

/// Build Kitty graphics protocol escape sequence, handling chunking for large images.
///
/// - `img_size_px`: actual image dimensions in pixels (square)
/// - `display_cols`: number of terminal columns to display image in
/// - `display_rows`: number of terminal rows to display image in
fn build_kitty_escape_sequence(
    b64_data: &str,
    img_size_px: u32,
    display_cols: u16,
    display_rows: u16,
) -> String {
    const CHUNK_SIZE: usize = 4096;
    let mut result = String::new();
    let chunks: Vec<&str> = b64_data
        .as_bytes()
        .chunks(CHUNK_SIZE)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    for (i, chunk) in chunks.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == chunks.len() - 1;
        let more = if is_last { 0 } else { 1 };

        if is_first {
            // First chunk includes image parameters
            // a=T: transmit and display
            // f=100: PNG format
            // s,v: source image size in pixels
            // c,r: display size in terminal cells (columns, rows)
            // m=1: more chunks coming (0 if last)
            result.push_str(&format!(
                "\x1b_Ga=T,f=100,s={},v={},c={},r={},m={};{}\x1b\\",
                img_size_px, img_size_px, display_cols, display_rows, more, chunk
            ));
        } else {
            // Continuation chunks
            result.push_str(&format!("\x1b_Gm={};{}\x1b\\", more, chunk));
        }
    }

    result
}

/// Generate QR code from data using optimal encoding.
fn generate_qr_code(data: &str) -> Option<QrCode> {
    let is_alphanumeric_char =
        |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || " $%*+-./:".contains(c);

    // Try different error correction levels
    for ec_level in [QrCodeEcc::Quartile, QrCodeEcc::Medium, QrCodeEcc::Low] {
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
            return Some(code);
        }
    }
    None
}

/// Calculate required terminal rows for a Kitty image.
///
/// Kitty displays images using character cells. Each cell is typically
/// ~7x14 pixels (varies by font). We estimate conservatively.
pub fn kitty_image_rows(height_px: u32) -> u16 {
    // Assume ~14 pixels per row (common for terminal fonts)
    // Add 1 for rounding
    ((height_px + 13) / 14) as u16
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
