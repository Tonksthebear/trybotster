package relay

import (
	"encoding/json"
	"testing"

	"github.com/trybotster/botster-hub/internal/hub"
)

// ========== TerminalMessage Tests ==========

func TestOutputMessage(t *testing.T) {
	msg := OutputMessage("hello")
	if msg.Type != "output" {
		t.Errorf("Type = %q, want 'output'", msg.Type)
	}
	if msg.Data != "hello" {
		t.Errorf("Data = %q, want 'hello'", msg.Data)
	}

	data, err := json.Marshal(msg)
	if err != nil {
		t.Fatalf("Marshal error: %v", err)
	}
	if string(data) != `{"type":"output","data":"hello"}` {
		t.Errorf("JSON = %s", data)
	}
}

func TestAgentsMessage(t *testing.T) {
	repo := "owner/repo"
	agents := []AgentInfo{{ID: "test-id", Repo: &repo}}
	msg := AgentsMessage(agents)

	if msg.Type != "agents" {
		t.Errorf("Type = %q", msg.Type)
	}
	if len(msg.Agents) != 1 {
		t.Errorf("Agents len = %d", len(msg.Agents))
	}
}

func TestWorktreesMessage(t *testing.T) {
	worktrees := []WorktreeInfo{{Path: "/path", Branch: "main"}}
	msg := WorktreesMessage(worktrees, "owner/repo")

	if msg.Type != "worktrees" {
		t.Errorf("Type = %q", msg.Type)
	}
	if msg.Repo != "owner/repo" {
		t.Errorf("Repo = %q", msg.Repo)
	}
}

func TestAgentSelectedMessage(t *testing.T) {
	msg := AgentSelectedMessage("agent-123")
	if msg.Type != "agent_selected" {
		t.Errorf("Type = %q", msg.Type)
	}
	if msg.ID != "agent-123" {
		t.Errorf("ID = %q", msg.ID)
	}
}

func TestErrorMessage(t *testing.T) {
	msg := ErrorMessage("something went wrong")
	if msg.Type != "error" {
		t.Errorf("Type = %q", msg.Type)
	}
	if msg.Message != "something went wrong" {
		t.Errorf("Message = %q", msg.Message)
	}
}

func TestScrollbackMessage(t *testing.T) {
	lines := []string{"line1", "line2", "line3"}
	msg := ScrollbackMessage(lines)

	if msg.Type != "scrollback" {
		t.Errorf("Type = %q", msg.Type)
	}
	if len(msg.Lines) != 3 {
		t.Errorf("Lines len = %d", len(msg.Lines))
	}
}

// ========== BrowserCommand Parsing Tests ==========

func TestParseBrowserCommandInput(t *testing.T) {
	data := []byte(`{"type":"input","data":"ls -la"}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "input" {
		t.Errorf("Type = %q", cmd.Type)
	}
	if cmd.Data != "ls -la" {
		t.Errorf("Data = %q", cmd.Data)
	}
}

func TestParseBrowserCommandSetMode(t *testing.T) {
	data := []byte(`{"type":"set_mode","mode":"gui"}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "set_mode" {
		t.Errorf("Type = %q", cmd.Type)
	}
	if cmd.Mode != "gui" {
		t.Errorf("Mode = %q", cmd.Mode)
	}
}

func TestParseBrowserCommandListAgents(t *testing.T) {
	data := []byte(`{"type":"list_agents"}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "list_agents" {
		t.Errorf("Type = %q", cmd.Type)
	}
}

func TestParseBrowserCommandSelectAgent(t *testing.T) {
	data := []byte(`{"type":"select_agent","id":"agent-abc-123"}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "select_agent" {
		t.Errorf("Type = %q", cmd.Type)
	}
	if cmd.ID != "agent-abc-123" {
		t.Errorf("ID = %q", cmd.ID)
	}
}

func TestParseBrowserCommandCreateAgent(t *testing.T) {
	data := []byte(`{"type":"create_agent","issue_or_branch":"42","prompt":"Fix the bug"}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "create_agent" {
		t.Errorf("Type = %q", cmd.Type)
	}
	if cmd.IssueOrBranch == nil || *cmd.IssueOrBranch != "42" {
		t.Errorf("IssueOrBranch = %v", cmd.IssueOrBranch)
	}
	if cmd.Prompt == nil || *cmd.Prompt != "Fix the bug" {
		t.Errorf("Prompt = %v", cmd.Prompt)
	}
}

func TestParseBrowserCommandScroll(t *testing.T) {
	data := []byte(`{"type":"scroll","direction":"up","lines":5}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "scroll" {
		t.Errorf("Type = %q", cmd.Type)
	}
	if cmd.Direction != "up" {
		t.Errorf("Direction = %q", cmd.Direction)
	}
	if cmd.Lines == nil || *cmd.Lines != 5 {
		t.Errorf("Lines = %v", cmd.Lines)
	}
}

func TestParseBrowserCommandResize(t *testing.T) {
	data := []byte(`{"type":"resize","cols":120,"rows":40}`)
	cmd, err := ParseBrowserCommand(data)
	if err != nil {
		t.Fatalf("Parse error: %v", err)
	}
	if cmd.Type != "resize" {
		t.Errorf("Type = %q", cmd.Type)
	}
	if cmd.Cols != 120 {
		t.Errorf("Cols = %d", cmd.Cols)
	}
	if cmd.Rows != 40 {
		t.Errorf("Rows = %d", cmd.Rows)
	}
}

func TestParseBrowserCommandInvalid(t *testing.T) {
	data := []byte(`not valid json`)
	_, err := ParseBrowserCommand(data)
	if err == nil {
		t.Error("Expected error for invalid JSON")
	}
}

// ========== CommandToEvent Tests ==========

func TestCommandToEventInput(t *testing.T) {
	cmd := &BrowserCommand{Type: "input", Data: "test"}
	event := CommandToEvent(cmd)
	if event.Type != EventInput {
		t.Errorf("Type = %v, want EventInput", event.Type)
	}
	if event.Data != "test" {
		t.Errorf("Data = %q", event.Data)
	}
}

func TestCommandToEventSetMode(t *testing.T) {
	cmd := &BrowserCommand{Type: "set_mode", Mode: "gui"}
	event := CommandToEvent(cmd)
	if event.Type != EventSetMode {
		t.Errorf("Type = %v", event.Type)
	}
	if event.Mode != "gui" {
		t.Errorf("Mode = %q", event.Mode)
	}
}

func TestCommandToEventScroll(t *testing.T) {
	lines := uint32(5)
	cmd := &BrowserCommand{Type: "scroll", Direction: "up", Lines: &lines}
	event := CommandToEvent(cmd)
	if event.Type != EventScroll {
		t.Errorf("Type = %v", event.Type)
	}
	if event.Direction != "up" {
		t.Errorf("Direction = %q", event.Direction)
	}
	if event.Lines != 5 {
		t.Errorf("Lines = %d", event.Lines)
	}
}

func TestCommandToEventScrollDefaultLines(t *testing.T) {
	cmd := &BrowserCommand{Type: "scroll", Direction: "down"}
	event := CommandToEvent(cmd)
	if event.Lines != 10 {
		t.Errorf("Lines = %d, want 10 (default)", event.Lines)
	}
}

func TestCommandToEventResize(t *testing.T) {
	cmd := &BrowserCommand{Type: "resize", Cols: 120, Rows: 40}
	event := CommandToEvent(cmd)
	if event.Type != EventResize {
		t.Errorf("Type = %v", event.Type)
	}
	if event.Resize == nil {
		t.Fatal("Resize is nil")
	}
	if event.Resize.Cols != 120 || event.Resize.Rows != 40 {
		t.Errorf("Resize = %v", event.Resize)
	}
}

func TestCommandToEventHandshake(t *testing.T) {
	cmd := &BrowserCommand{
		Type:              "handshake",
		DeviceName:        "Test Device",
		BrowserCurve25519: "base64key",
	}
	event := CommandToEvent(cmd)
	if event.Type != EventConnected {
		t.Errorf("Type = %v", event.Type)
	}
	if event.DeviceName != "Test Device" {
		t.Errorf("DeviceName = %q", event.DeviceName)
	}
	if event.PublicKey != "base64key" {
		t.Errorf("PublicKey = %q", event.PublicKey)
	}
}

// ========== BrowserEventToHubAction Tests ==========

func defaultContext() *BrowserEventContext {
	return &BrowserEventContext{
		WorktreeBase: "/tmp/worktrees",
		RepoPath:     "/home/user/repo",
		RepoName:     "owner/repo",
	}
}

func TestEventToActionInput(t *testing.T) {
	event := &BrowserEvent{Type: EventInput, Data: "hello"}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action == nil {
		t.Fatal("Action is nil")
	}
	if action.Type != hub.ActionSendInput {
		t.Errorf("Type = %v", action.Type)
	}
	if string(action.Input) != "hello" {
		t.Errorf("InputData = %q", action.Input)
	}
}

func TestEventToActionSelectAgent(t *testing.T) {
	event := &BrowserEvent{Type: EventSelectAgent, ID: "owner-repo-42"}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action == nil {
		t.Fatal("Action is nil")
	}
	if action.Type != hub.ActionSelectByKey {
		t.Errorf("Type = %v", action.Type)
	}
	if action.SessionKey != "owner-repo-42" {
		t.Errorf("SessionKey = %q", action.SessionKey)
	}
}

func TestEventToActionDeleteAgent(t *testing.T) {
	event := &BrowserEvent{
		Type:           EventDeleteAgent,
		ID:             "owner-repo-42",
		DeleteWorktree: true,
	}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action == nil {
		t.Fatal("Action is nil")
	}
	if action.Type != hub.ActionCloseAgent {
		t.Errorf("Type = %v", action.Type)
	}
	if action.SessionKey != "owner-repo-42" {
		t.Errorf("SessionKey = %q", action.SessionKey)
	}
	if !action.DeleteWorktree {
		t.Error("DeleteWorktree should be true")
	}
}

func TestEventToActionScroll(t *testing.T) {
	ctx := defaultContext()

	up := &BrowserEvent{Type: EventScroll, Direction: "up", Lines: 5}
	upAction := BrowserEventToHubAction(up, ctx)
	if upAction.Type != hub.ActionScrollUp || upAction.Lines != 5 {
		t.Errorf("ScrollUp action = %v", upAction)
	}

	down := &BrowserEvent{Type: EventScroll, Direction: "down", Lines: 10}
	downAction := BrowserEventToHubAction(down, ctx)
	if downAction.Type != hub.ActionScrollDown || downAction.Lines != 10 {
		t.Errorf("ScrollDown action = %v", downAction)
	}
}

func TestEventToActionScrollToBottomTop(t *testing.T) {
	ctx := defaultContext()

	bottom := &BrowserEvent{Type: EventScrollToBottom}
	bottomAction := BrowserEventToHubAction(bottom, ctx)
	if bottomAction.Type != hub.ActionScrollToBottom {
		t.Errorf("Type = %v", bottomAction.Type)
	}

	top := &BrowserEvent{Type: EventScrollToTop}
	topAction := BrowserEventToHubAction(top, ctx)
	if topAction.Type != hub.ActionScrollToTop {
		t.Errorf("Type = %v", topAction.Type)
	}
}

func TestEventToActionTogglePtyView(t *testing.T) {
	event := &BrowserEvent{Type: EventTogglePtyView}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action.Type != hub.ActionTogglePTYView {
		t.Errorf("Type = %v", action.Type)
	}
}

func TestEventToActionResize(t *testing.T) {
	event := &BrowserEvent{
		Type:   EventResize,
		Resize: &BrowserResize{Rows: 40, Cols: 120},
	}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action.Type != hub.ActionResize {
		t.Errorf("Type = %v", action.Type)
	}
	if action.Rows != 40 || action.Cols != 120 {
		t.Errorf("Rows=%d, Cols=%d", action.Rows, action.Cols)
	}
}

func TestEventToActionConnectedReturnsNil(t *testing.T) {
	event := &BrowserEvent{Type: EventConnected}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action != nil {
		t.Error("Connected event should not produce action")
	}
}

func TestEventToActionListEventsReturnNil(t *testing.T) {
	ctx := defaultContext()

	list := &BrowserEvent{Type: EventListAgents}
	if BrowserEventToHubAction(list, ctx) != nil {
		t.Error("ListAgents should return nil")
	}

	worktrees := &BrowserEvent{Type: EventListWorktrees}
	if BrowserEventToHubAction(worktrees, ctx) != nil {
		t.Error("ListWorktrees should return nil")
	}
}

func TestEventToActionCreateAgentWithIssueNumber(t *testing.T) {
	issueNum := "42"
	event := &BrowserEvent{
		Type:          EventCreateAgent,
		IssueOrBranch: &issueNum,
	}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action == nil {
		t.Fatal("Action is nil")
	}
	if action.Type != hub.ActionSpawnAgent {
		t.Errorf("Type = %v", action.Type)
	}
	if action.IssueNumber == nil || *action.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v", action.IssueNumber)
	}
	if action.BranchName != "botster-issue-42" {
		t.Errorf("BranchName = %q", action.BranchName)
	}
}

func TestEventToActionCreateAgentWithBranch(t *testing.T) {
	branch := "feature-branch"
	event := &BrowserEvent{
		Type:          EventCreateAgent,
		IssueOrBranch: &branch,
	}
	ctx := defaultContext()
	action := BrowserEventToHubAction(event, ctx)

	if action.IssueNumber != nil {
		t.Errorf("IssueNumber should be nil, got %v", action.IssueNumber)
	}
	if action.BranchName != "feature-branch" {
		t.Errorf("BranchName = %q", action.BranchName)
	}
}

// ========== BrowserState Tests ==========

func TestBrowserStateDefault(t *testing.T) {
	state := NewBrowserState()
	if state.IsConnected() {
		t.Error("Should not be connected initially")
	}
	if state.Dims != nil {
		t.Error("Dims should be nil")
	}
}

func TestBrowserStateDisconnect(t *testing.T) {
	state := NewBrowserState()
	state.Connected = true
	state.Dims = &BrowserResize{Rows: 24, Cols: 80}
	hash := uint64(12345)
	state.LastScreenHash = &hash

	state.Disconnect()

	if state.Connected {
		t.Error("Should be disconnected")
	}
	if state.Dims != nil {
		t.Error("Dims should be nil")
	}
	if state.LastScreenHash != nil {
		t.Error("LastScreenHash should be nil")
	}
}

func TestBrowserStateHandleConnected(t *testing.T) {
	state := NewBrowserState()
	state.HandleConnected("Test Device")

	if !state.Connected {
		t.Error("Should be connected")
	}
	if state.Mode == nil || *state.Mode != BrowserModeGUI {
		t.Errorf("Mode = %v, want GUI", state.Mode)
	}
}

func TestBrowserStateHandleResize(t *testing.T) {
	state := NewBrowserState()
	hash := uint64(12345)
	state.LastScreenHash = &hash

	rows, cols := state.HandleResize(BrowserResize{Rows: 40, Cols: 120})

	if rows != 40 || cols != 120 {
		t.Errorf("rows=%d, cols=%d", rows, cols)
	}
	if state.Dims == nil {
		t.Error("Dims should not be nil")
	}
	if state.LastScreenHash != nil {
		t.Error("LastScreenHash should be invalidated")
	}
}

func TestBrowserStateHandleSetModeGUI(t *testing.T) {
	state := NewBrowserState()
	state.HandleSetMode("gui")
	if state.Mode == nil || *state.Mode != BrowserModeGUI {
		t.Errorf("Mode = %v", state.Mode)
	}
}

func TestBrowserStateHandleSetModeTUI(t *testing.T) {
	state := NewBrowserState()
	state.HandleSetMode("tui")
	if state.Mode == nil || *state.Mode != BrowserModeTUI {
		t.Errorf("Mode = %v", state.Mode)
	}
}

func TestBrowserStateInvalidateScreen(t *testing.T) {
	state := NewBrowserState()
	hash := uint64(12345)
	state.LastScreenHash = &hash

	state.InvalidateScreen()

	if state.LastScreenHash != nil {
		t.Error("LastScreenHash should be nil")
	}
}

func TestBrowserStateSetTailscaleInfo(t *testing.T) {
	state := NewBrowserState()
	state.SetTailscaleInfo("https://example.com/hubs/abc#key=xxx", "cli-abc.tail.local")

	if state.TailscaleConnectionURL != "https://example.com/hubs/abc#key=xxx" {
		t.Errorf("TailscaleConnectionURL = %q", state.TailscaleConnectionURL)
	}
	if state.TailscaleHostname != "cli-abc.tail.local" {
		t.Errorf("TailscaleHostname = %q", state.TailscaleHostname)
	}
}

// ========== Helper Function Tests ==========

func TestCalculateAgentDimsGUI(t *testing.T) {
	dims := &BrowserResize{Rows: 40, Cols: 120}
	cols, rows := CalculateAgentDims(dims, BrowserModeGUI)
	if cols != 120 || rows != 40 {
		t.Errorf("cols=%d, rows=%d", cols, rows)
	}
}

func TestCalculateAgentDimsTUI(t *testing.T) {
	dims := &BrowserResize{Rows: 40, Cols: 100}
	cols, rows := CalculateAgentDims(dims, BrowserModeTUI)
	// 70% of 100 = 70, minus 2 = 68
	if cols != 68 {
		t.Errorf("cols = %d, want 68", cols)
	}
	// 40 minus 2 = 38
	if rows != 38 {
		t.Errorf("rows = %d, want 38", rows)
	}
}

func TestGetOutputForModeGUI(t *testing.T) {
	gui := BrowserModeGUI
	agentOutput := "agent output"
	output := GetOutputForMode(&gui, "tui stuff", &agentOutput)
	if output != "agent output" {
		t.Errorf("output = %q", output)
	}
}

func TestGetOutputForModeGUINoAgent(t *testing.T) {
	gui := BrowserModeGUI
	output := GetOutputForMode(&gui, "tui stuff", nil)
	if output != "\x1b[2J\x1b[HNo agent selected" {
		t.Errorf("output = %q", output)
	}
}

func TestGetOutputForModeTUI(t *testing.T) {
	tui := BrowserModeTUI
	agentOutput := "agent output"
	output := GetOutputForMode(&tui, "tui stuff", &agentOutput)
	if output != "tui stuff" {
		t.Errorf("output = %q", output)
	}
}

func TestGetOutputForModeNil(t *testing.T) {
	output := GetOutputForMode(nil, "tui stuff", nil)
	if output != "tui stuff" {
		t.Errorf("output = %q", output)
	}
}

func TestBuildWorktreeInfo(t *testing.T) {
	info := BuildWorktreeInfo("/path/to/worktree", "botster-issue-42")
	if info.Path != "/path/to/worktree" {
		t.Errorf("Path = %q", info.Path)
	}
	if info.Branch != "botster-issue-42" {
		t.Errorf("Branch = %q", info.Branch)
	}
	if info.IssueNumber == nil || *info.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v", info.IssueNumber)
	}
}

func TestBuildWorktreeInfoNoIssue(t *testing.T) {
	info := BuildWorktreeInfo("/path", "feature-branch")
	if info.IssueNumber != nil {
		t.Errorf("IssueNumber should be nil, got %v", info.IssueNumber)
	}
}

// ========== TerminalOutputSender Tests ==========

func TestTerminalOutputSender(t *testing.T) {
	ch := make(chan string, 10)
	sender := NewTerminalOutputSender(ch)

	if sender.IsClosed() {
		t.Error("Should not be closed initially")
	}

	err := sender.Send("test output")
	if err != nil {
		t.Errorf("Send error: %v", err)
	}

	select {
	case msg := <-ch:
		if msg != "test output" {
			t.Errorf("Received = %q", msg)
		}
	default:
		t.Error("No message received")
	}

	sender.Close()
	if !sender.IsClosed() {
		t.Error("Should be closed after Close()")
	}
}

func TestTerminalOutputSenderDropsOnFull(t *testing.T) {
	ch := make(chan string, 1)
	sender := NewTerminalOutputSender(ch)

	// Fill channel
	sender.Send("first")

	// This should not block
	err := sender.Send("second")
	if err != nil {
		t.Errorf("Send error: %v", err)
	}

	// Only first message should be in channel
	msg := <-ch
	if msg != "first" {
		t.Errorf("Expected 'first', got %q", msg)
	}
}

// ========== AgentInfo Serialization Tests ==========

func TestAgentInfoSerialization(t *testing.T) {
	repo := "owner/repo"
	issueNum := uint64(42)
	branch := "botster-issue-42"
	status := "Running"
	port := uint16(3000)
	running := true
	hasPty := true
	view := "cli"
	offset := uint32(0)
	hubID := "hub-123"

	info := AgentInfo{
		ID:            "test-id",
		Repo:          &repo,
		IssueNumber:   &issueNum,
		BranchName:    &branch,
		Status:        &status,
		TunnelPort:    &port,
		ServerRunning: &running,
		HasServerPty:  &hasPty,
		ActivePtyView: &view,
		ScrollOffset:  &offset,
		HubIdentifier: &hubID,
	}

	data, err := json.Marshal(info)
	if err != nil {
		t.Fatalf("Marshal error: %v", err)
	}

	var decoded AgentInfo
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("Unmarshal error: %v", err)
	}

	if decoded.ID != "test-id" {
		t.Errorf("ID = %q", decoded.ID)
	}
	if decoded.IssueNumber == nil || *decoded.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v", decoded.IssueNumber)
	}
}

// ========== CheckBrowserResize Tests ==========

func TestCheckBrowserResizeConnected(t *testing.T) {
	// Reset state
	lastDims = 0
	wasConnected = false

	dims := &BrowserDimsWithMode{Rows: 40, Cols: 120, Mode: BrowserModeGUI}
	result := CheckBrowserResize(dims, [2]uint16{24, 80})

	if result.Action != ResizeAgents {
		t.Errorf("Action = %v, want ResizeAgents", result.Action)
	}
	if result.Rows != 40 || result.Cols != 120 {
		t.Errorf("Rows=%d, Cols=%d", result.Rows, result.Cols)
	}
}

func TestCheckBrowserResizeTUIMode(t *testing.T) {
	// Reset state
	lastDims = 0
	wasConnected = false

	dims := &BrowserDimsWithMode{Rows: 40, Cols: 100, Mode: BrowserModeTUI}
	result := CheckBrowserResize(dims, [2]uint16{24, 80})

	if result.Action != ResizeAgents {
		t.Errorf("Action = %v", result.Action)
	}
	// 70% of 100 = 70, minus 2 = 68
	if result.Cols != 68 {
		t.Errorf("Cols = %d, want 68", result.Cols)
	}
	// 40 - 2 = 38
	if result.Rows != 38 {
		t.Errorf("Rows = %d, want 38", result.Rows)
	}
}

func TestCheckBrowserResizeNoChange(t *testing.T) {
	// Reset state
	lastDims = 0
	wasConnected = false

	dims := &BrowserDimsWithMode{Rows: 40, Cols: 120, Mode: BrowserModeGUI}

	// First call should trigger resize
	result1 := CheckBrowserResize(dims, [2]uint16{24, 80})
	if result1.Action != ResizeAgents {
		t.Errorf("First call should trigger resize")
	}

	// Second call with same dims should be no-op
	result2 := CheckBrowserResize(dims, [2]uint16{24, 80})
	if result2.Action != ResizeNone {
		t.Errorf("Second call should be ResizeNone, got %v", result2.Action)
	}
}

func TestCheckBrowserResizeDisconnected(t *testing.T) {
	// Reset state
	lastDims = 0
	wasConnected = true // Simulate previous connection

	result := CheckBrowserResize(nil, [2]uint16{24, 80})

	if result.Action != ResetToLocal {
		t.Errorf("Action = %v, want ResetToLocal", result.Action)
	}
}
