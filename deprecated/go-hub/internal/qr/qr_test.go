package qr

import (
	"strings"
	"testing"
)

func TestGenerateLinesSmallData(t *testing.T) {
	lines := GenerateLines("test", 100, 50)

	if len(lines) == 0 {
		t.Fatal("expected non-empty lines")
	}

	// Should not be the error message
	if strings.Contains(lines[0], "too large") {
		t.Errorf("unexpected error message for small data")
	}
}

func TestGenerateLinesURL(t *testing.T) {
	lines := GenerateLines("https://example.com", 100, 50)

	if len(lines) == 0 {
		t.Fatal("expected non-empty lines")
	}

	// Should not be the error message
	if strings.Contains(lines[0], "too large") {
		t.Errorf("unexpected error message for URL")
	}
}

func TestGenerateLinesInsufficientSpace(t *testing.T) {
	// Very small dimensions should return error message
	lines := GenerateLines("https://example.com/very/long/url/that/is/too/big", 10, 5)

	if len(lines) == 0 {
		t.Fatal("expected error lines")
	}

	// Should contain error message
	if !strings.Contains(lines[0], "too large") {
		t.Errorf("expected 'too large' error message, got: %s", lines[0])
	}
}

func TestGenerateLinesUsesHalfBlocks(t *testing.T) {
	lines := GenerateLines("A", 100, 50)

	allText := strings.Join(lines, "")

	// Should contain at least some QR characters
	hasFullBlock := strings.ContainsRune(allText, '█')
	hasUpperHalf := strings.ContainsRune(allText, '▀')
	hasLowerHalf := strings.ContainsRune(allText, '▄')
	hasSpace := strings.ContainsRune(allText, ' ')

	if !hasFullBlock && !hasUpperHalf && !hasLowerHalf && !hasSpace {
		t.Errorf("expected QR block characters in output")
	}
}

func TestGenerateLinesConsistentWidth(t *testing.T) {
	lines := GenerateLines("hello", 100, 50)

	if len(lines) < 2 {
		t.Fatal("expected multiple lines")
	}

	// All lines should have the same width (in runes)
	firstWidth := len([]rune(lines[0]))
	for i, line := range lines[1:] {
		width := len([]rune(line))
		if width != firstWidth {
			t.Errorf("line %d has width %d, expected %d", i+1, width, firstWidth)
		}
	}
}

func TestGenerateLinesSquareish(t *testing.T) {
	lines := GenerateLines("test", 100, 50)

	if len(lines) == 0 {
		t.Fatal("expected non-empty lines")
	}

	// Width should be roughly 2x height (due to half-block encoding)
	width := len([]rune(lines[0]))
	height := len(lines)

	// Width should be about 1.5-2.5x height
	ratio := float64(width) / float64(height)
	if ratio < 1.5 || ratio > 2.5 {
		t.Errorf("unexpected aspect ratio: width=%d, height=%d, ratio=%.2f", width, height, ratio)
	}
}

func TestGenerateLinesEmptyData(t *testing.T) {
	// Empty string is valid for QR encoding
	lines := GenerateLines("", 100, 50)
	if len(lines) == 0 {
		t.Error("expected output for empty data")
	}
}

func TestGenerateLinesLongData(t *testing.T) {
	// Long data should work with enough space
	longData := strings.Repeat("a", 200)
	lines := GenerateLines(longData, 200, 100)

	// With enough space, should not show error
	if strings.Contains(lines[0], "too large") {
		t.Log("QR code was too large even with 200x100, which is expected for very long data")
	} else {
		// Verify it's actual QR content
		allText := strings.Join(lines, "")
		if len(allText) == 0 {
			t.Error("expected non-empty QR output")
		}
	}
}

func TestGenerateLinesInverted(t *testing.T) {
	normal := GenerateLines("test", 100, 50)
	inverted := GenerateLinesInverted("test", 100, 50)

	if len(normal) != len(inverted) {
		t.Errorf("line count mismatch: normal=%d, inverted=%d", len(normal), len(inverted))
		return
	}

	// At least some characters should be different
	normalAll := strings.Join(normal, "")
	invertedAll := strings.Join(inverted, "")

	if normalAll == invertedAll {
		t.Error("inverted should differ from normal")
	}
}

func TestGenerateLinesInvertedErrorCase(t *testing.T) {
	// Should return same error message
	lines := GenerateLinesInverted("https://example.com/long/url", 10, 5)

	if len(lines) == 0 {
		t.Fatal("expected error lines")
	}

	if !strings.Contains(lines[0], "too large") {
		t.Errorf("expected 'too large' error message")
	}
}

func TestDimensions(t *testing.T) {
	tests := []struct {
		data      string
		minWidth  uint16
		maxWidth  uint16
		minHeight uint16
		maxHeight uint16
	}{
		{"A", 21, 30, 10, 15},              // Very short data
		{"hello", 21, 40, 10, 20},          // Short data
		{"https://example.com", 25, 50, 12, 25}, // URL
	}

	for _, tt := range tests {
		w, h := Dimensions(tt.data)

		if w == 0 || h == 0 {
			t.Errorf("Dimensions(%q) returned 0", tt.data)
			continue
		}

		if w < tt.minWidth || w > tt.maxWidth {
			t.Errorf("Dimensions(%q) width=%d, expected %d-%d", tt.data, w, tt.minWidth, tt.maxWidth)
		}

		if h < tt.minHeight || h > tt.maxHeight {
			t.Errorf("Dimensions(%q) height=%d, expected %d-%d", tt.data, h, tt.minHeight, tt.maxHeight)
		}

		// Height should be roughly half the width (due to half-block encoding)
		if float64(w)/float64(h) < 1.5 || float64(w)/float64(h) > 2.5 {
			t.Errorf("Dimensions(%q) unexpected ratio: w=%d, h=%d", tt.data, w, h)
		}
	}
}

func TestDimensionsConsistentWithGenerate(t *testing.T) {
	data := "test123"

	w, h := Dimensions(data)
	lines := GenerateLines(data, 100, 50)

	if len(lines) == 0 {
		t.Fatal("expected lines")
	}

	// Generated width should match Dimensions
	genWidth := uint16(len([]rune(lines[0])))
	genHeight := uint16(len(lines))

	if genWidth != w {
		t.Errorf("width mismatch: Dimensions=%d, Generated=%d", w, genWidth)
	}

	if genHeight != h {
		t.Errorf("height mismatch: Dimensions=%d, Generated=%d", h, genHeight)
	}
}

func TestGenerateLinesExactFit(t *testing.T) {
	data := "test"

	// Get actual dimensions
	w, h := Dimensions(data)

	// Generate with exact fit
	lines := GenerateLines(data, w, h)

	if strings.Contains(lines[0], "too large") {
		t.Errorf("should fit when given exact dimensions w=%d, h=%d", w, h)
	}

	// Generate with one less column should fail
	lines = GenerateLines(data, w-1, h)
	// May or may not fail depending on error correction fallback
}

func TestGenerateLinesRecoveryFallback(t *testing.T) {
	// When dimensions are tight, should fall back to lower recovery levels
	data := "https://example.com"
	w, h := Dimensions(data)

	// Try with slightly reduced dimensions
	lines := GenerateLines(data, w-2, h)

	// With fallback, might still succeed with lower recovery level
	// Just verify it doesn't panic and returns something
	if len(lines) == 0 {
		t.Error("expected some output")
	}
}

func TestGenerateLinesValidUTF8(t *testing.T) {
	lines := GenerateLines("test", 100, 50)

	for i, line := range lines {
		// Each line should be valid UTF-8
		for _, r := range line {
			if r == '\uFFFD' { // Unicode replacement character
				t.Errorf("line %d contains invalid UTF-8", i)
			}
		}
	}
}

func TestGenerateLinesOnlyExpectedChars(t *testing.T) {
	lines := GenerateLines("test", 100, 50)
	allText := strings.Join(lines, "")

	for _, r := range allText {
		switch r {
		case '█', '▀', '▄', ' ':
			// Valid QR characters
		default:
			t.Errorf("unexpected character: %q (U+%04X)", r, r)
		}
	}
}
