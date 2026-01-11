package agent

import (
	"strings"
	"testing"
	"time"
)

func TestNew(t *testing.T) {
	issueNum := 42
	agent := New("owner/repo", &issueNum, "botster-42", "/tmp/worktree")

	if agent.Repo != "owner/repo" {
		t.Errorf("Repo = %q, want 'owner/repo'", agent.Repo)
	}
	if agent.IssueNumber == nil || *agent.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v, want 42", agent.IssueNumber)
	}
	if agent.BranchName != "botster-42" {
		t.Errorf("BranchName = %q, want 'botster-42'", agent.BranchName)
	}
	if agent.WorktreePath != "/tmp/worktree" {
		t.Errorf("WorktreePath = %q, want '/tmp/worktree'", agent.WorktreePath)
	}
	if agent.Status != StatusInitializing {
		t.Errorf("Status = %q, want %q", agent.Status, StatusInitializing)
	}
	if agent.ID.String() == "" {
		t.Error("ID should be set")
	}
}

func TestSessionKeyWithIssue(t *testing.T) {
	issueNum := 42
	agent := New("owner/repo", &issueNum, "botster-42", "/tmp/worktree")

	key := agent.SessionKey()
	if !strings.Contains(key, "owner") || !strings.Contains(key, "repo") || !strings.Contains(key, "42") {
		t.Errorf("SessionKey = %q, should contain owner, repo, and 42", key)
	}
}

func TestSessionKeyWithBranch(t *testing.T) {
	agent := New("owner/repo", nil, "feature/new-thing", "/tmp/worktree")

	key := agent.SessionKey()
	if !strings.Contains(key, "owner") || !strings.Contains(key, "repo") || !strings.Contains(key, "feature") {
		t.Errorf("SessionKey = %q, should contain owner, repo, and branch", key)
	}
}

func TestAge(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	age := agent.Age()
	if age > time.Second {
		t.Errorf("Age = %v, should be < 1 second for new agent", age)
	}
}

func TestTogglePTYViewWithoutServer(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should stay on CLI when no server PTY
	agent.TogglePTYView()
	if agent.activePTY != PTYViewCLI {
		t.Errorf("activePTY = %v, should stay CLI when no server", agent.activePTY)
	}
}

func TestRingBuffer(t *testing.T) {
	rb := NewRingBuffer(3)

	rb.Push([]byte("a"))
	rb.Push([]byte("b"))
	rb.Push([]byte("c"))

	// Should have all 3
	data := rb.Drain()
	if string(data) != "abc" {
		t.Errorf("Drain = %q, want 'abc'", string(data))
	}

	// Should be empty after drain
	data = rb.Drain()
	if len(data) != 0 {
		t.Errorf("Drain after drain = %q, want empty", string(data))
	}
}

func TestRingBufferOverflow(t *testing.T) {
	rb := NewRingBuffer(2)

	rb.Push([]byte("a"))
	rb.Push([]byte("b"))
	rb.Push([]byte("c")) // Should drop "a"

	data := rb.Drain()
	if string(data) != "bc" {
		t.Errorf("Drain = %q, want 'bc' (oldest dropped)", string(data))
	}
}

func TestGetID(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	id := agent.GetID()
	if id == "" {
		t.Error("GetID should return non-empty string")
	}
}

func TestSpawnAndWriteInput(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Spawn a simple command
	err := agent.Spawn("cat", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	if agent.Status != StatusRunning {
		t.Errorf("Status = %q, want %q", agent.Status, StatusRunning)
	}

	// Write input
	err = agent.WriteInput([]byte("hello\n"))
	if err != nil {
		t.Errorf("WriteInput failed: %v", err)
	}

	// Wait for output
	time.Sleep(100 * time.Millisecond)

	// Drain raw output
	output := agent.DrainRawOutput()
	if !strings.Contains(string(output), "hello") {
		t.Logf("output = %q", string(output))
	}

	// Cleanup
	agent.Close()
}

func TestResize(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Spawn first
	err := agent.Spawn("sleep 1", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Resize
	err = agent.Resize(40, 120)
	if err != nil {
		t.Errorf("Resize failed: %v", err)
	}

	if agent.cliPTY.rows != 40 {
		t.Errorf("rows = %d, want 40", agent.cliPTY.rows)
	}
	if agent.cliPTY.cols != 120 {
		t.Errorf("cols = %d, want 120", agent.cliPTY.cols)
	}

	agent.Close()
}

func TestClose(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Spawn a long-running command
	err := agent.Spawn("sleep 60", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Close should complete without blocking
	done := make(chan struct{})
	go func() {
		agent.Close()
		close(done)
	}()

	select {
	case <-done:
		// Good
	case <-time.After(2 * time.Second):
		t.Error("Close blocked for too long")
	}
}

func TestDrainRawOutputEmpty(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Without spawning, should return nil
	output := agent.DrainRawOutput()
	if output != nil && len(output) != 0 {
		t.Errorf("DrainRawOutput without spawn = %v, want nil/empty", output)
	}
}

func TestPTYViewConstants(t *testing.T) {
	if PTYViewCLI != 0 {
		t.Errorf("PTYViewCLI = %d, want 0", PTYViewCLI)
	}
	if PTYViewServer != 1 {
		t.Errorf("PTYViewServer = %d, want 1", PTYViewServer)
	}
}

func TestStatusConstants(t *testing.T) {
	tests := []struct {
		status Status
		want   string
	}{
		{StatusInitializing, "initializing"},
		{StatusRunning, "running"},
		{StatusCompleted, "completed"},
		{StatusFailed, "failed"},
	}

	for _, tt := range tests {
		if string(tt.status) != tt.want {
			t.Errorf("Status = %q, want %q", tt.status, tt.want)
		}
	}
}

func TestWriteInputWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return error when no PTY
	err := agent.WriteInput([]byte("test"))
	if err == nil {
		t.Error("WriteInput should fail without PTY")
	}
}

func TestReadWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return error when no PTY
	buf := make([]byte, 100)
	_, err := agent.Read(buf)
	if err == nil {
		t.Error("Read should fail without PTY")
	}
}

func TestWriteWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return error when no PTY
	_, err := agent.Write([]byte("test"))
	if err == nil {
		t.Error("Write should fail without PTY")
	}
}

func TestSessionKeySanitization(t *testing.T) {
	// Test that repo name is sanitized (/ replaced with -)
	issueNum := 42
	agent := New("owner/repo", &issueNum, "main", "/tmp")

	key := agent.SessionKey()
	if strings.Contains(key, "/") {
		t.Errorf("SessionKey = %q, should not contain '/'", key)
	}
	if key != "owner-repo-42" {
		t.Errorf("SessionKey = %q, want 'owner-repo-42'", key)
	}
}

func TestSessionKeyBranchSanitization(t *testing.T) {
	// Test that branch name is sanitized (/ replaced with -)
	agent := New("owner/repo", nil, "feature/new-thing", "/tmp")

	key := agent.SessionKey()
	if strings.Contains(key, "/") {
		t.Errorf("SessionKey = %q, should not contain '/'", key)
	}
	if key != "owner-repo-feature-new-thing" {
		t.Errorf("SessionKey = %q, want 'owner-repo-feature-new-thing'", key)
	}
}

func TestLastActivityUpdates(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")
	initialActivity := agent.GetLastActivity()

	// Spawn a command that produces output
	err := agent.Spawn("echo 'test'", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Wait for output
	time.Sleep(100 * time.Millisecond)

	// LastActivity should have been updated
	newActivity := agent.GetLastActivity()
	if !newActivity.After(initialActivity) {
		t.Error("LastActivity should have been updated after output")
	}

	agent.Close()
}

func TestTimeSinceLastActivity(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should be very short for new agent
	duration := agent.TimeSinceLastActivity()
	if duration > time.Second {
		t.Errorf("TimeSinceLastActivity = %v, should be < 1 second", duration)
	}
}

func TestGetScreenWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return nil without PTY
	screen := agent.GetScreen()
	if screen != nil {
		t.Errorf("GetScreen without PTY = %v, want nil", screen)
	}
}

func TestGetScreenAsANSIWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return empty string without PTY
	ansi := agent.GetScreenAsANSI()
	if ansi != "" {
		t.Errorf("GetScreenAsANSI without PTY = %q, want empty", ansi)
	}
}

func TestGetScreenHashWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return 0 without PTY
	hash := agent.GetScreenHash()
	if hash != 0 {
		t.Errorf("GetScreenHash without PTY = %d, want 0", hash)
	}
}

func TestHasScreenChangedWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return false without PTY
	changed := agent.HasScreenChanged()
	if changed {
		t.Error("HasScreenChanged without PTY should be false")
	}
}

func TestScrollWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should not panic
	agent.ScrollUp(10)
	agent.ScrollDown(5)
	agent.ScrollReset()

	offset := agent.GetScrollOffset()
	if offset != 0 {
		t.Errorf("GetScrollOffset = %d, want 0", offset)
	}
}

func TestScrollUpDown(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Spawn to create PTY with parser
	err := agent.Spawn("cat", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Scroll up
	agent.ScrollUp(10)
	offset := agent.GetScrollOffset()
	// Offset might be capped at scrollback size (0 initially)
	t.Logf("Scroll offset after ScrollUp(10): %d", offset)

	// Scroll down
	agent.ScrollDown(5)
	offset = agent.GetScrollOffset()
	t.Logf("Scroll offset after ScrollDown(5): %d", offset)

	// Reset
	agent.ScrollReset()
	offset = agent.GetScrollOffset()
	if offset != 0 {
		t.Errorf("Scroll offset after reset = %d, want 0", offset)
	}

	agent.Close()
}

func TestHasServerPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should be false initially
	if agent.HasServerPTY() {
		t.Error("HasServerPTY should be false initially")
	}
}

func TestGetActivePTYView(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should be CLI by default
	view := agent.GetActivePTYView()
	if view != PTYViewCLI {
		t.Errorf("GetActivePTYView = %d, want PTYViewCLI", view)
	}
}

func TestTogglePTYViewWithServer(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Spawn CLI PTY first
	err := agent.Spawn("cat", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Spawn server PTY
	err = agent.SpawnServer("cat", nil)
	if err != nil {
		t.Fatalf("SpawnServer failed: %v", err)
	}

	if !agent.HasServerPTY() {
		t.Error("HasServerPTY should be true after SpawnServer")
	}

	// Toggle should switch to server
	agent.TogglePTYView()
	if agent.GetActivePTYView() != PTYViewServer {
		t.Error("Should have switched to server PTY")
	}

	// Toggle again should switch back to CLI
	agent.TogglePTYView()
	if agent.GetActivePTYView() != PTYViewCLI {
		t.Error("Should have switched back to CLI PTY")
	}

	agent.Close()
}

func TestNotificationsChannel(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return a channel
	ch := agent.Notifications()
	if ch == nil {
		t.Error("Notifications channel should not be nil")
	}
}

func TestGetScrollbackWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return nil without PTY
	sb := agent.GetScrollback()
	if sb != nil {
		t.Errorf("GetScrollback without PTY = %v, want nil", sb)
	}
}

func TestScrollbackCountWithoutPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Should return 0 without PTY
	count := agent.ScrollbackCount()
	if count != 0 {
		t.Errorf("ScrollbackCount without PTY = %d, want 0", count)
	}
}

func TestScreenWithSpawnedPTY(t *testing.T) {
	agent := New("test/repo", nil, "main", "/tmp")

	// Spawn a command that outputs text
	err := agent.Spawn("echo 'Hello Screen Test'", nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Wait for output to be processed
	time.Sleep(200 * time.Millisecond)

	// Get screen content
	screen := agent.GetScreen()
	if screen == nil {
		t.Error("GetScreen should return screen content after spawn")
	}

	// Get hash
	hash := agent.GetScreenHash()
	t.Logf("Screen hash: %d", hash)

	// Get ANSI screen
	ansi := agent.GetScreenAsANSI()
	if ansi == "" {
		t.Error("GetScreenAsANSI should return content after spawn")
	}

	agent.Close()
}
