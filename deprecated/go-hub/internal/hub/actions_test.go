package hub

import (
	"testing"

	"github.com/trybotster/botster-hub/internal/agent"
)

// === Action Constructor Tests ===

func TestSpawnAgentAction(t *testing.T) {
	issueNum := 42
	action := SpawnAgentAction(&issueNum, "botster-42", "/tmp/worktree", "/tmp/repo", "owner/repo", "Fix bug", nil, "")

	if action.Type != ActionSpawnAgent {
		t.Errorf("Type = %v, want ActionSpawnAgent", action.Type)
	}
	if action.IssueNumber == nil || *action.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v, want 42", action.IssueNumber)
	}
	if action.BranchName != "botster-42" {
		t.Errorf("BranchName = %q, want 'botster-42'", action.BranchName)
	}
}

func TestCloseAgentAction(t *testing.T) {
	action := CloseAgentAction("session-key", true)

	if action.Type != ActionCloseAgent {
		t.Errorf("Type = %v, want ActionCloseAgent", action.Type)
	}
	if action.SessionKey != "session-key" {
		t.Errorf("SessionKey = %q, want 'session-key'", action.SessionKey)
	}
	if !action.DeleteWorktree {
		t.Error("DeleteWorktree should be true")
	}
}

func TestNavigationActions(t *testing.T) {
	tests := []struct {
		name   string
		action HubAction
		want   ActionType
	}{
		{"SelectNext", SelectNextAction(), ActionSelectNext},
		{"SelectPrevious", SelectPreviousAction(), ActionSelectPrevious},
		{"SelectByIndex", SelectByIndexAction(3), ActionSelectByIndex},
		{"SelectByKey", SelectByKeyAction("key"), ActionSelectByKey},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.action.Type != tt.want {
				t.Errorf("Type = %v, want %v", tt.action.Type, tt.want)
			}
		})
	}
}

func TestScrollActions(t *testing.T) {
	tests := []struct {
		name   string
		action HubAction
		want   ActionType
		lines  int
	}{
		{"ScrollUp", ScrollUpAction(10), ActionScrollUp, 10},
		{"ScrollDown", ScrollDownAction(5), ActionScrollDown, 5},
		{"ScrollToTop", ScrollToTopAction(), ActionScrollToTop, 0},
		{"ScrollToBottom", ScrollToBottomAction(), ActionScrollToBottom, 0},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.action.Type != tt.want {
				t.Errorf("Type = %v, want %v", tt.action.Type, tt.want)
			}
			if tt.lines > 0 && tt.action.Lines != tt.lines {
				t.Errorf("Lines = %d, want %d", tt.action.Lines, tt.lines)
			}
		})
	}
}

func TestSendInputAction(t *testing.T) {
	input := []byte("hello world")
	action := SendInputAction(input)

	if action.Type != ActionSendInput {
		t.Errorf("Type = %v, want ActionSendInput", action.Type)
	}
	if string(action.Input) != "hello world" {
		t.Errorf("Input = %q, want 'hello world'", string(action.Input))
	}
}

func TestResizeAction(t *testing.T) {
	action := ResizeAction(40, 120)

	if action.Type != ActionResize {
		t.Errorf("Type = %v, want ActionResize", action.Type)
	}
	if action.Rows != 40 {
		t.Errorf("Rows = %d, want 40", action.Rows)
	}
	if action.Cols != 120 {
		t.Errorf("Cols = %d, want 120", action.Cols)
	}
}

func TestInputActions(t *testing.T) {
	charAction := InputCharAction('x')
	if charAction.Type != ActionInputChar {
		t.Errorf("Type = %v, want ActionInputChar", charAction.Type)
	}
	if charAction.Char != 'x' {
		t.Errorf("Char = %q, want 'x'", charAction.Char)
	}

	if InputBackspaceAction().Type != ActionInputBackspace {
		t.Error("InputBackspaceAction should have ActionInputBackspace type")
	}
	if InputSubmitAction().Type != ActionInputSubmit {
		t.Error("InputSubmitAction should have ActionInputSubmit type")
	}
	if InputClearAction().Type != ActionInputClear {
		t.Error("InputClearAction should have ActionInputClear type")
	}
}

// === Action Classification Tests ===

func TestIsPTYInput(t *testing.T) {
	if !SendInputAction([]byte{'a'}).IsPTYInput() {
		t.Error("SendInput should be PTY input")
	}
	if SelectNextAction().IsPTYInput() {
		t.Error("SelectNext should not be PTY input")
	}
	if QuitAction().IsPTYInput() {
		t.Error("Quit should not be PTY input")
	}
}

func TestIsSelectionChange(t *testing.T) {
	selectionActions := []HubAction{
		SelectNextAction(),
		SelectPreviousAction(),
		SelectByIndexAction(1),
		SelectByKeyAction("key"),
	}

	for _, action := range selectionActions {
		if !action.IsSelectionChange() {
			t.Errorf("%v should be selection change", action.Type)
		}
	}

	nonSelectionActions := []HubAction{
		SendInputAction(nil),
		QuitAction(),
		ScrollUpAction(1),
	}

	for _, action := range nonSelectionActions {
		if action.IsSelectionChange() {
			t.Errorf("%v should not be selection change", action.Type)
		}
	}
}

func TestIsScrollAction(t *testing.T) {
	scrollActions := []HubAction{
		ScrollUpAction(1),
		ScrollDownAction(1),
		ScrollToTopAction(),
		ScrollToBottomAction(),
	}

	for _, action := range scrollActions {
		if !action.IsScrollAction() {
			t.Errorf("%v should be scroll action", action.Type)
		}
	}

	if SelectNextAction().IsScrollAction() {
		t.Error("SelectNext should not be scroll action")
	}
}

func TestIsMenuAction(t *testing.T) {
	menuActions := []HubAction{
		OpenMenuAction(),
		CloseModalAction(),
		MenuUpAction(),
		MenuDownAction(),
		MenuSelectAction(0),
	}

	for _, action := range menuActions {
		if !action.IsMenuAction() {
			t.Errorf("%v should be menu action", action.Type)
		}
	}

	if QuitAction().IsMenuAction() {
		t.Error("Quit should not be menu action")
	}
}

func TestIsInputAction(t *testing.T) {
	inputActions := []HubAction{
		InputCharAction('a'),
		InputBackspaceAction(),
		InputSubmitAction(),
		InputClearAction(),
	}

	for _, action := range inputActions {
		if !action.IsInputAction() {
			t.Errorf("%v should be input action", action.Type)
		}
	}

	if QuitAction().IsInputAction() {
		t.Error("Quit should not be input action")
	}
}

func TestIsWorktreeAction(t *testing.T) {
	worktreeActions := []HubAction{
		WorktreeUpAction(),
		WorktreeDownAction(),
		WorktreeSelectAction(0),
	}

	for _, action := range worktreeActions {
		if !action.IsWorktreeAction() {
			t.Errorf("%v should be worktree action", action.Type)
		}
	}

	if QuitAction().IsWorktreeAction() {
		t.Error("Quit should not be worktree action")
	}
}

// === ActionType String Tests ===

func TestActionTypeString(t *testing.T) {
	tests := []struct {
		actionType ActionType
		want       string
	}{
		{ActionSpawnAgent, "SpawnAgent"},
		{ActionCloseAgent, "CloseAgent"},
		{ActionSelectNext, "SelectNext"},
		{ActionSelectPrevious, "SelectPrevious"},
		{ActionSendInput, "SendInput"},
		{ActionQuit, "Quit"},
		{ActionResize, "Resize"},
		{ActionNone, "None"},
	}

	for _, tt := range tests {
		t.Run(tt.want, func(t *testing.T) {
			if got := tt.actionType.String(); got != tt.want {
				t.Errorf("String() = %q, want %q", got, tt.want)
			}
		})
	}
}

// === Dispatch Context Tests ===

func TestNewDispatchContext(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	if ctx.State != state {
		t.Error("State should be set")
	}
	if ctx.Mode != ModeNormal {
		t.Errorf("Mode = %v, want ModeNormal", ctx.Mode)
	}
	if !ctx.PollingEnabled {
		t.Error("PollingEnabled should be true by default")
	}
	if ctx.TerminalRows != 24 {
		t.Errorf("TerminalRows = %d, want 24", ctx.TerminalRows)
	}
	if ctx.TerminalCols != 80 {
		t.Errorf("TerminalCols = %d, want 80", ctx.TerminalCols)
	}
}

// === Dispatch Tests ===

// newTestAgent creates an agent for testing dispatch.
func newTestAgent(repo string, issueNumber *int, branchName string) *agent.Agent {
	return agent.New(repo, issueNumber, branchName, "/tmp/worktree")
}

func TestDispatchQuit(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	if ctx.Quit {
		t.Error("Quit should be false initially")
	}

	Dispatch(ctx, QuitAction())

	if !ctx.Quit {
		t.Error("Quit should be true after QuitAction")
	}
}

func TestDispatchSelectNext(t *testing.T) {
	state := NewHubState()

	// Add three agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := newTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	ctx := NewDispatchContext(state)

	if state.SelectedIndex() != 0 {
		t.Errorf("SelectedIndex = %d, want 0", state.SelectedIndex())
	}

	Dispatch(ctx, SelectNextAction())

	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex = %d, want 1 after SelectNext", state.SelectedIndex())
	}
}

func TestDispatchSelectPrevious(t *testing.T) {
	state := NewHubState()

	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := newTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	ctx := NewDispatchContext(state)

	// Wrap backwards from 0
	Dispatch(ctx, SelectPreviousAction())

	if state.SelectedIndex() != 2 {
		t.Errorf("SelectedIndex = %d, want 2 (wrap around)", state.SelectedIndex())
	}
}

func TestDispatchSelectByIndex(t *testing.T) {
	state := NewHubState()

	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := newTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	ctx := NewDispatchContext(state)

	// Select by 1-based index
	Dispatch(ctx, SelectByIndexAction(2))

	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex = %d, want 1", state.SelectedIndex())
	}
}

func TestDispatchSelectByKey(t *testing.T) {
	state := NewHubState()

	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := newTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	ctx := NewDispatchContext(state)

	Dispatch(ctx, SelectByKeyAction("key-2"))

	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex = %d, want 1", state.SelectedIndex())
	}
}

func TestDispatchResize(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	if ctx.TerminalRows != 24 || ctx.TerminalCols != 80 {
		t.Error("Default dimensions should be 24x80")
	}

	Dispatch(ctx, ResizeAction(40, 120))

	if ctx.TerminalRows != 40 {
		t.Errorf("TerminalRows = %d, want 40", ctx.TerminalRows)
	}
	if ctx.TerminalCols != 120 {
		t.Errorf("TerminalCols = %d, want 120", ctx.TerminalCols)
	}
}

func TestDispatchTogglePolling(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	if !ctx.PollingEnabled {
		t.Error("PollingEnabled should be true initially")
	}

	Dispatch(ctx, TogglePollingAction())

	if ctx.PollingEnabled {
		t.Error("PollingEnabled should be false after toggle")
	}

	Dispatch(ctx, TogglePollingAction())

	if !ctx.PollingEnabled {
		t.Error("PollingEnabled should be true after second toggle")
	}
}

func TestDispatchOpenMenu(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.MenuSelected = 5 // Some non-zero value

	Dispatch(ctx, OpenMenuAction())

	if ctx.Mode != ModeMenu {
		t.Errorf("Mode = %v, want ModeMenu", ctx.Mode)
	}
	if ctx.MenuSelected != 0 {
		t.Errorf("MenuSelected = %d, should be reset to 0", ctx.MenuSelected)
	}
}

func TestDispatchCloseModal(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.Mode = ModeMenu
	ctx.InputBuffer = "some input"

	Dispatch(ctx, CloseModalAction())

	if ctx.Mode != ModeNormal {
		t.Errorf("Mode = %v, want ModeNormal", ctx.Mode)
	}
	if ctx.InputBuffer != "" {
		t.Errorf("InputBuffer = %q, should be empty", ctx.InputBuffer)
	}
}

func TestDispatchShowConnectionCode(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	Dispatch(ctx, ShowConnectionCodeAction())

	if ctx.Mode != ModeConnectionCode {
		t.Errorf("Mode = %v, want ModeConnectionCode", ctx.Mode)
	}
}

func TestDispatchMenuNavigation(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.MenuSelected = 2

	Dispatch(ctx, MenuUpAction())

	if ctx.MenuSelected != 1 {
		t.Errorf("MenuSelected = %d, want 1 after MenuUp", ctx.MenuSelected)
	}

	Dispatch(ctx, MenuUpAction())

	if ctx.MenuSelected != 0 {
		t.Errorf("MenuSelected = %d, want 0 after second MenuUp", ctx.MenuSelected)
	}

	// Should not go below 0
	Dispatch(ctx, MenuUpAction())

	if ctx.MenuSelected != 0 {
		t.Errorf("MenuSelected = %d, should stay at 0", ctx.MenuSelected)
	}

	Dispatch(ctx, MenuDownAction())

	if ctx.MenuSelected != 1 {
		t.Errorf("MenuSelected = %d, want 1 after MenuDown", ctx.MenuSelected)
	}
}

func TestDispatchWorktreeNavigation(t *testing.T) {
	state := NewHubState()
	state.SetAvailableWorktrees([]WorktreeInfo{
		{Path: "/path/1", Branch: "branch-1"},
		{Path: "/path/2", Branch: "branch-2"},
	})

	ctx := NewDispatchContext(state)

	Dispatch(ctx, WorktreeDownAction())

	if ctx.WorktreeSelected != 1 {
		t.Errorf("WorktreeSelected = %d, want 1", ctx.WorktreeSelected)
	}

	Dispatch(ctx, WorktreeUpAction())

	if ctx.WorktreeSelected != 0 {
		t.Errorf("WorktreeSelected = %d, want 0", ctx.WorktreeSelected)
	}

	// Should not go below 0
	Dispatch(ctx, WorktreeUpAction())

	if ctx.WorktreeSelected != 0 {
		t.Errorf("WorktreeSelected = %d, should stay at 0", ctx.WorktreeSelected)
	}
}

func TestDispatchWorktreeSelect(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	// Select index 0 means "create new worktree"
	Dispatch(ctx, WorktreeSelectAction(0))

	if ctx.Mode != ModeNewAgentCreateWorktree {
		t.Errorf("Mode = %v, want ModeNewAgentCreateWorktree", ctx.Mode)
	}

	ctx.Mode = ModeNormal

	// Select index > 0 means existing worktree
	Dispatch(ctx, WorktreeSelectAction(1))

	if ctx.Mode != ModeNewAgentPrompt {
		t.Errorf("Mode = %v, want ModeNewAgentPrompt", ctx.Mode)
	}
}

func TestDispatchInputChar(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	Dispatch(ctx, InputCharAction('H'))
	Dispatch(ctx, InputCharAction('i'))

	if ctx.InputBuffer != "Hi" {
		t.Errorf("InputBuffer = %q, want 'Hi'", ctx.InputBuffer)
	}
}

func TestDispatchInputBackspace(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.InputBuffer = "Hello"

	Dispatch(ctx, InputBackspaceAction())

	if ctx.InputBuffer != "Hell" {
		t.Errorf("InputBuffer = %q, want 'Hell'", ctx.InputBuffer)
	}

	// Backspace on empty should not panic
	ctx.InputBuffer = ""
	Dispatch(ctx, InputBackspaceAction())

	if ctx.InputBuffer != "" {
		t.Errorf("InputBuffer = %q, should stay empty", ctx.InputBuffer)
	}
}

func TestDispatchInputBackspaceUTF8(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.InputBuffer = "Hello 世界"

	Dispatch(ctx, InputBackspaceAction())

	if ctx.InputBuffer != "Hello 世" {
		t.Errorf("InputBuffer = %q, want 'Hello 世'", ctx.InputBuffer)
	}
}

func TestDispatchInputClear(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.InputBuffer = "Some text"

	Dispatch(ctx, InputClearAction())

	if ctx.InputBuffer != "" {
		t.Errorf("InputBuffer = %q, should be empty", ctx.InputBuffer)
	}
}

func TestDispatchConfirmCloseAgent(t *testing.T) {
	state := NewHubState()
	issueNum := 42
	ag := newTestAgent("owner/repo", &issueNum, "botster-42")
	state.AddAgent("owner-repo-42", ag)

	ctx := NewDispatchContext(state)
	ctx.Mode = ModeCloseAgentConfirm

	closeAgentCalled := false
	ctx.OnCloseAgent = func(sessionKey string, deleteWorktree bool) error {
		closeAgentCalled = true
		if sessionKey != "owner-repo-42" {
			t.Errorf("sessionKey = %q, want 'owner-repo-42'", sessionKey)
		}
		if deleteWorktree {
			t.Error("deleteWorktree should be false for ConfirmCloseAgent")
		}
		return nil
	}

	Dispatch(ctx, ConfirmCloseAgentAction())

	if !closeAgentCalled {
		t.Error("OnCloseAgent should have been called")
	}
	if ctx.Mode != ModeNormal {
		t.Errorf("Mode = %v, want ModeNormal", ctx.Mode)
	}
}

func TestDispatchConfirmCloseAgentDeleteWorktree(t *testing.T) {
	state := NewHubState()
	issueNum := 42
	ag := newTestAgent("owner/repo", &issueNum, "botster-42")
	state.AddAgent("owner-repo-42", ag)

	ctx := NewDispatchContext(state)
	ctx.Mode = ModeCloseAgentConfirm

	deleteWorktreeValue := false
	ctx.OnCloseAgent = func(sessionKey string, deleteWorktree bool) error {
		deleteWorktreeValue = deleteWorktree
		return nil
	}

	Dispatch(ctx, ConfirmCloseAgentDeleteWorktreeAction())

	if !deleteWorktreeValue {
		t.Error("deleteWorktree should be true")
	}
}

func TestDispatchKillSelectedAgent(t *testing.T) {
	state := NewHubState()
	issueNum := 42
	ag := newTestAgent("owner/repo", &issueNum, "botster-42")
	state.AddAgent("owner-repo-42", ag)

	ctx := NewDispatchContext(state)

	killedKey := ""
	ctx.OnCloseAgent = func(sessionKey string, deleteWorktree bool) error {
		killedKey = sessionKey
		return nil
	}

	Dispatch(ctx, KillSelectedAgentAction())

	if killedKey != "owner-repo-42" {
		t.Errorf("Killed agent key = %q, want 'owner-repo-42'", killedKey)
	}
}

func TestDispatchSpawnAgent(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	spawnCalled := false
	ctx.OnSpawnAgent = func(config *SpawnConfig) error {
		spawnCalled = true
		if config.RepoName != "owner/repo" {
			t.Errorf("RepoName = %q, want 'owner/repo'", config.RepoName)
		}
		return nil
	}

	issueNum := 42
	Dispatch(ctx, SpawnAgentAction(&issueNum, "botster-42", "/tmp/wt", "/tmp/repo", "owner/repo", "Fix bug", nil, ""))

	if !spawnCalled {
		t.Error("OnSpawnAgent should have been called")
	}
}

func TestDispatchCloseAgent(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	closedKey := ""
	closedDeleteWT := false
	ctx.OnCloseAgent = func(sessionKey string, deleteWorktree bool) error {
		closedKey = sessionKey
		closedDeleteWT = deleteWorktree
		return nil
	}

	Dispatch(ctx, CloseAgentAction("session-123", true))

	if closedKey != "session-123" {
		t.Errorf("Closed key = %q, want 'session-123'", closedKey)
	}
	if !closedDeleteWT {
		t.Error("DeleteWorktree should be true")
	}
}

func TestDispatchRefreshWorktrees(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	refreshCalled := false
	ctx.OnRefreshWorktrees = func() error {
		refreshCalled = true
		return nil
	}

	Dispatch(ctx, RefreshWorktreesAction())

	if !refreshCalled {
		t.Error("OnRefreshWorktrees should have been called")
	}
}

func TestDispatchNone(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)
	ctx.Mode = ModeMenu
	ctx.InputBuffer = "test"

	// None action should not modify anything
	Dispatch(ctx, NoneAction())

	if ctx.Mode != ModeMenu {
		t.Errorf("Mode = %v, should not have changed", ctx.Mode)
	}
	if ctx.InputBuffer != "test" {
		t.Errorf("InputBuffer = %q, should not have changed", ctx.InputBuffer)
	}
}

func TestDispatchWithNoAgentSelected(t *testing.T) {
	state := NewHubState()
	ctx := NewDispatchContext(state)

	// These should not panic even with no agent selected
	Dispatch(ctx, SendInputAction([]byte("hello")))
	Dispatch(ctx, TogglePTYViewAction())
	Dispatch(ctx, ScrollUpAction(10))
	Dispatch(ctx, ScrollDownAction(5))
	Dispatch(ctx, ScrollToTopAction())
	Dispatch(ctx, ScrollToBottomAction())
}

// === AppMode Tests ===

func TestAppModeString(t *testing.T) {
	tests := []struct {
		mode AppMode
		want string
	}{
		{ModeNormal, "Normal"},
		{ModeMenu, "Menu"},
		{ModeConnectionCode, "ConnectionCode"},
		{ModeCloseAgentConfirm, "CloseAgentConfirm"},
		{ModeNewAgentSelectWorktree, "NewAgentSelectWorktree"},
		{ModeNewAgentCreateWorktree, "NewAgentCreateWorktree"},
		{ModeNewAgentPrompt, "NewAgentPrompt"},
	}

	for _, tt := range tests {
		t.Run(tt.want, func(t *testing.T) {
			if got := tt.mode.String(); got != tt.want {
				t.Errorf("String() = %q, want %q", got, tt.want)
			}
		})
	}
}

// === Menu Action Tests ===

func TestGetMenuActionNoAgent(t *testing.T) {
	ctx := MenuContext{
		HasAgent:       false,
		HasServerPTY:   false,
		PollingEnabled: true,
	}

	// Without agent, menu is: NewAgent, ShowConnectionCode, TogglePolling
	if getMenuAction(ctx, 0) != MenuActionNewAgent {
		t.Error("Index 0 should be NewAgent")
	}
	if getMenuAction(ctx, 1) != MenuActionShowConnectionCode {
		t.Error("Index 1 should be ShowConnectionCode")
	}
	if getMenuAction(ctx, 2) != MenuActionTogglePolling {
		t.Error("Index 2 should be TogglePolling")
	}
}

func TestGetMenuActionWithAgent(t *testing.T) {
	ctx := MenuContext{
		HasAgent:       true,
		HasServerPTY:   false,
		PollingEnabled: true,
	}

	// With agent (no server PTY): CloseAgent, NewAgent, ShowConnectionCode, TogglePolling
	if getMenuAction(ctx, 0) != MenuActionCloseAgent {
		t.Error("Index 0 should be CloseAgent")
	}
	if getMenuAction(ctx, 1) != MenuActionNewAgent {
		t.Error("Index 1 should be NewAgent")
	}
}

func TestGetMenuActionWithServerPTY(t *testing.T) {
	ctx := MenuContext{
		HasAgent:       true,
		HasServerPTY:   true,
		PollingEnabled: true,
	}

	// With server PTY: TogglePTYView, CloseAgent, NewAgent, ShowConnectionCode, TogglePolling
	if getMenuAction(ctx, 0) != MenuActionTogglePTYView {
		t.Error("Index 0 should be TogglePTYView")
	}
	if getMenuAction(ctx, 1) != MenuActionCloseAgent {
		t.Error("Index 1 should be CloseAgent")
	}
}
