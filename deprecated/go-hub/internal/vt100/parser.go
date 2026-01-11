// Package vt100 provides VT100 terminal emulation for screen state tracking.
//
// This wraps github.com/charmbracelet/x/vt which properly handles
// alternate screen buffer (CSI ?1049h/l), carriage return for in-place updates
// (spinners, progress bars), and full VT100/xterm-256color escape sequences.
package vt100

import (
	"hash/fnv"
	"image/color"
	"sync"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/vt"
)

// MaxScrollback is the default scrollback buffer size.
const MaxScrollback = 20000

// Parser wraps the charmbracelet/x/vt terminal emulator.
type Parser struct {
	mu sync.Mutex

	// term is the underlying terminal emulator (thread-safe).
	term vt.Terminal

	// rows and cols are the terminal dimensions.
	rows, cols int

	// scrollback is the scrollback buffer (lines that scrolled off top).
	scrollback []string

	// maxScrollback is the maximum scrollback lines to retain.
	maxScrollback int
}

// CellInfo holds the character and formatting for a single cell.
type CellInfo struct {
	Char rune
	FG   color.Color
	BG   color.Color
	Bold bool
	Dim  bool
}

// New creates a new VT100 parser with the specified dimensions.
func New(rows, cols int) *Parser {
	return NewWithScrollback(rows, cols, MaxScrollback)
}

// NewWithScrollback creates a parser with custom scrollback limit.
func NewWithScrollback(rows, cols, scrollback int) *Parser {
	// Use SafeEmulator for thread-safe access
	term := vt.NewSafeEmulator(cols, rows)

	return &Parser{
		term:          term,
		rows:          rows,
		cols:          cols,
		scrollback:    make([]string, 0),
		maxScrollback: scrollback,
	}
}

// Process feeds bytes to the terminal emulator.
func (p *Parser) Process(data []byte) {
	// SafeEmulator handles its own locking
	p.term.Write(data)
}

// Size returns the current terminal dimensions.
func (p *Parser) Size() (rows, cols int) {
	return p.term.Height(), p.term.Width()
}

// SetSize resizes the terminal.
func (p *Parser) SetSize(rows, cols int) {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.rows = rows
	p.cols = cols
	p.term.Resize(cols, rows)
}

// CursorPosition returns the current cursor position (row, col).
func (p *Parser) CursorPosition() (row, col int) {
	pos := p.term.CursorPosition()
	return pos.Y, pos.X
}

// GetScreen returns the visible screen as lines (plain text, no ANSI).
func (p *Parser) GetScreen() []string {
	p.mu.Lock()
	defer p.mu.Unlock()

	lines := make([]string, p.rows)
	for y := 0; y < p.rows; y++ {
		var line []rune
		for x := 0; x < p.cols; x++ {
			cell := p.term.CellAt(x, y)
			if cell != nil && cell.Content != "" {
				runes := []rune(cell.Content)
				if len(runes) > 0 {
					line = append(line, runes[0])
				} else {
					line = append(line, ' ')
				}
			} else {
				line = append(line, ' ')
			}
		}
		lines[y] = string(line)
	}
	return lines
}

// GetScreenCells returns the raw cell content and formatting for direct rendering.
// This enables true cell-by-cell rendering like ratatui.
func (p *Parser) GetScreenCells() [][]CellInfo {
	p.mu.Lock()
	defer p.mu.Unlock()

	cells := make([][]CellInfo, p.rows)

	for y := 0; y < p.rows; y++ {
		cells[y] = make([]CellInfo, p.cols)
		for x := 0; x < p.cols; x++ {
			cell := p.term.CellAt(x, y)

			info := CellInfo{
				Char: ' ',
				FG:   nil,
				BG:   nil,
			}

			if cell != nil {
				// Content is a grapheme cluster (string), get first rune
				if cell.Content != "" {
					runes := []rune(cell.Content)
					if len(runes) > 0 {
						info.Char = runes[0]
					}
				}
				info.FG = cell.Style.Fg
				info.BG = cell.Style.Bg
				info.Bold = cell.Style.Attrs&uv.AttrBold != 0
				info.Dim = cell.Style.Attrs&uv.AttrFaint != 0
			}

			cells[y][x] = info
		}
	}

	return cells
}

// GetScreenAsANSI renders the screen with ANSI escape sequences.
// Suitable for streaming to a remote terminal.
func (p *Parser) GetScreenAsANSI() string {
	return p.term.Render()
}

// GetScreenForTUI returns screen lines with SGR styling codes only.
// Safe to embed in a TUI panel.
func (p *Parser) GetScreenForTUI() []string {
	// For now, return plain text - the TUI uses GetScreenCells for styling
	return p.GetScreen()
}

// GetScreenHash computes a hash for change detection.
func (p *Parser) GetScreenHash() uint64 {
	p.mu.Lock()
	defer p.mu.Unlock()

	h := fnv.New64a()

	for y := 0; y < p.rows; y++ {
		for x := 0; x < p.cols; x++ {
			cell := p.term.CellAt(x, y)
			if cell != nil && cell.Content != "" {
				h.Write([]byte(cell.Content))
			}
		}
	}

	// Include cursor position in hash
	pos := p.term.CursorPosition()
	h.Write([]byte{byte(pos.Y), byte(pos.X)})

	// Include scrollback count
	h.Write([]byte{byte(len(p.scrollback))})

	return h.Sum64()
}

// Clear resets the terminal to initial state.
func (p *Parser) Clear() {
	p.term.Write([]byte("\x1b[0m\x1b[2J\x1b[3J\x1b[H"))
}

// ClearScrollback clears the scrollback buffer.
func (p *Parser) ClearScrollback() {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.scrollback = p.scrollback[:0]
}

// ScrollbackCount returns the number of scrollback lines.
func (p *Parser) ScrollbackCount() int {
	p.mu.Lock()
	defer p.mu.Unlock()

	return len(p.scrollback)
}

// AddToScrollback adds a line to the scrollback buffer.
func (p *Parser) AddToScrollback(line string) {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.scrollback = append(p.scrollback, line)
	if len(p.scrollback) > p.maxScrollback {
		p.scrollback = p.scrollback[1:]
	}
}

// GetScrollback returns a copy of the scrollback buffer.
func (p *Parser) GetScrollback() []string {
	p.mu.Lock()
	defer p.mu.Unlock()

	result := make([]string, len(p.scrollback))
	copy(result, p.scrollback)
	return result
}

// GetContents returns the visible screen content as a single string.
func (p *Parser) GetContents() string {
	lines := p.GetScreen()
	result := ""
	for i, line := range lines {
		result += line
		if i < len(lines)-1 {
			result += "\n"
		}
	}
	return result
}
