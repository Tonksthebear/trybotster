// Package hub provides the central state management for botster-hub.
//
// This file contains the dispatch function - the central handler for all actions.
// TUI input, browser events, and server messages all eventually become actions
// that are processed here.
package hub

import (
	"github.com/trybotster/botster-hub/internal/agent"
)

// AppMode represents the current UI mode.
type AppMode int

const (
	ModeNormal AppMode = iota
	ModeMenu
	ModeConnectionCode
	ModeCloseAgentConfirm
	ModeNewAgentSelectWorktree
	ModeNewAgentCreateWorktree
	ModeNewAgentPrompt
)

// String returns a string representation of the app mode.
func (m AppMode) String() string {
	switch m {
	case ModeNormal:
		return "Normal"
	case ModeMenu:
		return "Menu"
	case ModeConnectionCode:
		return "ConnectionCode"
	case ModeCloseAgentConfirm:
		return "CloseAgentConfirm"
	case ModeNewAgentSelectWorktree:
		return "NewAgentSelectWorktree"
	case ModeNewAgentCreateWorktree:
		return "NewAgentCreateWorktree"
	case ModeNewAgentPrompt:
		return "NewAgentPrompt"
	default:
		return "Unknown"
	}
}

// DispatchContext holds mutable state needed for dispatch.
// This is used when the Hub itself is not available (e.g., in tests).
type DispatchContext struct {
	State             *HubState
	Mode              AppMode
	MenuSelected      int
	WorktreeSelected  int
	InputBuffer       string
	PollingEnabled    bool
	TerminalRows      uint16
	TerminalCols      uint16
	Quit              bool
	ConnectionURL     string

	// Callbacks for lifecycle operations
	OnSpawnAgent   func(config *SpawnConfig) error
	OnCloseAgent   func(sessionKey string, deleteWorktree bool) error
	OnRefreshWorktrees func() error
	OnCopyToClipboard func(text string) error
}

// SpawnConfig contains configuration for spawning an agent.
type SpawnConfig struct {
	IssueNumber   *int
	BranchName    string
	WorktreePath  string
	RepoPath      string
	RepoName      string
	Prompt        string
	MessageID     *int64
	InvocationURL string
}

// NewDispatchContext creates a new dispatch context with sensible defaults.
func NewDispatchContext(state *HubState) *DispatchContext {
	return &DispatchContext{
		State:          state,
		Mode:           ModeNormal,
		PollingEnabled: true,
		TerminalRows:   24,
		TerminalCols:   80,
	}
}

// Dispatch processes a HubAction and modifies the context state accordingly.
// This is the central dispatch point for all actions.
func Dispatch(ctx *DispatchContext, action HubAction) {
	switch action.Type {
	case ActionQuit:
		ctx.Quit = true

	case ActionSelectNext:
		ctx.State.SelectNext()

	case ActionSelectPrevious:
		ctx.State.SelectPrevious()

	case ActionSelectByIndex:
		ctx.State.SelectByIndex(action.Index)

	case ActionSelectByKey:
		ctx.State.SelectByKey(action.SessionKey)

	case ActionTogglePTYView:
		if ag := ctx.State.SelectedAgent(); ag != nil {
			ag.TogglePTYView()
		}

	case ActionScrollUp:
		if ag := ctx.State.SelectedAgent(); ag != nil {
			ag.ScrollUp(action.Lines)
		}

	case ActionScrollDown:
		if ag := ctx.State.SelectedAgent(); ag != nil {
			ag.ScrollDown(action.Lines)
		}

	case ActionScrollToTop:
		if ag := ctx.State.SelectedAgent(); ag != nil {
			scrollAgentToTop(ag)
		}

	case ActionScrollToBottom:
		if ag := ctx.State.SelectedAgent(); ag != nil {
			ag.ScrollReset()
		}

	case ActionSendInput:
		if ag := ctx.State.SelectedAgent(); ag != nil {
			if err := ag.WriteInput(action.Input); err != nil {
				// Log error but don't crash - input is just dropped
			}
		}

	case ActionResize:
		ctx.TerminalRows = action.Rows
		ctx.TerminalCols = action.Cols
		for _, ag := range ctx.State.AllAgents() {
			ag.Resize(action.Rows, action.Cols)
		}

	case ActionTogglePolling:
		ctx.PollingEnabled = !ctx.PollingEnabled

	// === Agent Lifecycle ===
	case ActionSpawnAgent:
		if ctx.OnSpawnAgent != nil {
			config := &SpawnConfig{
				IssueNumber:   action.IssueNumber,
				BranchName:    action.BranchName,
				WorktreePath:  action.WorktreePath,
				RepoPath:      action.RepoPath,
				RepoName:      action.RepoName,
				Prompt:        action.Prompt,
				MessageID:     action.MessageID,
				InvocationURL: action.InvocationURL,
			}
			_ = ctx.OnSpawnAgent(config)
		}

	case ActionCloseAgent:
		if ctx.OnCloseAgent != nil {
			_ = ctx.OnCloseAgent(action.SessionKey, action.DeleteWorktree)
		}

	case ActionKillSelectedAgent:
		if key := ctx.State.SelectedSessionKey(); key != "" {
			if ctx.OnCloseAgent != nil {
				_ = ctx.OnCloseAgent(key, false)
			}
		}

	// === UI Mode ===
	case ActionOpenMenu:
		ctx.Mode = ModeMenu
		ctx.MenuSelected = 0

	case ActionCloseModal:
		ctx.Mode = ModeNormal
		ctx.InputBuffer = ""

	case ActionShowConnectionCode:
		ctx.Mode = ModeConnectionCode

	case ActionCopyConnectionURL:
		if ctx.ConnectionURL != "" && ctx.OnCopyToClipboard != nil {
			_ = ctx.OnCopyToClipboard(ctx.ConnectionURL)
		}

	// === Menu Navigation ===
	case ActionMenuUp:
		if ctx.MenuSelected > 0 {
			ctx.MenuSelected--
		}

	case ActionMenuDown:
		// Menu item count would be provided by the TUI layer
		ctx.MenuSelected++

	case ActionMenuSelect:
		handleMenuSelect(ctx, action.Index)

	// === Worktree Selection ===
	case ActionWorktreeUp:
		if ctx.WorktreeSelected > 0 {
			ctx.WorktreeSelected--
		}

	case ActionWorktreeDown:
		maxWorktrees := len(ctx.State.AvailableWorktrees())
		if ctx.WorktreeSelected < maxWorktrees {
			ctx.WorktreeSelected++
		}

	case ActionWorktreeSelect:
		if action.Index == 0 {
			// "Create new worktree" option
			ctx.Mode = ModeNewAgentCreateWorktree
			ctx.InputBuffer = ""
		} else {
			ctx.Mode = ModeNewAgentPrompt
			ctx.InputBuffer = ""
		}

	// === Text Input ===
	case ActionInputChar:
		ctx.InputBuffer += string(action.Char)

	case ActionInputBackspace:
		if len(ctx.InputBuffer) > 0 {
			// Handle UTF-8 properly
			runes := []rune(ctx.InputBuffer)
			ctx.InputBuffer = string(runes[:len(runes)-1])
		}

	case ActionInputSubmit:
		handleInputSubmit(ctx)

	case ActionInputClear:
		ctx.InputBuffer = ""

	// === Confirmation Dialogs ===
	case ActionConfirmCloseAgent:
		if key := ctx.State.SelectedSessionKey(); key != "" {
			if ctx.OnCloseAgent != nil {
				_ = ctx.OnCloseAgent(key, false)
			}
		}
		ctx.Mode = ModeNormal

	case ActionConfirmCloseAgentDeleteWorktree:
		if key := ctx.State.SelectedSessionKey(); key != "" {
			if ctx.OnCloseAgent != nil {
				_ = ctx.OnCloseAgent(key, true)
			}
		}
		ctx.Mode = ModeNormal

	case ActionRefreshWorktrees:
		if ctx.OnRefreshWorktrees != nil {
			_ = ctx.OnRefreshWorktrees()
		}

	case ActionNone:
		// No-op
	}
}

// MenuContext provides context for building menu items.
type MenuContext struct {
	HasAgent     bool
	HasServerPTY bool
	ActivePTY    agent.PTYView
	PollingEnabled bool
}

// MenuAction represents a menu item action.
type MenuAction int

const (
	MenuActionTogglePTYView MenuAction = iota
	MenuActionCloseAgent
	MenuActionNewAgent
	MenuActionShowConnectionCode
	MenuActionTogglePolling
)

// handleMenuSelect processes a menu selection.
func handleMenuSelect(ctx *DispatchContext, index int) {
	// Build menu context
	selectedAgent := ctx.State.SelectedAgent()
	menuCtx := MenuContext{
		HasAgent:       selectedAgent != nil,
		HasServerPTY:   selectedAgent != nil && selectedAgent.HasServerPTY(),
		PollingEnabled: ctx.PollingEnabled,
	}
	if selectedAgent != nil {
		menuCtx.ActivePTY = selectedAgent.GetActivePTYView()
	}

	// Get action for selection
	action := getMenuAction(menuCtx, index)

	switch action {
	case MenuActionTogglePTYView:
		Dispatch(ctx, TogglePTYViewAction())
		ctx.Mode = ModeNormal

	case MenuActionCloseAgent:
		if ctx.State.IsEmpty() {
			ctx.Mode = ModeNormal
		} else {
			ctx.Mode = ModeCloseAgentConfirm
		}

	case MenuActionNewAgent:
		if ctx.OnRefreshWorktrees != nil {
			if err := ctx.OnRefreshWorktrees(); err == nil {
				ctx.Mode = ModeNewAgentSelectWorktree
				ctx.WorktreeSelected = 0
			} else {
				ctx.Mode = ModeNormal
			}
		} else {
			ctx.Mode = ModeNewAgentSelectWorktree
			ctx.WorktreeSelected = 0
		}

	case MenuActionShowConnectionCode:
		Dispatch(ctx, ShowConnectionCodeAction())

	case MenuActionTogglePolling:
		ctx.PollingEnabled = !ctx.PollingEnabled
		ctx.Mode = ModeNormal
	}
}

// getMenuAction returns the menu action for a given selection index.
// Menu items are dynamic based on context.
func getMenuAction(ctx MenuContext, index int) MenuAction {
	// Menu structure:
	// 0: Toggle PTY View (if has server PTY)
	// 1: Close Agent (if has agent)
	// 2: New Agent
	// 3: Show Connection Code
	// 4: Toggle Polling

	currentIndex := 0

	// Toggle PTY View
	if ctx.HasServerPTY {
		if index == currentIndex {
			return MenuActionTogglePTYView
		}
		currentIndex++
	}

	// Close Agent
	if ctx.HasAgent {
		if index == currentIndex {
			return MenuActionCloseAgent
		}
		currentIndex++
	}

	// New Agent
	if index == currentIndex {
		return MenuActionNewAgent
	}
	currentIndex++

	// Show Connection Code
	if index == currentIndex {
		return MenuActionShowConnectionCode
	}
	currentIndex++

	// Toggle Polling
	if index == currentIndex {
		return MenuActionTogglePolling
	}

	// Default
	return MenuActionNewAgent
}

// handleInputSubmit processes input submission based on current mode.
func handleInputSubmit(ctx *DispatchContext) {
	switch ctx.Mode {
	case ModeNewAgentCreateWorktree:
		if ctx.InputBuffer != "" {
			// Parse branch name or issue number from input
			// Delegate to OnSpawnAgent if configured
			// For now, just clear and return to normal
		}

	case ModeNewAgentPrompt:
		// Spawn agent from selected worktree with prompt
		// For now, just clear and return to normal
	}

	ctx.Mode = ModeNormal
	ctx.InputBuffer = ""
}

// scrollAgentToTop is a helper to scroll an agent to the top of scrollback.
func scrollAgentToTop(ag *agent.Agent) {
	count := ag.ScrollbackCount()
	ag.ScrollUp(count)
}
