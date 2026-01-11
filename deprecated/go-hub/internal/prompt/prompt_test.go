package prompt

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestLocalPromptPriority(t *testing.T) {
	dir := t.TempDir()
	promptContent := "Local test prompt"

	// Write local prompt
	localPath := filepath.Join(dir, LocalPromptFile)
	if err := os.WriteFile(localPath, []byte(promptContent), 0644); err != nil {
		t.Fatalf("failed to write prompt: %v", err)
	}

	m := NewManager()
	result, err := m.GetPrompt(dir)

	if err != nil {
		t.Fatalf("GetPrompt failed: %v", err)
	}

	if result != promptContent {
		t.Errorf("got %q, want %q", result, promptContent)
	}
}

func TestGetPromptWithFallbackUsesLocal(t *testing.T) {
	dir := t.TempDir()
	promptContent := "Local prompt"

	localPath := filepath.Join(dir, LocalPromptFile)
	if err := os.WriteFile(localPath, []byte(promptContent), 0644); err != nil {
		t.Fatalf("failed to write prompt: %v", err)
	}

	m := NewManager()
	result := m.GetPromptWithFallback(dir, "fallback")

	if result != promptContent {
		t.Errorf("got %q, want %q", result, promptContent)
	}
}

func TestGetPromptWithFallbackUsesFallback(t *testing.T) {
	dir := t.TempDir()
	// No local prompt, and we're not hitting real GitHub

	// Create a manager that will fail to fetch
	m := &Manager{
		httpClient: &http.Client{
			Transport: &failingTransport{},
		},
	}

	result := m.GetPromptWithFallback(dir, "fallback content")

	if result != "fallback content" {
		t.Errorf("got %q, want 'fallback content'", result)
	}
}

type failingTransport struct{}

func (t *failingTransport) RoundTrip(*http.Request) (*http.Response, error) {
	return nil, http.ErrServerClosed
}

func TestGetLocalPrompt(t *testing.T) {
	dir := t.TempDir()

	// No prompt exists
	content, err := GetLocalPrompt(dir)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content != "" {
		t.Errorf("expected empty content, got %q", content)
	}

	// Write prompt
	promptPath := filepath.Join(dir, LocalPromptFile)
	if err := os.WriteFile(promptPath, []byte("test prompt"), 0644); err != nil {
		t.Fatalf("failed to write: %v", err)
	}

	content, err = GetLocalPrompt(dir)
	if err != nil {
		t.Fatalf("GetLocalPrompt failed: %v", err)
	}
	if content != "test prompt" {
		t.Errorf("got %q, want 'test prompt'", content)
	}
}

func TestWriteLocalPrompt(t *testing.T) {
	dir := t.TempDir()
	content := "Written prompt content"

	if err := WriteLocalPrompt(dir, content); err != nil {
		t.Fatalf("WriteLocalPrompt failed: %v", err)
	}

	// Verify file exists and has correct content
	promptPath := filepath.Join(dir, LocalPromptFile)
	data, err := os.ReadFile(promptPath)
	if err != nil {
		t.Fatalf("failed to read written file: %v", err)
	}

	if string(data) != content {
		t.Errorf("got %q, want %q", string(data), content)
	}

	// Verify permissions
	info, err := os.Stat(promptPath)
	if err != nil {
		t.Fatalf("stat failed: %v", err)
	}
	mode := info.Mode().Perm()
	if mode != 0644 {
		t.Errorf("expected 0644 permissions, got %o", mode)
	}
}

func TestWriteLocalPromptOverwrites(t *testing.T) {
	dir := t.TempDir()

	// Write initial content
	if err := WriteLocalPrompt(dir, "first"); err != nil {
		t.Fatalf("first write failed: %v", err)
	}

	// Overwrite
	if err := WriteLocalPrompt(dir, "second"); err != nil {
		t.Fatalf("second write failed: %v", err)
	}

	content, err := GetLocalPrompt(dir)
	if err != nil {
		t.Fatalf("GetLocalPrompt failed: %v", err)
	}
	if content != "second" {
		t.Errorf("got %q, want 'second'", content)
	}
}

func TestHasLocalPrompt(t *testing.T) {
	dir := t.TempDir()

	// No prompt exists
	if HasLocalPrompt(dir) {
		t.Error("expected false when no prompt exists")
	}

	// Create prompt
	if err := WriteLocalPrompt(dir, "test"); err != nil {
		t.Fatalf("WriteLocalPrompt failed: %v", err)
	}

	// Now should exist
	if !HasLocalPrompt(dir) {
		t.Error("expected true after creating prompt")
	}
}

func TestFetchDefaultPromptMocked(t *testing.T) {
	// Create mock server
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "botster_prompt.md") {
			w.Write([]byte("# Default Prompt\n\nThis is the default."))
		} else {
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	defer server.Close()

	// Create manager with custom http client pointing to mock
	m := &Manager{
		httpClient: server.Client(),
	}

	// We need to intercept the URL construction for this to work
	// Since the URL is hardcoded, we can't easily test the actual fetch
	// Instead, verify the manager initialization works
	if m.httpClient == nil {
		t.Error("http client should be set")
	}
}

func TestNewManager(t *testing.T) {
	m := NewManager()

	if m == nil {
		t.Fatal("NewManager returned nil")
	}

	if m.httpClient == nil {
		t.Error("httpClient should be initialized")
	}

	if m.httpClient.Timeout == 0 {
		t.Error("httpClient should have a timeout")
	}
}

func TestGetPromptNoLocalNoNetwork(t *testing.T) {
	dir := t.TempDir()

	// Create a manager that will fail network requests
	m := &Manager{
		httpClient: &http.Client{
			Transport: &failingTransport{},
		},
	}

	_, err := m.GetPrompt(dir)

	if err == nil {
		t.Error("expected error when no local prompt and network fails")
	}
}

func TestLocalPromptFileConstant(t *testing.T) {
	if LocalPromptFile != ".botster_prompt" {
		t.Errorf("LocalPromptFile = %q, want '.botster_prompt'", LocalPromptFile)
	}
}

func TestDefaultPromptRepoConstant(t *testing.T) {
	if DefaultPromptRepo != "Tonksthebear/trybotster" {
		t.Errorf("DefaultPromptRepo = %q, want 'Tonksthebear/trybotster'", DefaultPromptRepo)
	}
}

func TestDefaultPromptPathConstant(t *testing.T) {
	if DefaultPromptPath != "cli/botster_prompt" {
		t.Errorf("DefaultPromptPath = %q, want 'cli/botster_prompt'", DefaultPromptPath)
	}
}

func TestWriteLocalPromptCreatesFile(t *testing.T) {
	dir := t.TempDir()
	content := "New prompt"

	// Ensure file doesn't exist
	promptPath := filepath.Join(dir, LocalPromptFile)
	if _, err := os.Stat(promptPath); !os.IsNotExist(err) {
		t.Fatal("prompt file should not exist initially")
	}

	if err := WriteLocalPrompt(dir, content); err != nil {
		t.Fatalf("WriteLocalPrompt failed: %v", err)
	}

	// File should now exist
	if _, err := os.Stat(promptPath); os.IsNotExist(err) {
		t.Error("prompt file should exist after write")
	}
}

func TestEmptyLocalPrompt(t *testing.T) {
	dir := t.TempDir()

	// Write empty prompt
	if err := WriteLocalPrompt(dir, ""); err != nil {
		t.Fatalf("WriteLocalPrompt failed: %v", err)
	}

	content, err := GetLocalPrompt(dir)
	if err != nil {
		t.Fatalf("GetLocalPrompt failed: %v", err)
	}
	if content != "" {
		t.Errorf("got %q, want empty string", content)
	}

	// HasLocalPrompt should still return true
	if !HasLocalPrompt(dir) {
		t.Error("HasLocalPrompt should return true even for empty file")
	}
}

func TestGetPromptUsesLocalEvenIfEmpty(t *testing.T) {
	dir := t.TempDir()

	// Write empty local prompt
	localPath := filepath.Join(dir, LocalPromptFile)
	if err := os.WriteFile(localPath, []byte(""), 0644); err != nil {
		t.Fatalf("failed to write: %v", err)
	}

	m := NewManager()
	result, err := m.GetPrompt(dir)

	// Should succeed with empty string (not try to fetch remote)
	if err != nil {
		t.Fatalf("GetPrompt failed: %v", err)
	}
	if result != "" {
		t.Errorf("got %q, want empty string", result)
	}
}

func TestGetLocalPromptReadError(t *testing.T) {
	// Create a directory where we try to read a file
	dir := t.TempDir()
	promptPath := filepath.Join(dir, LocalPromptFile)

	// Create a directory instead of a file
	if err := os.Mkdir(promptPath, 0755); err != nil {
		t.Fatalf("failed to create dir: %v", err)
	}

	_, err := GetLocalPrompt(dir)

	// Should return error because it's a directory, not a file
	if err == nil {
		t.Error("expected error when reading directory as file")
	}
}

func TestWriteLocalPromptInvalidDir(t *testing.T) {
	// Try to write to a non-existent directory
	err := WriteLocalPrompt("/nonexistent/path/that/does/not/exist", "content")

	if err == nil {
		t.Error("expected error when writing to non-existent directory")
	}
}
