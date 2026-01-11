package notification

import (
	"testing"
)

func TestStandaloneBellIgnored(t *testing.T) {
	// Standalone BEL character is ignored (not useful for agent notifications)
	data := []byte("some output\x07more output")
	notifications := Detect(data)
	if len(notifications) != 0 {
		t.Errorf("len = %d, want 0 (standalone BEL should be ignored)", len(notifications))
	}
}

func TestDetectOSC9WithBELTerminator(t *testing.T) {
	// OSC 9 with BEL terminator: ESC ] 9 ; message BEL
	data := []byte("\x1b]9;Test notification\x07")
	notifications := Detect(data)

	if len(notifications) != 1 {
		t.Fatalf("len = %d, want 1", len(notifications))
	}
	if notifications[0].Type != TypeOSC9 {
		t.Errorf("Type = %q, want %q", notifications[0].Type, TypeOSC9)
	}
	if notifications[0].Message != "Test notification" {
		t.Errorf("Message = %q, want 'Test notification'", notifications[0].Message)
	}
}

func TestDetectOSC9WithSTTerminator(t *testing.T) {
	// OSC 9 with ST terminator: ESC ] 9 ; message ESC \
	data := []byte("\x1b]9;Agent notification\x1b\\")
	notifications := Detect(data)

	if len(notifications) != 1 {
		t.Fatalf("len = %d, want 1", len(notifications))
	}
	if notifications[0].Type != TypeOSC9 {
		t.Errorf("Type = %q, want %q", notifications[0].Type, TypeOSC9)
	}
	if notifications[0].Message != "Agent notification" {
		t.Errorf("Message = %q, want 'Agent notification'", notifications[0].Message)
	}
}

func TestDetectOSC777Notification(t *testing.T) {
	// OSC 777: ESC ] 777 ; notify ; title ; body BEL
	data := []byte("\x1b]777;notify;Build Complete;All tests passed\x07")
	notifications := Detect(data)

	if len(notifications) != 1 {
		t.Fatalf("len = %d, want 1", len(notifications))
	}
	if notifications[0].Type != TypeOSC777 {
		t.Errorf("Type = %q, want %q", notifications[0].Type, TypeOSC777)
	}
	if notifications[0].Title != "Build Complete" {
		t.Errorf("Title = %q, want 'Build Complete'", notifications[0].Title)
	}
	if notifications[0].Body != "All tests passed" {
		t.Errorf("Body = %q, want 'All tests passed'", notifications[0].Body)
	}
}

func TestNoFalsePositiveBELInOSC(t *testing.T) {
	// BEL inside OSC should not trigger standalone Bell notification
	data := []byte("\x1b]9;message\x07")
	notifications := Detect(data)

	if len(notifications) != 1 {
		t.Fatalf("len = %d, want 1", len(notifications))
	}
	// Should be OSC9, not something else
	if notifications[0].Type != TypeOSC9 {
		t.Errorf("Type = %q, want %q", notifications[0].Type, TypeOSC9)
	}
}

func TestOSC9FiltersEscapeSequenceMessages(t *testing.T) {
	// OSC 9 with escape-sequence-like content (just numbers/semicolons) should be filtered
	data := []byte("\x1b]9;4;0;\x07")
	notifications := Detect(data)

	if len(notifications) != 0 {
		t.Errorf("len = %d, want 0 (should filter escape-sequence-like messages)", len(notifications))
	}

	// But real messages should still work
	data = []byte("\x1b]9;Real notification message\x07")
	notifications = Detect(data)

	if len(notifications) != 1 {
		t.Fatalf("len = %d, want 1", len(notifications))
	}
	if notifications[0].Message != "Real notification message" {
		t.Errorf("Message = %q, want 'Real notification message'", notifications[0].Message)
	}
}

func TestMultipleNotifications(t *testing.T) {
	// Multiple notifications in one buffer
	data := []byte("\x07\x1b]9;first\x07\x07\x1b]9;second\x1b\\")
	notifications := Detect(data)

	// Should detect: OSC9("first"), OSC9("second") - no standalone Bell
	if len(notifications) != 2 {
		t.Errorf("len = %d, want 2", len(notifications))
	}
}

func TestNoNotificationsInRegularOutput(t *testing.T) {
	// Regular output without OSC sequences
	data := []byte("Building project...\nCompilation complete.")
	notifications := Detect(data)

	if len(notifications) != 0 {
		t.Errorf("len = %d, want 0", len(notifications))
	}
}

func TestOSC777TitleOnly(t *testing.T) {
	// OSC 777 with title but no body
	data := []byte("\x1b]777;notify;Title Only\x07")
	notifications := Detect(data)

	if len(notifications) != 1 {
		t.Fatalf("len = %d, want 1", len(notifications))
	}
	if notifications[0].Title != "Title Only" {
		t.Errorf("Title = %q, want 'Title Only'", notifications[0].Title)
	}
	if notifications[0].Body != "" {
		t.Errorf("Body = %q, want empty", notifications[0].Body)
	}
}

func TestOSC777EmptyFiltered(t *testing.T) {
	// OSC 777 with empty title and body should be filtered
	data := []byte("\x1b]777;notify;\x07")
	notifications := Detect(data)

	if len(notifications) != 0 {
		t.Errorf("len = %d, want 0 (empty notification should be filtered)", len(notifications))
	}
}

func TestMixedContent(t *testing.T) {
	// Regular output mixed with notifications
	data := []byte("Starting build...\x1b]9;Build started\x07\nCompiling...\x1b]777;notify;Done;Success\x07End")
	notifications := Detect(data)

	if len(notifications) != 2 {
		t.Fatalf("len = %d, want 2", len(notifications))
	}
	if notifications[0].Type != TypeOSC9 {
		t.Errorf("notifications[0].Type = %q, want %q", notifications[0].Type, TypeOSC9)
	}
	if notifications[1].Type != TypeOSC777 {
		t.Errorf("notifications[1].Type = %q, want %q", notifications[1].Type, TypeOSC777)
	}
}

func TestIsEscapeSequence(t *testing.T) {
	tests := []struct {
		input string
		want  bool
	}{
		{"4;0;", true},
		{"123", true},
		{";", true},
		{"", false},
		{"hello", false},
		{"4;0;hello", false},
		{"Real message", false},
	}

	for _, tt := range tests {
		got := isEscapeSequence(tt.input)
		if got != tt.want {
			t.Errorf("isEscapeSequence(%q) = %v, want %v", tt.input, got, tt.want)
		}
	}
}

func TestAgentStatus(t *testing.T) {
	tests := []struct {
		status AgentStatus
		want   string
	}{
		{StatusInitializing, "initializing"},
		{StatusRunning, "running"},
		{StatusFinished, "finished"},
		{StatusFailed, "failed"},
		{StatusKilled, "killed"},
	}

	for _, tt := range tests {
		if string(tt.status) != tt.want {
			t.Errorf("AgentStatus = %q, want %q", tt.status, tt.want)
		}
	}
}
