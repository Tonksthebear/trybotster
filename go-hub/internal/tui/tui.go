// Package tui provides the terminal user interface using Bubble Tea.
//
// The TUI renders the hub's state to the terminal and converts keyboard
// input into hub actions. It follows the Elm architecture pattern with
// Model, Update, and View functions.
package tui

import (
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/trybotster/botster-hub/internal/hub"
)

// Styles for the TUI.
var (
	titleStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(lipgloss.Color("205"))

	statusStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("240"))

	selectedStyle = lipgloss.NewStyle().
			Bold(true).
			Foreground(lipgloss.Color("86"))

	terminalBorderStyle = lipgloss.NewStyle().
				Border(lipgloss.RoundedBorder()).
				BorderForeground(lipgloss.Color("62"))
)

// Model holds the TUI state.
type Model struct {
	hub      *hub.Hub
	width    int
	height   int
	quitting bool
}

// New creates a new TUI model.
func New(h *hub.Hub) Model {
	return Model{
		hub: h,
	}
}

// Init implements tea.Model.
func (m Model) Init() tea.Cmd {
	return nil
}

// Update implements tea.Model.
func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		m.hub.SetTerminalDims(uint16(msg.Height), uint16(msg.Width))
		return m, nil

	case tea.KeyMsg:
		switch msg.String() {
		case "q", "ctrl+c":
			m.quitting = true
			m.hub.RequestQuit()
			return m, tea.Quit

		case "p":
			m.hub.TogglePolling()
			return m, nil

		case "tab":
			if ag := m.hub.GetSelectedAgent(); ag != nil {
				ag.TogglePTYView()
			}
			return m, nil

		case "left", "h":
			// Previous agent
			m.hub.SelectedAgent--
			if m.hub.SelectedAgent < 0 {
				m.hub.SelectedAgent = m.hub.AgentCount() - 1
			}
			return m, nil

		case "right", "l":
			// Next agent
			m.hub.SelectedAgent++
			if m.hub.SelectedAgent >= m.hub.AgentCount() {
				m.hub.SelectedAgent = 0
			}
			return m, nil

		default:
			// Send input to active agent
			if ag := m.hub.GetSelectedAgent(); ag != nil {
				ag.WriteInput([]byte(msg.String()))
			}
			return m, nil
		}
	}

	return m, nil
}

// View implements tea.Model.
func (m Model) View() string {
	if m.quitting {
		return "Shutting down...\n"
	}

	var b strings.Builder

	// Title bar
	title := titleStyle.Render("Botster Hub")
	status := statusStyle.Render(fmt.Sprintf(" | Agents: %d | Polling: %v",
		m.hub.AgentCount(),
		m.hub.PollingEnabled,
	))
	b.WriteString(title + status + "\n\n")

	// Connection info
	if m.hub.ConnectionURL != "" {
		b.WriteString(fmt.Sprintf("Connect: %s\n\n", m.hub.ConnectionURL))
	}

	// Agent list
	if m.hub.AgentCount() == 0 {
		b.WriteString("No agents running. Waiting for messages...\n")
	} else {
		ag := m.hub.GetSelectedAgent()
		if ag != nil {
			header := fmt.Sprintf("[%s] %s", ag.Status, ag.SessionKey())
			b.WriteString(selectedStyle.Render(header) + "\n")

			// Terminal content (placeholder - will add VT100 rendering)
			termContent := "Terminal output will appear here...\n"
			b.WriteString(terminalBorderStyle.Render(termContent))
		}
	}

	// Footer
	b.WriteString("\n")
	b.WriteString(statusStyle.Render("q: quit | p: toggle polling | tab: toggle PTY view | ←/→: switch agents"))

	return b.String()
}

// Run starts the TUI.
func Run(h *hub.Hub) error {
	p := tea.NewProgram(New(h), tea.WithAltScreen())
	_, err := p.Run()
	return err
}
