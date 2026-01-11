package git

import (
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
)

func TestNew(t *testing.T) {
	manager := New("/tmp/worktrees", nil)
	if manager.baseDir != "/tmp/worktrees" {
		t.Errorf("baseDir = %q, want /tmp/worktrees", manager.baseDir)
	}
}

func TestNewWithLogger(t *testing.T) {
	logger := slog.Default()
	manager := New("/tmp/worktrees", logger)
	if manager.logger != logger {
		t.Error("logger should be set")
	}
}

func TestExtractRepoNameHTTPS(t *testing.T) {
	tests := []struct {
		url  string
		want string
	}{
		{"https://github.com/owner/repo.git", "owner/repo"},
		{"https://github.com/owner/repo", "owner/repo"},
		{"http://github.com/owner/repo.git", "owner/repo"},
	}

	for _, tt := range tests {
		got := extractRepoName(tt.url)
		if got != tt.want {
			t.Errorf("extractRepoName(%q) = %q, want %q", tt.url, got, tt.want)
		}
	}
}

func TestExtractRepoNameSSH(t *testing.T) {
	tests := []struct {
		url  string
		want string
	}{
		{"git@github.com:owner/repo.git", "owner/repo"},
		{"git@github.com:owner/repo", "owner/repo"},
	}

	for _, tt := range tests {
		got := extractRepoName(tt.url)
		if got != tt.want {
			t.Errorf("extractRepoName(%q) = %q, want %q", tt.url, got, tt.want)
		}
	}
}

func TestReadBotsterCopyPatterns(t *testing.T) {
	// Create temp directory with .botster_copy file
	tmpDir := t.TempDir()
	botsterCopy := filepath.Join(tmpDir, ".botster_copy")

	content := `# This is a comment
*.env
config/*.json

# Another comment
tmp/**
`
	if err := os.WriteFile(botsterCopy, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	patterns, err := ReadBotsterCopyPatterns(tmpDir)
	if err != nil {
		t.Fatal(err)
	}

	expected := []string{"*.env", "config/*.json", "tmp/**"}
	if len(patterns) != len(expected) {
		t.Fatalf("len(patterns) = %d, want %d", len(patterns), len(expected))
	}

	for i, p := range patterns {
		if p != expected[i] {
			t.Errorf("patterns[%d] = %q, want %q", i, p, expected[i])
		}
	}
}

func TestReadBotsterCopyPatternsNoFile(t *testing.T) {
	tmpDir := t.TempDir()

	patterns, err := ReadBotsterCopyPatterns(tmpDir)
	if err != nil {
		t.Fatal(err)
	}

	if patterns != nil {
		t.Errorf("patterns = %v, want nil for missing file", patterns)
	}
}

func TestReadBotsterInitCommands(t *testing.T) {
	tmpDir := t.TempDir()
	botsterInit := filepath.Join(tmpDir, ".botster_init")

	content := `# Setup commands
npm install
npm run build
# More setup
echo "done"
`
	if err := os.WriteFile(botsterInit, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	commands, err := ReadBotsterInitCommands(tmpDir)
	if err != nil {
		t.Fatal(err)
	}

	expected := []string{"npm install", "npm run build", `echo "done"`}
	if len(commands) != len(expected) {
		t.Fatalf("len(commands) = %d, want %d", len(commands), len(expected))
	}

	for i, c := range commands {
		if c != expected[i] {
			t.Errorf("commands[%d] = %q, want %q", i, c, expected[i])
		}
	}
}

func TestReadBotsterTeardownCommands(t *testing.T) {
	tmpDir := t.TempDir()
	botsterTeardown := filepath.Join(tmpDir, ".botster_teardown")

	content := `# Cleanup commands
npm run clean
rm -rf node_modules
`
	if err := os.WriteFile(botsterTeardown, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	commands, err := ReadBotsterTeardownCommands(tmpDir)
	if err != nil {
		t.Fatal(err)
	}

	expected := []string{"npm run clean", "rm -rf node_modules"}
	if len(commands) != len(expected) {
		t.Fatalf("len(commands) = %d, want %d", len(commands), len(expected))
	}
}

func TestReadBotsterTeardownCommandsNoFile(t *testing.T) {
	tmpDir := t.TempDir()

	commands, err := ReadBotsterTeardownCommands(tmpDir)
	if err != nil {
		t.Fatal(err)
	}

	if commands != nil {
		t.Errorf("commands = %v, want nil for missing file", commands)
	}
}

func TestWriteClaudeSettings(t *testing.T) {
	tmpDir := t.TempDir()

	if err := WriteClaudeSettings(tmpDir); err != nil {
		t.Fatal(err)
	}

	// Check file was created
	settingsPath := filepath.Join(tmpDir, ".claude", "settings.local.json")
	data, err := os.ReadFile(settingsPath)
	if err != nil {
		t.Fatal(err)
	}

	var settings ClaudeSettings
	if err := json.Unmarshal(data, &settings); err != nil {
		t.Fatal(err)
	}

	if len(settings.AllowedDirectories) != 1 {
		t.Errorf("len(AllowedDirectories) = %d, want 1", len(settings.AllowedDirectories))
	}

	if settings.AllowedDirectories[0] != tmpDir {
		t.Errorf("AllowedDirectories[0] = %q, want %q", settings.AllowedDirectories[0], tmpDir)
	}

	if settings.PermissionMode != "acceptEdits" {
		t.Errorf("PermissionMode = %q, want 'acceptEdits'", settings.PermissionMode)
	}
}

func TestCopyBotsterFilesNoPatterns(t *testing.T) {
	srcDir := t.TempDir()
	destDir := t.TempDir()

	// No .botster_copy file - should not error
	if err := CopyBotsterFiles(srcDir, destDir); err != nil {
		t.Fatal(err)
	}
}

func TestCopyBotsterFiles(t *testing.T) {
	srcDir := t.TempDir()
	destDir := t.TempDir()

	// Create .botster_copy
	botsterCopy := filepath.Join(srcDir, ".botster_copy")
	if err := os.WriteFile(botsterCopy, []byte("*.env\nconfig/*.json"), 0644); err != nil {
		t.Fatal(err)
	}

	// Create files to copy
	if err := os.WriteFile(filepath.Join(srcDir, ".env"), []byte("SECRET=value"), 0644); err != nil {
		t.Fatal(err)
	}

	configDir := filepath.Join(srcDir, "config")
	if err := os.MkdirAll(configDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(configDir, "app.json"), []byte(`{"key": "value"}`), 0644); err != nil {
		t.Fatal(err)
	}

	// Create a file that shouldn't be copied
	if err := os.WriteFile(filepath.Join(srcDir, "README.md"), []byte("# README"), 0644); err != nil {
		t.Fatal(err)
	}

	if err := CopyBotsterFiles(srcDir, destDir); err != nil {
		t.Fatal(err)
	}

	// Check .env was copied
	envPath := filepath.Join(destDir, ".env")
	if _, err := os.Stat(envPath); os.IsNotExist(err) {
		t.Error(".env should have been copied")
	}

	// Check config/app.json was copied
	appJsonPath := filepath.Join(destDir, "config", "app.json")
	if _, err := os.Stat(appJsonPath); os.IsNotExist(err) {
		t.Error("config/app.json should have been copied")
	}

	// Check README.md was NOT copied
	readmePath := filepath.Join(destDir, "README.md")
	if _, err := os.Stat(readmePath); err == nil {
		t.Error("README.md should NOT have been copied")
	}
}

func TestSanitizeRef(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{"owner/repo#123", "owner-repo-123"},
		{"owner/repo", "owner-repo"},
		{"simple", "simple"},
		{"with spaces", "with-spaces"},
	}

	for _, tt := range tests {
		got := sanitizeRef(tt.input)
		if got != tt.want {
			t.Errorf("sanitizeRef(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

func TestCleanupWorktreeNonExistent(t *testing.T) {
	tmpDir := t.TempDir()
	manager := New(tmpDir, nil)

	// Should not error on non-existent worktree
	err := manager.CleanupWorktree(tmpDir, filepath.Join(tmpDir, "nonexistent"))
	if err != nil {
		t.Errorf("CleanupWorktree should not error on non-existent path: %v", err)
	}
}

func TestDeleteWorktreeByPathOutsideManagedDir(t *testing.T) {
	tmpDir := t.TempDir()
	manager := New(filepath.Join(tmpDir, "managed"), nil)

	// Should error when path is outside managed directory
	err := manager.DeleteWorktreeByPath("/some/other/path", "botster-test")
	if err == nil {
		t.Error("DeleteWorktreeByPath should error for path outside managed directory")
	}
}

func TestDeleteWorktreeByPathMainRepo(t *testing.T) {
	tmpDir := t.TempDir()
	manager := New(tmpDir, nil)

	// Create a fake "main repo" (directory with .git directory)
	fakeRepo := filepath.Join(tmpDir, "repo")
	if err := os.MkdirAll(filepath.Join(fakeRepo, ".git"), 0755); err != nil {
		t.Fatal(err)
	}

	// Should refuse to delete main repo
	err := manager.DeleteWorktreeByPath(fakeRepo, "botster-test")
	if err == nil {
		t.Error("DeleteWorktreeByPath should refuse to delete main repository")
	}

	if _, ok := err.(error); ok {
		if err.Error() == "" {
			t.Error("Error should have message about refusing to delete main repo")
		}
	}
}

func TestCopyFilePreservesPermissions(t *testing.T) {
	tmpDir := t.TempDir()
	srcFile := filepath.Join(tmpDir, "src.sh")
	destFile := filepath.Join(tmpDir, "dest.sh")

	// Create executable file
	if err := os.WriteFile(srcFile, []byte("#!/bin/bash\necho hello"), 0755); err != nil {
		t.Fatal(err)
	}

	if err := copyFile(srcFile, destFile); err != nil {
		t.Fatal(err)
	}

	// Check permissions are preserved
	info, err := os.Stat(destFile)
	if err != nil {
		t.Fatal(err)
	}

	// Check it's executable (at least by owner)
	if info.Mode()&0100 == 0 {
		t.Error("Copied file should preserve executable permission")
	}
}

func TestRepoInfoStruct(t *testing.T) {
	info := &RepoInfo{
		Path: "/path/to/repo",
		Name: "owner/repo",
	}

	if info.Path != "/path/to/repo" {
		t.Errorf("Path = %q, want '/path/to/repo'", info.Path)
	}
	if info.Name != "owner/repo" {
		t.Errorf("Name = %q, want 'owner/repo'", info.Name)
	}
}

func TestWorktreeStruct(t *testing.T) {
	wt := &Worktree{
		Path:     "/path/to/worktree",
		Branch:   "botster-issue-42",
		AgentID:  "abc12345",
		IssueRef: "owner/repo#42",
	}

	if wt.Path != "/path/to/worktree" {
		t.Errorf("Path = %q", wt.Path)
	}
	if wt.Branch != "botster-issue-42" {
		t.Errorf("Branch = %q", wt.Branch)
	}
	if wt.AgentID != "abc12345" {
		t.Errorf("AgentID = %q", wt.AgentID)
	}
	if wt.IssueRef != "owner/repo#42" {
		t.Errorf("IssueRef = %q", wt.IssueRef)
	}
}

func TestClaudeSettingsStruct(t *testing.T) {
	settings := ClaudeSettings{
		AllowedDirectories: []string{"/path/to/worktree"},
		PermissionMode:     "acceptEdits",
	}

	// Test JSON marshaling
	data, err := json.Marshal(settings)
	if err != nil {
		t.Fatal(err)
	}

	var decoded ClaudeSettings
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatal(err)
	}

	if decoded.PermissionMode != "acceptEdits" {
		t.Errorf("PermissionMode = %q", decoded.PermissionMode)
	}
}

func TestEmptyBotsterFiles(t *testing.T) {
	tmpDir := t.TempDir()

	// Create empty .botster_init file
	if err := os.WriteFile(filepath.Join(tmpDir, ".botster_init"), []byte(""), 0644); err != nil {
		t.Fatal(err)
	}

	commands, err := ReadBotsterInitCommands(tmpDir)
	if err != nil {
		t.Fatal(err)
	}

	if len(commands) != 0 {
		t.Errorf("Empty file should return empty slice, got %v", commands)
	}
}

func TestBotsterCopySkipsGitDir(t *testing.T) {
	srcDir := t.TempDir()
	destDir := t.TempDir()

	// Create .botster_copy that matches files in subdirectories
	botsterCopy := filepath.Join(srcDir, ".botster_copy")
	if err := os.WriteFile(botsterCopy, []byte("src/**\n*.txt"), 0644); err != nil {
		t.Fatal(err)
	}

	// Create .git directory with files
	gitDir := filepath.Join(srcDir, ".git")
	if err := os.MkdirAll(gitDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(gitDir, "config"), []byte("git config"), 0644); err != nil {
		t.Fatal(err)
	}

	// Create a normal file at root
	if err := os.WriteFile(filepath.Join(srcDir, "file.txt"), []byte("content"), 0644); err != nil {
		t.Fatal(err)
	}

	// Create files in src directory
	srcSubDir := filepath.Join(srcDir, "src")
	if err := os.MkdirAll(srcSubDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(srcSubDir, "main.go"), []byte("package main"), 0644); err != nil {
		t.Fatal(err)
	}

	if err := CopyBotsterFiles(srcDir, destDir); err != nil {
		t.Fatal(err)
	}

	// .git directory should NOT be copied
	destGitDir := filepath.Join(destDir, ".git")
	if _, err := os.Stat(destGitDir); err == nil {
		t.Error(".git directory should NOT be copied")
	}

	// Root file should be copied (matches *.txt)
	destFile := filepath.Join(destDir, "file.txt")
	if _, err := os.Stat(destFile); os.IsNotExist(err) {
		t.Error("file.txt should have been copied")
	}

	// src/main.go should be copied
	destSrcFile := filepath.Join(destDir, "src", "main.go")
	if _, err := os.Stat(destSrcFile); os.IsNotExist(err) {
		t.Error("src/main.go should have been copied")
	}
}
