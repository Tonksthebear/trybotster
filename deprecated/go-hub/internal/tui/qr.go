// Package tui provides the terminal user interface for botster-hub.
package tui

import (
	"github.com/skip2/go-qrcode"
)

// GenerateQRLines generates a QR code as terminal-renderable lines using half-block characters.
// Uses █ (full), ▀ (top half), ▄ (bottom half), and space for different combinations.
func GenerateQRLines(data string, maxWidth, maxHeight int) []string {
	// Try different error correction levels (highest to lowest)
	levels := []qrcode.RecoveryLevel{
		qrcode.Highest,
		qrcode.High,
		qrcode.Medium,
		qrcode.Low,
	}

	for _, level := range levels {
		qr, err := qrcode.New(data, level)
		if err != nil {
			continue
		}

		// Disable the border (we'll add our own quiet zone)
		qr.DisableBorder = true
		bitmap := qr.Bitmap()

		size := len(bitmap)
		quietZone := 2
		totalSize := size + quietZone*2

		// Calculate display dimensions
		// Each QR module = 1 char wide
		// Each 2 QR rows = 1 terminal row (using half-block chars)
		qrWidth := totalSize
		qrHeight := (totalSize + 1) / 2

		if qrWidth <= maxWidth && qrHeight <= maxHeight {
			// Generate the lines
			var lines []string
			for rowPair := 0; rowPair < qrHeight; rowPair++ {
				upperY := rowPair*2 - quietZone
				lowerY := rowPair*2 + 1 - quietZone

				var line string
				for x := -quietZone; x < size+quietZone; x++ {
					upper := getPixel(bitmap, x, upperY)
					lower := getPixel(bitmap, x, lowerY)

					// Map to half-block characters
					// Dark = true, Light = false
					switch {
					case upper && lower:
						line += "█" // Both dark
					case upper && !lower:
						line += "▀" // Top half dark
					case !upper && lower:
						line += "▄" // Bottom half dark
					default:
						line += " " // Both light
					}
				}
				lines = append(lines, line)
			}
			return lines
		}
	}

	// If QR code doesn't fit, return error message
	return []string{
		"QR code too large for terminal",
		"Please resize your terminal window",
		"(need at least 60x30 characters)",
	}
}

// getPixel returns whether a pixel is dark (true) or light (false).
// Returns false (light) for pixels outside the bitmap (quiet zone).
func getPixel(bitmap [][]bool, x, y int) bool {
	if y < 0 || y >= len(bitmap) || x < 0 || x >= len(bitmap[0]) {
		return false // Quiet zone is light
	}
	return bitmap[y][x]
}
