// Package hub provides the central state management for botster-hub.
//
// This file contains HubAction types and the central dispatch function.
// Actions represent user intent from any input source (TUI, browser, server).
// The Hub processes actions uniformly regardless of their origin.
package hub

// ActionType identifies the kind of action.
type ActionType int

const (
	// === Agent Lifecycle ===
	ActionSpawnAgent ActionType = iota
	ActionCloseAgent
	ActionKillSelectedAgent

	// === Agent Selection ===
	ActionSelectNext
	ActionSelectPrevious
	ActionSelectByIndex
	ActionSelectByKey

	// === Agent Interaction ===
	ActionSendInput
	ActionTogglePTYView
	ActionScrollUp
	ActionScrollDown
	ActionScrollToTop
	ActionScrollToBottom

	// === UI State ===
	ActionOpenMenu
	ActionCloseModal
	ActionMenuUp
	ActionMenuDown
	ActionMenuSelect
	ActionShowConnectionCode
	ActionCopyConnectionURL

	// === Text Input ===
	ActionInputChar
	ActionInputBackspace
	ActionInputSubmit
	ActionInputClear

	// === Worktree Selection ===
	ActionWorktreeUp
	ActionWorktreeDown
	ActionWorktreeSelect

	// === Confirmation Dialogs ===
	ActionConfirmCloseAgent
	ActionConfirmCloseAgentDeleteWorktree

	// === Application Control ===
	ActionQuit
	ActionTogglePolling
	ActionRefreshWorktrees
	ActionResize
	ActionNone
)

// HubAction represents a user intention that modifies hub state.
// Actions can come from keyboard input, browser events, or server messages.
type HubAction struct {
	Type ActionType

	// === SpawnAgent fields ===
	IssueNumber   *int
	BranchName    string
	WorktreePath  string
	RepoPath      string
	RepoName      string
	Prompt        string
	MessageID     *int64
	InvocationURL string

	// === CloseAgent fields ===
	SessionKey      string
	DeleteWorktree  bool

	// === SelectByIndex field ===
	Index int

	// === SendInput field ===
	Input []byte

	// === ScrollUp/ScrollDown field ===
	Lines int

	// === InputChar field ===
	Char rune

	// === Resize fields ===
	Rows uint16
	Cols uint16
}

// --- Action Constructors ---

// SpawnAgentAction creates an action to spawn a new agent.
func SpawnAgentAction(issueNumber *int, branchName, worktreePath, repoPath, repoName, prompt string, messageID *int64, invocationURL string) HubAction {
	return HubAction{
		Type:          ActionSpawnAgent,
		IssueNumber:   issueNumber,
		BranchName:    branchName,
		WorktreePath:  worktreePath,
		RepoPath:      repoPath,
		RepoName:      repoName,
		Prompt:        prompt,
		MessageID:     messageID,
		InvocationURL: invocationURL,
	}
}

// CloseAgentAction creates an action to close an agent.
func CloseAgentAction(sessionKey string, deleteWorktree bool) HubAction {
	return HubAction{
		Type:           ActionCloseAgent,
		SessionKey:     sessionKey,
		DeleteWorktree: deleteWorktree,
	}
}

// SelectNextAction creates an action to select the next agent.
func SelectNextAction() HubAction {
	return HubAction{Type: ActionSelectNext}
}

// SelectPreviousAction creates an action to select the previous agent.
func SelectPreviousAction() HubAction {
	return HubAction{Type: ActionSelectPrevious}
}

// SelectByIndexAction creates an action to select by 1-based index.
func SelectByIndexAction(index int) HubAction {
	return HubAction{Type: ActionSelectByIndex, Index: index}
}

// SelectByKeyAction creates an action to select by session key.
func SelectByKeyAction(key string) HubAction {
	return HubAction{Type: ActionSelectByKey, SessionKey: key}
}

// SendInputAction creates an action to send input to the selected agent.
func SendInputAction(input []byte) HubAction {
	return HubAction{Type: ActionSendInput, Input: input}
}

// TogglePTYViewAction creates an action to toggle PTY view.
func TogglePTYViewAction() HubAction {
	return HubAction{Type: ActionTogglePTYView}
}

// ScrollUpAction creates an action to scroll up.
func ScrollUpAction(lines int) HubAction {
	return HubAction{Type: ActionScrollUp, Lines: lines}
}

// ScrollDownAction creates an action to scroll down.
func ScrollDownAction(lines int) HubAction {
	return HubAction{Type: ActionScrollDown, Lines: lines}
}

// ScrollToTopAction creates an action to scroll to top.
func ScrollToTopAction() HubAction {
	return HubAction{Type: ActionScrollToTop}
}

// ScrollToBottomAction creates an action to scroll to bottom.
func ScrollToBottomAction() HubAction {
	return HubAction{Type: ActionScrollToBottom}
}

// OpenMenuAction creates an action to open the menu.
func OpenMenuAction() HubAction {
	return HubAction{Type: ActionOpenMenu}
}

// CloseModalAction creates an action to close the current modal.
func CloseModalAction() HubAction {
	return HubAction{Type: ActionCloseModal}
}

// MenuUpAction creates a menu navigation up action.
func MenuUpAction() HubAction {
	return HubAction{Type: ActionMenuUp}
}

// MenuDownAction creates a menu navigation down action.
func MenuDownAction() HubAction {
	return HubAction{Type: ActionMenuDown}
}

// MenuSelectAction creates a menu selection action.
func MenuSelectAction(index int) HubAction {
	return HubAction{Type: ActionMenuSelect, Index: index}
}

// ShowConnectionCodeAction creates an action to show the QR code.
func ShowConnectionCodeAction() HubAction {
	return HubAction{Type: ActionShowConnectionCode}
}

// CopyConnectionURLAction creates an action to copy the connection URL.
func CopyConnectionURLAction() HubAction {
	return HubAction{Type: ActionCopyConnectionURL}
}

// InputCharAction creates an action to add a character to input.
func InputCharAction(c rune) HubAction {
	return HubAction{Type: ActionInputChar, Char: c}
}

// InputBackspaceAction creates an action to delete the last character.
func InputBackspaceAction() HubAction {
	return HubAction{Type: ActionInputBackspace}
}

// InputSubmitAction creates an action to submit input.
func InputSubmitAction() HubAction {
	return HubAction{Type: ActionInputSubmit}
}

// InputClearAction creates an action to clear input.
func InputClearAction() HubAction {
	return HubAction{Type: ActionInputClear}
}

// WorktreeUpAction creates a worktree navigation up action.
func WorktreeUpAction() HubAction {
	return HubAction{Type: ActionWorktreeUp}
}

// WorktreeDownAction creates a worktree navigation down action.
func WorktreeDownAction() HubAction {
	return HubAction{Type: ActionWorktreeDown}
}

// WorktreeSelectAction creates a worktree selection action.
func WorktreeSelectAction(index int) HubAction {
	return HubAction{Type: ActionWorktreeSelect, Index: index}
}

// ConfirmCloseAgentAction creates an action to confirm closing an agent.
func ConfirmCloseAgentAction() HubAction {
	return HubAction{Type: ActionConfirmCloseAgent}
}

// ConfirmCloseAgentDeleteWorktreeAction creates an action to confirm closing with worktree deletion.
func ConfirmCloseAgentDeleteWorktreeAction() HubAction {
	return HubAction{Type: ActionConfirmCloseAgentDeleteWorktree}
}

// QuitAction creates an action to quit the application.
func QuitAction() HubAction {
	return HubAction{Type: ActionQuit}
}

// TogglePollingAction creates an action to toggle message polling.
func TogglePollingAction() HubAction {
	return HubAction{Type: ActionTogglePolling}
}

// RefreshWorktreesAction creates an action to refresh available worktrees.
func RefreshWorktreesAction() HubAction {
	return HubAction{Type: ActionRefreshWorktrees}
}

// ResizeAction creates an action to handle terminal resize.
func ResizeAction(rows, cols uint16) HubAction {
	return HubAction{Type: ActionResize, Rows: rows, Cols: cols}
}

// NoneAction creates a no-op action.
func NoneAction() HubAction {
	return HubAction{Type: ActionNone}
}

// KillSelectedAgentAction creates an action to kill the selected agent.
func KillSelectedAgentAction() HubAction {
	return HubAction{Type: ActionKillSelectedAgent}
}

// --- Action Classification Methods ---

// IsPTYInput returns true if this action sends input to the PTY.
func (a HubAction) IsPTYInput() bool {
	return a.Type == ActionSendInput
}

// IsSelectionChange returns true if this action modifies agent selection.
func (a HubAction) IsSelectionChange() bool {
	switch a.Type {
	case ActionSelectNext, ActionSelectPrevious, ActionSelectByIndex, ActionSelectByKey:
		return true
	default:
		return false
	}
}

// IsScrollAction returns true if this action affects scroll state.
func (a HubAction) IsScrollAction() bool {
	switch a.Type {
	case ActionScrollUp, ActionScrollDown, ActionScrollToTop, ActionScrollToBottom:
		return true
	default:
		return false
	}
}

// IsMenuAction returns true if this action is menu-related.
func (a HubAction) IsMenuAction() bool {
	switch a.Type {
	case ActionOpenMenu, ActionCloseModal, ActionMenuUp, ActionMenuDown, ActionMenuSelect:
		return true
	default:
		return false
	}
}

// IsInputAction returns true if this action is text input-related.
func (a HubAction) IsInputAction() bool {
	switch a.Type {
	case ActionInputChar, ActionInputBackspace, ActionInputSubmit, ActionInputClear:
		return true
	default:
		return false
	}
}

// IsWorktreeAction returns true if this action is worktree selection-related.
func (a HubAction) IsWorktreeAction() bool {
	switch a.Type {
	case ActionWorktreeUp, ActionWorktreeDown, ActionWorktreeSelect:
		return true
	default:
		return false
	}
}

// String returns a string representation of the action type.
func (t ActionType) String() string {
	switch t {
	case ActionSpawnAgent:
		return "SpawnAgent"
	case ActionCloseAgent:
		return "CloseAgent"
	case ActionKillSelectedAgent:
		return "KillSelectedAgent"
	case ActionSelectNext:
		return "SelectNext"
	case ActionSelectPrevious:
		return "SelectPrevious"
	case ActionSelectByIndex:
		return "SelectByIndex"
	case ActionSelectByKey:
		return "SelectByKey"
	case ActionSendInput:
		return "SendInput"
	case ActionTogglePTYView:
		return "TogglePTYView"
	case ActionScrollUp:
		return "ScrollUp"
	case ActionScrollDown:
		return "ScrollDown"
	case ActionScrollToTop:
		return "ScrollToTop"
	case ActionScrollToBottom:
		return "ScrollToBottom"
	case ActionOpenMenu:
		return "OpenMenu"
	case ActionCloseModal:
		return "CloseModal"
	case ActionMenuUp:
		return "MenuUp"
	case ActionMenuDown:
		return "MenuDown"
	case ActionMenuSelect:
		return "MenuSelect"
	case ActionShowConnectionCode:
		return "ShowConnectionCode"
	case ActionCopyConnectionURL:
		return "CopyConnectionURL"
	case ActionInputChar:
		return "InputChar"
	case ActionInputBackspace:
		return "InputBackspace"
	case ActionInputSubmit:
		return "InputSubmit"
	case ActionInputClear:
		return "InputClear"
	case ActionWorktreeUp:
		return "WorktreeUp"
	case ActionWorktreeDown:
		return "WorktreeDown"
	case ActionWorktreeSelect:
		return "WorktreeSelect"
	case ActionConfirmCloseAgent:
		return "ConfirmCloseAgent"
	case ActionConfirmCloseAgentDeleteWorktree:
		return "ConfirmCloseAgentDeleteWorktree"
	case ActionQuit:
		return "Quit"
	case ActionTogglePolling:
		return "TogglePolling"
	case ActionRefreshWorktrees:
		return "RefreshWorktrees"
	case ActionResize:
		return "Resize"
	case ActionNone:
		return "None"
	default:
		return "Unknown"
	}
}
