package vt100

import (
	"strings"
	"testing"
)

func TestNew(t *testing.T) {
	p := New(24, 80)

	rows, cols := p.Size()
	if rows != 24 {
		t.Errorf("rows = %d, want 24", rows)
	}
	if cols != 80 {
		t.Errorf("cols = %d, want 80", cols)
	}
}

func TestNewWithScrollback(t *testing.T) {
	p := NewWithScrollback(24, 80, 100)

	// Add more than 100 lines to scrollback
	for i := 0; i < 150; i++ {
		p.AddToScrollback("test line")
	}

	if p.ScrollbackCount() != 100 {
		t.Errorf("scrollback count = %d, want 100", p.ScrollbackCount())
	}
}

func TestProcess(t *testing.T) {
	p := New(24, 80)

	p.Process([]byte("Hello, World!"))

	screen := p.GetScreen()
	if !strings.Contains(screen[0], "Hello, World!") {
		t.Errorf("screen[0] = %q, want to contain 'Hello, World!'", screen[0])
	}
}

func TestProcessMultipleLines(t *testing.T) {
	p := New(24, 80)

	p.Process([]byte("Line 1\r\nLine 2\r\nLine 3"))

	screen := p.GetScreen()
	if !strings.Contains(screen[0], "Line 1") {
		t.Errorf("screen[0] = %q, want to contain 'Line 1'", screen[0])
	}
	if !strings.Contains(screen[1], "Line 2") {
		t.Errorf("screen[1] = %q, want to contain 'Line 2'", screen[1])
	}
	if !strings.Contains(screen[2], "Line 3") {
		t.Errorf("screen[2] = %q, want to contain 'Line 3'", screen[2])
	}
}

func TestSetSize(t *testing.T) {
	p := New(24, 80)
	p.SetSize(40, 120)

	rows, cols := p.Size()
	if rows != 40 {
		t.Errorf("rows = %d, want 40", rows)
	}
	if cols != 120 {
		t.Errorf("cols = %d, want 120", cols)
	}
}

func TestCursorPosition(t *testing.T) {
	p := New(24, 80)

	// Initial position should be 0,0
	row, col := p.CursorPosition()
	if row != 0 {
		t.Errorf("initial row = %d, want 0", row)
	}
	if col != 0 {
		t.Errorf("initial col = %d, want 0", col)
	}

	// Process some text
	p.Process([]byte("Hello"))
	row, col = p.CursorPosition()
	if col != 5 {
		t.Errorf("col after 'Hello' = %d, want 5", col)
	}
}

func TestCursorMovement(t *testing.T) {
	p := New(24, 80)

	// Move cursor to row 5, col 10 using ANSI sequence
	p.Process([]byte("\x1b[5;10H"))

	row, col := p.CursorPosition()
	if row != 4 { // 0-indexed
		t.Errorf("row = %d, want 4", row)
	}
	if col != 9 { // 0-indexed
		t.Errorf("col = %d, want 9", col)
	}
}

func TestGetContents(t *testing.T) {
	p := New(24, 80)
	p.Process([]byte("Line 1\r\nLine 2"))

	contents := p.GetContents()
	if !strings.Contains(contents, "Line 1") {
		t.Errorf("contents should contain 'Line 1'")
	}
	if !strings.Contains(contents, "Line 2") {
		t.Errorf("contents should contain 'Line 2'")
	}
}

func TestGetScreenAsANSI(t *testing.T) {
	p := New(24, 80)
	p.Process([]byte("Hello"))

	ansi := p.GetScreenAsANSI()

	// Should contain cursor hide/show sequences
	if !strings.Contains(ansi, "\x1b[?25l") {
		t.Error("ANSI output should contain cursor hide sequence")
	}
	if !strings.Contains(ansi, "\x1b[?25h") {
		t.Error("ANSI output should contain cursor show sequence")
	}

	// Should contain the text
	if !strings.Contains(ansi, "H") {
		t.Error("ANSI output should contain 'H'")
	}
}

func TestGetScreenHash(t *testing.T) {
	p := New(24, 80)
	hash1 := p.GetScreenHash()

	p.Process([]byte("Some content"))
	hash2 := p.GetScreenHash()

	if hash1 == hash2 {
		t.Error("Hash should change after processing content")
	}
}

func TestGetScreenHashStable(t *testing.T) {
	p1 := New(24, 80)
	p2 := New(24, 80)

	p1.Process([]byte("Same content"))
	p2.Process([]byte("Same content"))

	hash1 := p1.GetScreenHash()
	hash2 := p2.GetScreenHash()

	if hash1 != hash2 {
		t.Error("Hash should be same for identical content")
	}
}

func TestClear(t *testing.T) {
	p := New(24, 80)
	p.Process([]byte("Some content to clear"))

	p.Clear()

	screen := p.GetScreen()
	// After clear, first line should be mostly empty
	trimmed := strings.TrimSpace(screen[0])
	if len(trimmed) > 0 && trimmed != "" {
		// Some terminals might have artifacts, but content should be gone
		if strings.Contains(trimmed, "content") {
			t.Errorf("screen[0] = %q, should be empty after clear", trimmed)
		}
	}
}

func TestScrollback(t *testing.T) {
	p := New(24, 80)

	p.AddToScrollback("line 1")
	p.AddToScrollback("line 2")
	p.AddToScrollback("line 3")

	if p.ScrollbackCount() != 3 {
		t.Errorf("scrollback count = %d, want 3", p.ScrollbackCount())
	}

	sb := p.GetScrollback()
	if len(sb) != 3 {
		t.Errorf("scrollback len = %d, want 3", len(sb))
	}
	if sb[0] != "line 1" {
		t.Errorf("scrollback[0] = %q, want 'line 1'", sb[0])
	}
}

func TestClearScrollback(t *testing.T) {
	p := New(24, 80)

	p.AddToScrollback("line 1")
	p.AddToScrollback("line 2")
	p.ClearScrollback()

	if p.ScrollbackCount() != 0 {
		t.Errorf("scrollback count = %d, want 0", p.ScrollbackCount())
	}
}

func TestScrollbackLimit(t *testing.T) {
	p := NewWithScrollback(24, 80, 10)

	for i := 0; i < 20; i++ {
		p.AddToScrollback("line")
	}

	if p.ScrollbackCount() != 10 {
		t.Errorf("scrollback count = %d, want 10", p.ScrollbackCount())
	}
}

func TestANSIColors(t *testing.T) {
	p := New(24, 80)

	// Process text with color codes
	p.Process([]byte("\x1b[31mRed text\x1b[0m"))

	screen := p.GetScreen()
	if !strings.Contains(screen[0], "Red text") {
		t.Errorf("screen should contain 'Red text'")
	}
}
