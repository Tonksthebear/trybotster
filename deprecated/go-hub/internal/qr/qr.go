// Package qr provides QR code generation optimized for terminal display.
//
// Uses Unicode half-block characters for correct aspect ratio since
// terminal characters are approximately 2:1 (height:width).
package qr

import (
	"strings"

	"github.com/skip2/go-qrcode"
)

// GenerateLines generates QR code as terminal-renderable lines.
//
// Uses Unicode half-block characters to render 2 QR rows per terminal row,
// producing correct aspect ratio since terminal chars are ~2:1 (height:width).
//
// Returns a vector of strings, each representing one terminal row.
// If the QR code cannot fit in the given dimensions, returns an error message.
func GenerateLines(data string, maxWidth, maxHeight uint16) []string {
	// Try different recovery levels from highest to lowest quality
	levels := []qrcode.RecoveryLevel{
		qrcode.High,
		qrcode.Medium,
		qrcode.Low,
	}

	for _, level := range levels {
		qr, err := qrcode.New(data, level)
		if err != nil {
			continue
		}

		// Get the bitmap (includes quiet zone)
		bitmap := qr.Bitmap()
		if len(bitmap) == 0 || len(bitmap[0]) == 0 {
			continue
		}

		size := len(bitmap)

		// Each QR module = 1 terminal char wide
		// Each 2 QR rows = 1 terminal row using half-block chars
		qrWidth := uint16(size)
		qrHeight := uint16((size + 1) / 2) // Ceiling division

		if qrWidth <= maxWidth && qrHeight <= maxHeight {
			lines := make([]string, 0, qrHeight)

			// Render 2 rows at a time using half-block characters
			// ▀ = top half (upper row dark, lower row light)
			// ▄ = bottom half (upper row light, lower row dark)
			// █ = full block (both dark)
			// ' ' = space (both light)
			for rowPair := 0; rowPair < (size+1)/2; rowPair++ {
				upperY := rowPair * 2
				lowerY := rowPair*2 + 1

				var sb strings.Builder
				sb.Grow(size * 3) // UTF-8 block chars are 3 bytes

				for x := 0; x < size; x++ {
					upper := bitmap[upperY][x]
					lower := false
					if lowerY < size {
						lower = bitmap[lowerY][x]
					}

					// Note: in go-qrcode, true = dark (black module)
					var ch rune
					switch {
					case upper && lower:
						ch = '█'
					case upper && !lower:
						ch = '▀'
					case !upper && lower:
						ch = '▄'
					default:
						ch = ' '
					}
					sb.WriteRune(ch)
				}
				lines = append(lines, sb.String())
			}

			return lines
		}
	}

	// If nothing fits, return error message
	return []string{
		"QR code too large for terminal",
		"Please resize your terminal window",
		"(need at least 60x30 characters)",
	}
}

// GenerateLinesInverted generates QR code with inverted colors.
// This is useful for light-on-dark terminal themes.
func GenerateLinesInverted(data string, maxWidth, maxHeight uint16) []string {
	levels := []qrcode.RecoveryLevel{
		qrcode.High,
		qrcode.Medium,
		qrcode.Low,
	}

	for _, level := range levels {
		qr, err := qrcode.New(data, level)
		if err != nil {
			continue
		}

		bitmap := qr.Bitmap()
		if len(bitmap) == 0 || len(bitmap[0]) == 0 {
			continue
		}

		size := len(bitmap)
		qrWidth := uint16(size)
		qrHeight := uint16((size + 1) / 2)

		if qrWidth <= maxWidth && qrHeight <= maxHeight {
			lines := make([]string, 0, qrHeight)

			for rowPair := 0; rowPair < (size+1)/2; rowPair++ {
				upperY := rowPair * 2
				lowerY := rowPair*2 + 1

				var sb strings.Builder
				sb.Grow(size * 3)

				for x := 0; x < size; x++ {
					// Invert: true becomes light, false becomes dark
					upper := !bitmap[upperY][x]
					lower := true // Default to light (inverted from dark quiet zone padding)
					if lowerY < size {
						lower = !bitmap[lowerY][x]
					}

					var ch rune
					switch {
					case upper && lower:
						ch = '█'
					case upper && !lower:
						ch = '▀'
					case !upper && lower:
						ch = '▄'
					default:
						ch = ' '
					}
					sb.WriteRune(ch)
				}
				lines = append(lines, sb.String())
			}

			return lines
		}
	}

	return []string{
		"QR code too large for terminal",
		"Please resize your terminal window",
		"(need at least 60x30 characters)",
	}
}

// Dimensions returns the expected dimensions of a QR code for the given data.
// Returns (width, height) in terminal columns/rows, or (0, 0) if encoding fails.
func Dimensions(data string) (uint16, uint16) {
	qr, err := qrcode.New(data, qrcode.Medium)
	if err != nil {
		return 0, 0
	}

	bitmap := qr.Bitmap()
	if len(bitmap) == 0 {
		return 0, 0
	}

	size := len(bitmap)
	return uint16(size), uint16((size + 1) / 2)
}
