// Package tui provides the terminal user interface using tcell for direct cell rendering.
//
// Unlike Bubble Tea, tcell gives direct buffer access - we copy cells from the
// VT100 emulator to the screen, preserving exact colors/attributes like ratatui.
package tui

import (
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/gdamore/tcell/v2"

	"github.com/trybotster/botster-hub/internal/agent"
	"github.com/trybotster/botster-hub/internal/hub"
	"github.com/trybotster/botster-hub/internal/tunnel"
	"github.com/trybotster/botster-hub/internal/vt100"
)

// TUI manages the terminal interface using tcell.
type TUI struct {
	screen tcell.Screen
	hub    *hub.Hub

	// Current mode
	mode AppMode

	// Menu state
	menuSelected int
	menuItems    []MenuItem

	// Worktree selection state
	worktreeSelected int
	worktrees        []worktreeEntry

	// New agent flow state
	newAgentBranch   string
	newAgentWorktree string

	// Text input state
	inputBuffer string

	// Terminal dimensions
	width, height int

	// Control
	quit   chan struct{}
	quitWg sync.WaitGroup

	mu sync.Mutex
}

// worktreeEntry for TUI selection (includes "create new" option).
type worktreeEntry struct {
	Path     string
	Branch   string
	IsCreate bool
	Label    string
}

// Run creates and runs the TUI (package-level function for backwards compatibility).
func Run(hubInstance *hub.Hub) error {
	t, err := NewTUI(hubInstance)
	if err != nil {
		return err
	}
	return t.Run()
}

// NewTUI creates a new tcell-based TUI.
func NewTUI(hubInstance *hub.Hub) (*TUI, error) {
	screen, err := tcell.NewScreen()
	if err != nil {
		return nil, fmt.Errorf("create screen: %w", err)
	}

	if err := screen.Init(); err != nil {
		return nil, fmt.Errorf("init screen: %w", err)
	}

	screen.EnableMouse()
	screen.EnablePaste()
	screen.Clear()

	w, h := screen.Size()

	return &TUI{
		screen: screen,
		hub:    hubInstance,
		mode:   ModeNormal,
		width:  w,
		height: h,
		quit:   make(chan struct{}),
	}, nil
}

// calculatePanelDims returns the terminal panel dimensions (rows, cols).
// This is the actual size the PTY should be, not the full terminal.
func (t *TUI) calculatePanelDims() (uint16, uint16) {
	// 30/70 split, right panel is terminal
	leftWidth := t.width * 30 / 100
	if leftWidth < 20 {
		leftWidth = 20
	}
	rightWidth := t.width - leftWidth - 1

	// Account for borders (2 chars) and help line (1 row)
	panelCols := rightWidth - 2
	panelRows := t.height - 1 - 2

	if panelCols < 10 {
		panelCols = 10
	}
	if panelRows < 5 {
		panelRows = 5
	}

	return uint16(panelRows), uint16(panelCols)
}

// Run starts the TUI event loop.
func (t *TUI) Run() error {
	defer t.screen.Fini()

	// Set initial PTY dimensions based on panel size
	panelRows, panelCols := t.calculatePanelDims()
	t.hub.SetTerminalDims(panelRows, panelCols)

	// Start render loop
	t.quitWg.Add(1)
	go t.renderLoop()

	// Event loop
	for {
		ev := t.screen.PollEvent()
		if ev == nil {
			return nil
		}

		switch ev := ev.(type) {
		case *tcell.EventResize:
			t.mu.Lock()
			t.width, t.height = ev.Size()
			// Calculate terminal panel dimensions (PTY should match this, not full terminal)
			panelRows, panelCols := t.calculatePanelDims()
			t.hub.SetTerminalDims(panelRows, panelCols)
			t.mu.Unlock()
			t.screen.Sync()

		case *tcell.EventKey:
			if t.handleKey(ev) {
				close(t.quit)
				t.quitWg.Wait()
				return nil
			}

		case *tcell.EventMouse:
			t.handleMouse(ev)
		}
	}
}

// renderLoop continuously renders the screen.
func (t *TUI) renderLoop() {
	defer t.quitWg.Done()

	ticker := time.NewTicker(50 * time.Millisecond) // 20 FPS
	defer ticker.Stop()

	for {
		select {
		case <-t.quit:
			return
		case <-ticker.C:
			t.render()
		}
	}
}

// render draws the entire screen.
func (t *TUI) render() {
	t.mu.Lock()
	defer t.mu.Unlock()

	t.screen.Clear()

	// Calculate panel dimensions (30/70 split)
	leftWidth := t.width * 30 / 100
	if leftWidth < 20 {
		leftWidth = 20
	}
	rightWidth := t.width - leftWidth - 1
	contentHeight := t.height - 1 // -1 for help line

	// Render panels
	t.renderAgentPanel(0, 0, leftWidth, contentHeight)
	t.renderTerminalPanel(leftWidth+1, 0, rightWidth, contentHeight)

	// Render help line
	t.renderHelpLine(0, t.height-1, t.width)

	// Render modal if active
	if t.mode != ModeNormal {
		t.renderModal()
	}

	t.screen.Show()
}

// Styles - use terminal defaults where possible for native feel
var (
	borderStyle  = tcell.StyleDefault.Foreground(tcell.ColorBlue)
	selectStyle  = tcell.StyleDefault.Reverse(true).Bold(true)
	normalStyle  = tcell.StyleDefault // Inherit terminal colors
	headerSty    = tcell.StyleDefault.Dim(true).Bold(true)
	helpSty      = tcell.StyleDefault.Dim(true)
	titleSty     = tcell.StyleDefault.Bold(true)
	modalBgStyle = tcell.StyleDefault
)

// renderAgentPanel renders the left panel with agent list.
func (t *TUI) renderAgentPanel(x, y, width, height int) {
	// Draw border
	t.drawBox(x, y, width, height, borderStyle)

	// Build title
	secondsSincePoll := uint64(time.Since(t.hub.LastPoll).Seconds())
	pollInterval := uint64(t.hub.Config.PollInterval)
	countdown := pollInterval - secondsSincePoll
	if countdown > pollInterval {
		countdown = 0
	}

	pollStatus := FormatPollStatus(t.hub.PollingEnabled, secondsSincePoll)
	tunnelStatus := FormatTunnelStatus(tunnel.StatusDisconnected)
	if t.hub.ConnectionURL != "" {
		tunnelStatus = FormatTunnelStatus(tunnel.StatusConnected)
	}
	vpnStatus := FormatVPNStatus(VPNStatusDisabled)

	title := fmt.Sprintf(" Agents (%d) %s %ds T:%s V:%s ",
		t.hub.AgentCount(), pollStatus, countdown, tunnelStatus, vpnStatus)

	// Draw title
	t.drawText(x+1, y, title, titleSty)

	// Draw agents
	agents := t.hub.GetAgentsOrdered()
	selectedIdx := t.hub.SelectedAgent
	innerWidth := width - 2

	for i, ag := range agents {
		if i >= height-2 {
			break
		}

		label := formatAgentLabel(ag)
		if len(label) > innerWidth-3 {
			label = label[:innerWidth-6] + "..."
		}

		style := normalStyle
		prefix := "  "
		if i == selectedIdx {
			style = selectStyle
			prefix = "> "
		}

		// Fill the line
		line := prefix + label
		for len(line) < innerWidth {
			line += " "
		}
		t.drawText(x+1, y+1+i, line[:innerWidth], style)
	}

	if len(agents) == 0 {
		t.drawText(x+2, y+1, "No agents", normalStyle)
		t.drawText(x+2, y+2, "Ctrl+P: menu", normalStyle)
	}
}

// formatAgentLabel formats an agent's display label with server status.
func formatAgentLabel(ag *agent.Agent) string {
	var label string

	if ag.IssueNumber != nil {
		label = fmt.Sprintf("%s#%d", ag.Repo, *ag.IssueNumber)
	} else if ag.Repo != "" {
		label = fmt.Sprintf("%s/%s", ag.Repo, ag.BranchName)
	} else {
		label = ag.BranchName
	}

	if ag.TunnelPort != nil {
		icon := "○"
		if ag.HasServerPTY() {
			icon = "▶"
		}
		label = fmt.Sprintf("%s %s:%d", label, icon, *ag.TunnelPort)
	}

	return label
}

// renderTerminalPanel renders the right panel with PTY content.
func (t *TUI) renderTerminalPanel(x, y, width, height int) {
	// Draw border
	t.drawBox(x, y, width, height, borderStyle)

	ag := t.hub.GetSelectedAgent()

	var title string
	if ag == nil {
		title = " Terminal "
		t.drawText(x+2, y+2, "No agent selected", normalStyle)
		t.drawText(x+2, y+3, "Press Ctrl+P to open menu", normalStyle)
	} else {
		// Build title
		var agentID string
		if ag.IssueNumber != nil {
			agentID = fmt.Sprintf("%s#%d", ag.Repo, *ag.IssueNumber)
		} else if ag.Repo != "" {
			agentID = fmt.Sprintf("%s/%s", ag.Repo, ag.BranchName)
		} else {
			agentID = ag.BranchName
		}

		viewIndicator := "[CLI]"
		if ag.HasServerPTY() {
			if ag.GetActivePTYView() == 1 {
				viewIndicator = "[SERVER | Ctrl+S: CLI]"
			} else {
				viewIndicator = "[CLI | Ctrl+S: Server]"
			}
		}

		scrollIndicator := ""
		if ag.GetScrollOffset() > 0 {
			scrollIndicator = fmt.Sprintf(" [+%d]", ag.GetScrollOffset())
		}

		title = fmt.Sprintf(" %s %s%s ", agentID, viewIndicator, scrollIndicator)

		// Render terminal content - direct cell copy from VT100
		t.renderVT100Content(ag, x+1, y+1, width-2, height-2)
	}

	// Draw title
	t.drawText(x+1, y, title, titleSty)
}

// renderVT100Content copies cells directly from VT100 buffer to screen.
// This is the key function - direct cell rendering like ratatui.
func (t *TUI) renderVT100Content(ag *agent.Agent, x, y, width, height int) {
	cells := ag.GetScreenCells()
	if cells == nil {
		t.drawText(x, y, "Terminal initializing...", normalStyle)
		return
	}

	vtRows := len(cells)
	vtCols := 0
	if vtRows > 0 {
		vtCols = len(cells[0])
	}

	// Calculate visible region (bottom of terminal if content exceeds panel)
	startRow := 0
	if vtRows > height {
		startRow = vtRows - height
	}

	for row := 0; row < height && startRow+row < vtRows; row++ {
		vtRow := startRow + row
		for col := 0; col < width && col < vtCols; col++ {
			ch := ' '
			style := tcell.StyleDefault

			if vtRow < len(cells) && col < len(cells[vtRow]) {
				cell := cells[vtRow][col]
				if cell.Char != 0 {
					ch = cell.Char
				}
				style = cellInfoToStyle(cell)
			}

			t.screen.SetContent(x+col, y+row, ch, nil, style)
		}
	}
}

// cellInfoToStyle converts vt100.CellInfo to tcell style.
func cellInfoToStyle(cell vt100.CellInfo) tcell.Style {
	style := tcell.StyleDefault

	// Foreground color
	if cell.FG != nil {
		style = style.Foreground(colorToTcell(cell.FG))
	}

	// Background color
	if cell.BG != nil {
		style = style.Background(colorToTcell(cell.BG))
	}

	// Attributes
	if cell.Bold {
		style = style.Bold(true)
	}
	if cell.Dim {
		style = style.Dim(true)
	}

	return style
}

// colorToTcell converts a color.Color to tcell color.
func colorToTcell(c interface{}) tcell.Color {
	if c == nil {
		return tcell.ColorDefault
	}

	// Try to get RGBA values
	switch v := c.(type) {
	case interface{ RGBA() (r, g, b, a uint32) }:
		r, g, b, _ := v.RGBA()
		// RGBA returns 16-bit values, convert to 8-bit
		return tcell.NewRGBColor(int32(r>>8), int32(g>>8), int32(b>>8))
	default:
		return tcell.ColorDefault
	}
}

// renderHelpLine renders the bottom help line.
func (t *TUI) renderHelpLine(x, y, width int) {
	var help string
	switch t.mode {
	case ModeNormal:
		help = "Ctrl+Q:Quit | Ctrl+P:Menu | Ctrl+J/K:Switch | Alt+PgUp/Dn:Scroll"
	case ModeMenu:
		help = "↑↓/jk:Navigate | Enter/Space:Select | Esc/q:Close"
	case ModeNewAgentSelectWorktree:
		help = "↑↓:Navigate | Enter:Select | Esc:Cancel"
	case ModeNewAgentCreateWorktree, ModeNewAgentPrompt:
		help = "Enter:Submit | Esc:Cancel"
	case ModeCloseAgentConfirm:
		help = "y:Close | d:Close+Delete | n/Esc:Cancel"
	case ModeConnectionCode:
		help = "c:Copy URL | Esc/Enter:Close"
	}

	t.drawText(x, y, help, helpSty)
}

// drawBox draws a box with single-line borders.
func (t *TUI) drawBox(x, y, width, height int, style tcell.Style) {
	// Corners
	t.screen.SetContent(x, y, tcell.RuneULCorner, nil, style)
	t.screen.SetContent(x+width-1, y, tcell.RuneURCorner, nil, style)
	t.screen.SetContent(x, y+height-1, tcell.RuneLLCorner, nil, style)
	t.screen.SetContent(x+width-1, y+height-1, tcell.RuneLRCorner, nil, style)

	// Horizontal lines
	for i := x + 1; i < x+width-1; i++ {
		t.screen.SetContent(i, y, tcell.RuneHLine, nil, style)
		t.screen.SetContent(i, y+height-1, tcell.RuneHLine, nil, style)
	}

	// Vertical lines
	for i := y + 1; i < y+height-1; i++ {
		t.screen.SetContent(x, i, tcell.RuneVLine, nil, style)
		t.screen.SetContent(x+width-1, i, tcell.RuneVLine, nil, style)
	}
}

// drawText draws text at position.
func (t *TUI) drawText(x, y int, text string, style tcell.Style) {
	for i, r := range text {
		if x+i < t.width {
			t.screen.SetContent(x+i, y, r, nil, style)
		}
	}
}

// fillRect fills a rectangle with spaces.
func (t *TUI) fillRect(x, y, width, height int, style tcell.Style) {
	for row := y; row < y+height && row < t.height; row++ {
		for col := x; col < x+width && col < t.width; col++ {
			t.screen.SetContent(col, row, ' ', nil, style)
		}
	}
}

// handleKey processes keyboard input. Returns true if should quit.
func (t *TUI) handleKey(ev *tcell.EventKey) bool {
	t.mu.Lock()
	defer t.mu.Unlock()

	switch t.mode {
	case ModeNormal:
		return t.handleNormalKey(ev)
	case ModeMenu:
		t.handleMenuKey(ev)
	case ModeNewAgentSelectWorktree:
		t.handleWorktreeSelectKey(ev)
	case ModeNewAgentCreateWorktree, ModeNewAgentPrompt:
		t.handleTextInputKey(ev)
	case ModeCloseAgentConfirm:
		t.handleCloseConfirmKey(ev)
	case ModeConnectionCode:
		t.handleConnectionCodeKey(ev)
	}
	return false
}

// handleNormalKey handles keys in normal mode.
func (t *TUI) handleNormalKey(ev *tcell.EventKey) bool {
	// Hub control uses Ctrl+key
	if ev.Modifiers()&tcell.ModCtrl != 0 {
		switch ev.Key() {
		case tcell.KeyCtrlQ:
			return true
		case tcell.KeyCtrlP:
			t.mode = ModeMenu
			t.menuSelected = 0
			t.menuItems = t.buildMenu()
			return false
		case tcell.KeyCtrlJ:
			t.hub.SelectNextAgent()
			return false
		case tcell.KeyCtrlK:
			t.hub.SelectPreviousAgent()
			return false
		case tcell.KeyCtrlRightSq: // Ctrl+]
			if ag := t.hub.GetSelectedAgent(); ag != nil && ag.HasServerPTY() {
				ag.TogglePTYView()
			}
			return false
		case tcell.KeyCtrlS: // Ctrl+S as fallback for PTY toggle
			if ag := t.hub.GetSelectedAgent(); ag != nil && ag.HasServerPTY() {
				ag.TogglePTYView()
			}
			return false
		case tcell.KeyCtrlC:
			t.sendToPTY([]byte{3})
			return false
		case tcell.KeyCtrlD:
			t.sendToPTY([]byte{4})
			return false
		case tcell.KeyCtrlZ:
			t.sendToPTY([]byte{26})
			return false
		}
	}

	// Alt+key for scrolling
	if ev.Modifiers()&tcell.ModAlt != 0 {
		switch ev.Key() {
		case tcell.KeyPgUp:
			if ag := t.hub.GetSelectedAgent(); ag != nil {
				ag.ScrollUp(t.height / 2)
			}
			return false
		case tcell.KeyPgDn:
			if ag := t.hub.GetSelectedAgent(); ag != nil {
				ag.ScrollDown(t.height / 2)
			}
			return false
		case tcell.KeyHome:
			if ag := t.hub.GetSelectedAgent(); ag != nil {
				ag.ScrollToTop()
			}
			return false
		case tcell.KeyEnd:
			if ag := t.hub.GetSelectedAgent(); ag != nil {
				ag.ScrollToBottom()
			}
			return false
		}
	}

	// Forward to PTY
	switch ev.Key() {
	case tcell.KeyEnter:
		t.sendToPTY([]byte{'\r'})
	case tcell.KeyBackspace, tcell.KeyBackspace2:
		t.sendToPTY([]byte{0x7f})
	case tcell.KeyTab:
		t.sendToPTY([]byte{'\t'})
	case tcell.KeyEscape:
		t.sendToPTY([]byte{0x1b})
	case tcell.KeyUp:
		t.sendToPTY([]byte{0x1b, '[', 'A'})
	case tcell.KeyDown:
		t.sendToPTY([]byte{0x1b, '[', 'B'})
	case tcell.KeyRight:
		t.sendToPTY([]byte{0x1b, '[', 'C'})
	case tcell.KeyLeft:
		t.sendToPTY([]byte{0x1b, '[', 'D'})
	case tcell.KeyDelete:
		t.sendToPTY([]byte{0x1b, '[', '3', '~'})
	case tcell.KeyInsert:
		t.sendToPTY([]byte{0x1b, '[', '2', '~'})
	case tcell.KeyPgUp:
		t.sendToPTY([]byte{0x1b, '[', '5', '~'})
	case tcell.KeyPgDn:
		t.sendToPTY([]byte{0x1b, '[', '6', '~'})
	case tcell.KeyHome:
		t.sendToPTY([]byte{0x1b, '[', 'H'})
	case tcell.KeyEnd:
		t.sendToPTY([]byte{0x1b, '[', 'F'})
	case tcell.KeyRune:
		t.sendToPTY([]byte(string(ev.Rune())))
	}

	return false
}

// sendToPTY sends input to the active PTY.
func (t *TUI) sendToPTY(input []byte) {
	if ag := t.hub.GetSelectedAgent(); ag != nil {
		ag.WriteInput(input)
	}
}

// handleMenuKey handles keys in menu mode.
func (t *TUI) handleMenuKey(ev *tcell.EventKey) {
	switch ev.Key() {
	case tcell.KeyEscape:
		t.mode = ModeNormal
	case tcell.KeyUp:
		if t.menuSelected > 0 {
			t.menuSelected--
		}
	case tcell.KeyDown:
		if t.menuSelected < SelectableCount(t.menuItems)-1 {
			t.menuSelected++
		}
	case tcell.KeyEnter:
		t.executeMenuAction(t.menuSelected)
	case tcell.KeyRune:
		switch ev.Rune() {
		case 'q':
			t.mode = ModeNormal
		case 'k':
			if t.menuSelected > 0 {
				t.menuSelected--
			}
		case 'j':
			if t.menuSelected < SelectableCount(t.menuItems)-1 {
				t.menuSelected++
			}
		case ' ':
			t.executeMenuAction(t.menuSelected)
		case '1', '2', '3', '4', '5', '6', '7', '8', '9':
			idx := int(ev.Rune() - '1')
			if idx < SelectableCount(t.menuItems) {
				t.executeMenuAction(idx)
			}
		}
	}
}

// handleWorktreeSelectKey handles keys in worktree selection mode.
func (t *TUI) handleWorktreeSelectKey(ev *tcell.EventKey) {
	switch ev.Key() {
	case tcell.KeyEscape:
		t.mode = ModeNormal
		t.inputBuffer = ""
	case tcell.KeyUp:
		if t.worktreeSelected > 0 {
			t.worktreeSelected--
		}
	case tcell.KeyDown:
		if t.worktreeSelected < len(t.worktrees)-1 {
			t.worktreeSelected++
		}
	case tcell.KeyEnter:
		t.selectWorktree()
	case tcell.KeyRune:
		switch ev.Rune() {
		case 'q':
			t.mode = ModeNormal
		case 'k':
			if t.worktreeSelected > 0 {
				t.worktreeSelected--
			}
		case 'j':
			if t.worktreeSelected < len(t.worktrees)-1 {
				t.worktreeSelected++
			}
		}
	}
}

// handleTextInputKey handles keys in text input modes.
func (t *TUI) handleTextInputKey(ev *tcell.EventKey) {
	switch ev.Key() {
	case tcell.KeyEscape:
		t.mode = ModeNormal
		t.inputBuffer = ""
	case tcell.KeyEnter:
		t.submitInput()
	case tcell.KeyBackspace, tcell.KeyBackspace2:
		if len(t.inputBuffer) > 0 {
			t.inputBuffer = t.inputBuffer[:len(t.inputBuffer)-1]
		}
	case tcell.KeyRune:
		t.inputBuffer += string(ev.Rune())
	}
}

// handleCloseConfirmKey handles keys in close confirmation mode.
func (t *TUI) handleCloseConfirmKey(ev *tcell.EventKey) {
	switch ev.Key() {
	case tcell.KeyEscape:
		t.mode = ModeNormal
	case tcell.KeyEnter:
		t.confirmClose(false)
	case tcell.KeyRune:
		switch ev.Rune() {
		case 'n', 'q':
			t.mode = ModeNormal
		case 'y':
			t.confirmClose(false)
		case 'd':
			t.confirmClose(true)
		}
	}
}

// handleConnectionCodeKey handles keys in connection code display mode.
func (t *TUI) handleConnectionCodeKey(ev *tcell.EventKey) {
	switch ev.Key() {
	case tcell.KeyEscape, tcell.KeyEnter:
		t.mode = ModeNormal
	case tcell.KeyRune:
		switch ev.Rune() {
		case 'q':
			t.mode = ModeNormal
		case 'c':
			// Copy URL to clipboard - would need clipboard integration
			// For now, just close the modal
		}
	}
}

// handleMouse handles mouse events.
func (t *TUI) handleMouse(ev *tcell.EventMouse) {
	t.mu.Lock()
	defer t.mu.Unlock()

	if t.mode != ModeNormal {
		return
	}

	switch ev.Buttons() {
	case tcell.WheelUp:
		if ag := t.hub.GetSelectedAgent(); ag != nil {
			ag.ScrollUp(3)
		}
	case tcell.WheelDown:
		if ag := t.hub.GetSelectedAgent(); ag != nil {
			ag.ScrollDown(3)
		}
	}
}

// renderModal renders modal overlays.
func (t *TUI) renderModal() {
	switch t.mode {
	case ModeMenu:
		t.renderMenuModal()
	case ModeNewAgentSelectWorktree:
		t.renderWorktreeModal()
	case ModeNewAgentCreateWorktree:
		t.renderInputModal("Create Worktree", "Branch name:")
	case ModeNewAgentPrompt:
		t.renderInputModal("Agent Prompt", "Initial prompt (or empty):")
	case ModeCloseAgentConfirm:
		t.renderCloseConfirmModal()
	case ModeConnectionCode:
		t.renderConnectionCodeModal()
	}
}

// renderMenuModal renders the main menu.
func (t *TUI) renderMenuModal() {
	modalWidth := 40
	modalHeight := len(t.menuItems) + 4
	x := (t.width - modalWidth) / 2
	y := (t.height - modalHeight) / 2

	// Clear background
	t.fillRect(x, y, modalWidth, modalHeight, modalBgStyle)

	// Draw border
	t.drawBox(x, y, modalWidth, modalHeight, borderStyle)

	// Title
	t.drawText(x+2, y, " Menu ", titleSty)

	// Menu items
	selectableIdx := 0
	for i, item := range t.menuItems {
		style := normalStyle
		if item.IsHeader {
			style = headerSty
		} else {
			if selectableIdx == t.menuSelected {
				style = selectStyle
			}
			selectableIdx++
		}

		label := item.Label
		if len(label) > modalWidth-4 {
			label = label[:modalWidth-7] + "..."
		}

		line := "  " + label
		for len(line) < modalWidth-2 {
			line += " "
		}
		t.drawText(x+1, y+2+i, line[:modalWidth-2], style)
	}
}

// renderWorktreeModal renders worktree selection.
func (t *TUI) renderWorktreeModal() {
	modalWidth := 50
	modalHeight := len(t.worktrees) + 4
	if modalHeight > t.height-4 {
		modalHeight = t.height - 4
	}
	x := (t.width - modalWidth) / 2
	y := (t.height - modalHeight) / 2

	t.fillRect(x, y, modalWidth, modalHeight, modalBgStyle)
	t.drawBox(x, y, modalWidth, modalHeight, borderStyle)
	t.drawText(x+2, y, " Select Worktree ", titleSty)

	for i, wt := range t.worktrees {
		if i >= modalHeight-4 {
			break
		}
		style := normalStyle
		if i == t.worktreeSelected {
			style = selectStyle
		}

		label := wt.Label
		if len(label) > modalWidth-4 {
			label = label[:modalWidth-7] + "..."
		}

		line := "  " + label
		for len(line) < modalWidth-2 {
			line += " "
		}
		t.drawText(x+1, y+2+i, line[:modalWidth-2], style)
	}
}

// renderInputModal renders a text input modal.
func (t *TUI) renderInputModal(title, prompt string) {
	modalWidth := 50
	modalHeight := 6
	x := (t.width - modalWidth) / 2
	y := (t.height - modalHeight) / 2

	t.fillRect(x, y, modalWidth, modalHeight, modalBgStyle)
	t.drawBox(x, y, modalWidth, modalHeight, borderStyle)
	t.drawText(x+2, y, " "+title+" ", titleSty)
	t.drawText(x+2, y+2, prompt, normalStyle)

	// Input field with cursor
	input := t.inputBuffer + "_"
	if len(input) > modalWidth-4 {
		input = input[len(input)-(modalWidth-4):]
	}
	t.drawText(x+2, y+3, input, normalStyle)
}

// renderCloseConfirmModal renders close confirmation.
func (t *TUI) renderCloseConfirmModal() {
	modalWidth := 45
	modalHeight := 7
	x := (t.width - modalWidth) / 2
	y := (t.height - modalHeight) / 2

	t.fillRect(x, y, modalWidth, modalHeight, modalBgStyle)
	t.drawBox(x, y, modalWidth, modalHeight, borderStyle)
	t.drawText(x+2, y, " Close Agent ", titleSty)
	t.drawText(x+2, y+2, "Close this agent?", normalStyle)
	t.drawText(x+2, y+4, "[y] Close  [d] Close+Delete  [n] Cancel", helpSty)
}

// renderConnectionCodeModal renders the connection code.
func (t *TUI) renderConnectionCodeModal() {
	url := t.hub.ConnectionURL
	modalWidth := len(url) + 6
	if modalWidth < 40 {
		modalWidth = 40
	}
	if modalWidth > t.width-4 {
		modalWidth = t.width - 4
	}
	modalHeight := 6
	x := (t.width - modalWidth) / 2
	y := (t.height - modalHeight) / 2

	t.fillRect(x, y, modalWidth, modalHeight, modalBgStyle)
	t.drawBox(x, y, modalWidth, modalHeight, borderStyle)
	t.drawText(x+2, y, " Connection URL ", titleSty)

	displayURL := url
	if displayURL == "" {
		displayURL = "(not available)"
	} else if len(displayURL) > modalWidth-4 {
		displayURL = displayURL[:modalWidth-7] + "..."
	}
	t.drawText(x+2, y+2, displayURL, normalStyle)
	t.drawText(x+2, y+4, "[c] Copy to clipboard", helpSty)
}

// buildMenu builds the menu items.
func (t *TUI) buildMenu() []MenuItem {
	ag := t.hub.GetSelectedAgent()
	hasAgent := ag != nil
	hasServerPty := hasAgent && ag.HasServerPTY()
	isServerView := hasAgent && ag.GetActivePTYView() == 1

	return BuildMenu(hasAgent, hasServerPty, isServerView, t.hub.PollingEnabled)
}

// executeMenuAction executes a menu action.
func (t *TUI) executeMenuAction(idx int) {
	items := t.menuItems
	selectableIdx := 0

	for _, item := range items {
		if item.IsHeader {
			continue
		}
		if selectableIdx == idx {
			switch item.Action {
			case "new_agent":
				t.loadWorktrees()
				t.worktreeSelected = 0
				t.mode = ModeNewAgentSelectWorktree
			case "close_agent":
				t.mode = ModeCloseAgentConfirm
			case "toggle_polling":
				t.hub.TogglePolling()
				t.mode = ModeNormal
			case "connection_code":
				t.mode = ModeConnectionCode
			case "toggle_pty":
				if ag := t.hub.GetSelectedAgent(); ag != nil {
					ag.TogglePTYView()
				}
				t.mode = ModeNormal
			}
			return
		}
		selectableIdx++
	}
}

// loadWorktrees loads available worktrees for selection.
func (t *TUI) loadWorktrees() {
	t.worktrees = nil

	// Add "create new" option first
	t.worktrees = append(t.worktrees, worktreeEntry{
		IsCreate: true,
		Label:    "[+] Create new worktree",
	})

	// Get available worktrees from Hub (filters out those with active agents)
	wts, err := t.hub.GetAvailableWorktrees()
	if err == nil {
		for _, wt := range wts {
			t.worktrees = append(t.worktrees, worktreeEntry{
				Path:   wt.Path,
				Branch: wt.Branch,
				Label:  fmt.Sprintf("%s (%s)", wt.Branch, wt.Path),
			})
		}
	}
}

// selectWorktree handles worktree selection.
func (t *TUI) selectWorktree() {
	if t.worktreeSelected >= len(t.worktrees) {
		return
	}

	wt := t.worktrees[t.worktreeSelected]
	if wt.IsCreate {
		t.mode = ModeNewAgentCreateWorktree
		t.inputBuffer = ""
	} else {
		t.newAgentWorktree = wt.Path
		t.newAgentBranch = wt.Branch
		t.mode = ModeNewAgentPrompt
		t.inputBuffer = ""
	}
}

// submitInput handles input submission.
func (t *TUI) submitInput() {
	input := strings.TrimSpace(t.inputBuffer)

	switch t.mode {
	case ModeNewAgentCreateWorktree:
		if input == "" {
			return
		}
		t.newAgentBranch = input
		t.newAgentWorktree = ""
		t.mode = ModeNewAgentPrompt
		t.inputBuffer = ""
	case ModeNewAgentPrompt:
		// Create the agent (prompt can be empty)
		t.createAgent(input)
		t.mode = ModeNormal
		t.inputBuffer = ""
		t.newAgentBranch = ""
		t.newAgentWorktree = ""
	}
}

// createAgent creates a new agent with the configured settings.
func (t *TUI) createAgent(prompt string) {
	// Use the hub to spawn the agent
	env := make(map[string]string)
	if prompt != "" {
		env["BOTSTER_PROMPT"] = prompt
	}

	err := t.hub.SpawnAgent(
		"",                  // repo - will be detected
		nil,                 // issue number
		t.newAgentBranch,    // branch
		t.newAgentWorktree,  // worktree path (empty = create new)
		"",                  // command - use default from .botster_init
		env,
	)
	if err != nil {
		// Log error - in a real implementation we'd show this in the UI
		t.hub.Logger.Error("Failed to create agent", "error", err)
	}
}

// confirmClose handles close confirmation.
func (t *TUI) confirmClose(deleteWorktree bool) {
	if ag := t.hub.GetSelectedAgent(); ag != nil {
		var err error
		if deleteWorktree {
			err = t.hub.CloseAgentAndDeleteWorktree(ag.GetID())
		} else {
			err = t.hub.CloseAgent(ag.GetID())
		}
		if err != nil {
			t.hub.Logger.Error("Failed to close agent", "error", err)
		}
	}
	t.mode = ModeNormal
}
