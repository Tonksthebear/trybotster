package pty

import (
	"strings"
	"testing"
	"time"
)

func TestNewSession(t *testing.T) {
	session := New(24, 80, nil)

	if session.rows != 24 {
		t.Errorf("rows = %d, want 24", session.rows)
	}
	if session.cols != 80 {
		t.Errorf("cols = %d, want 80", session.cols)
	}
	if session.IsSpawned() {
		t.Error("IsSpawned() = true before spawn")
	}
}

func TestSessionSize(t *testing.T) {
	session := New(30, 120, nil)

	rows, cols := session.Size()
	if rows != 30 {
		t.Errorf("rows = %d, want 30", rows)
	}
	if cols != 120 {
		t.Errorf("cols = %d, want 120", cols)
	}
}

func TestSpawnEcho(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "echo",
		Args:    []string{"hello", "world"},
		Dir:     "/tmp",
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	if !session.IsSpawned() {
		t.Error("IsSpawned() = false after spawn")
	}

	// Wait for output
	time.Sleep(100 * time.Millisecond)

	// Drain raw output
	output := session.DrainRawOutput()
	if !strings.Contains(string(output), "hello world") {
		t.Errorf("output = %q, want to contain 'hello world'", string(output))
	}

	// Cleanup
	session.Kill()
}

func TestSpawnBashCommand(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "/bin/bash",
		Args:    []string{"-c", "echo test_output_123"},
		Dir:     "/tmp",
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Wait for output
	time.Sleep(100 * time.Millisecond)

	output := session.DrainRawOutput()
	if !strings.Contains(string(output), "test_output_123") {
		t.Errorf("output = %q, want to contain 'test_output_123'", string(output))
	}

	session.Kill()
}

func TestResize(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "/bin/bash",
		Args:    []string{"-c", "sleep 1"},
		Dir:     "/tmp",
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Resize
	if err := session.Resize(40, 120); err != nil {
		t.Errorf("Resize failed: %v", err)
	}

	rows, cols := session.Size()
	if rows != 40 {
		t.Errorf("rows = %d, want 40", rows)
	}
	if cols != 120 {
		t.Errorf("cols = %d, want 120", cols)
	}

	session.Kill()
}

func TestWriteInput(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "/bin/cat",
		Dir:     "/tmp",
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Write to PTY
	_, err = session.WriteString("hello from test\n")
	if err != nil {
		t.Errorf("WriteString failed: %v", err)
	}

	// Wait for echo back
	time.Sleep(100 * time.Millisecond)

	output := session.DrainRawOutput()
	if !strings.Contains(string(output), "hello from test") {
		t.Errorf("output = %q, want to contain 'hello from test'", string(output))
	}

	session.Kill()
}

func TestBufferManagement(t *testing.T) {
	session := New(24, 80, nil)

	// Add lines directly to buffer
	for i := 0; i < MaxBufferLines+100; i++ {
		session.addToBuffer("test line")
	}

	snapshot := session.GetBufferSnapshot()
	if len(snapshot) != MaxBufferLines {
		t.Errorf("buffer len = %d, want %d", len(snapshot), MaxBufferLines)
	}
}

func TestKill(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "/bin/bash",
		Args:    []string{"-c", "sleep 60"},
		Dir:     "/tmp",
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Kill should complete without blocking
	done := make(chan struct{})
	go func() {
		session.Kill()
		close(done)
	}()

	select {
	case <-done:
		// Good - kill completed
	case <-time.After(2 * time.Second):
		t.Error("Kill() blocked for too long")
	}
}

func TestDrainRawOutput(t *testing.T) {
	session := New(24, 80, nil)

	// Queue some output manually
	session.rawOutputLock.Lock()
	session.rawOutput = append(session.rawOutput, []byte("chunk1"))
	session.rawOutput = append(session.rawOutput, []byte("chunk2"))
	session.rawOutputLock.Unlock()

	// Drain
	output := session.DrainRawOutput()
	if string(output) != "chunk1chunk2" {
		t.Errorf("DrainRawOutput = %q, want 'chunk1chunk2'", string(output))
	}

	// Should be empty now
	output2 := session.DrainRawOutput()
	if len(output2) != 0 {
		t.Errorf("Second drain = %q, want empty", string(output2))
	}
}

func TestInitCommands(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "/bin/cat",
		Dir:     "/tmp",
		InitCommands: []string{
			"init_cmd_1",
			"init_cmd_2",
		},
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	// Wait for init commands to be processed
	time.Sleep(100 * time.Millisecond)

	output := session.DrainRawOutput()
	if !strings.Contains(string(output), "init_cmd_1") {
		t.Errorf("output = %q, want to contain 'init_cmd_1'", string(output))
	}
	if !strings.Contains(string(output), "init_cmd_2") {
		t.Errorf("output = %q, want to contain 'init_cmd_2'", string(output))
	}

	session.Kill()
}

func TestEnvVars(t *testing.T) {
	session := New(24, 80, nil)

	err := session.Spawn(SpawnConfig{
		Command: "/bin/bash",
		Args:    []string{"-c", "echo $TEST_VAR_123"},
		Dir:     "/tmp",
		Env:     []string{"TEST_VAR_123=hello_env"},
	})
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}

	time.Sleep(100 * time.Millisecond)

	output := session.DrainRawOutput()
	if !strings.Contains(string(output), "hello_env") {
		t.Errorf("output = %q, want to contain 'hello_env'", string(output))
	}

	session.Kill()
}
