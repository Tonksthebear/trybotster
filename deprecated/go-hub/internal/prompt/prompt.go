// Package prompt handles prompt loading for agent sessions.
//
// Prompts configure agent behavior in each worktree. They are loaded
// with the following priority:
//  1. Local .botster_prompt file in the worktree
//  2. Remote prompt fetched from GitHub
package prompt

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"time"
)

const (
	// DefaultPromptRepo is the GitHub repo to fetch default prompts from.
	DefaultPromptRepo = "Tonksthebear/trybotster"
	// DefaultPromptPath is the path within the repo for the prompt file.
	DefaultPromptPath = "cli/botster_prompt"
	// LocalPromptFile is the filename for local prompts in a worktree.
	LocalPromptFile = ".botster_prompt"
)

// Manager handles prompt loading for agent sessions.
type Manager struct {
	httpClient *http.Client
}

// NewManager creates a new prompt manager.
func NewManager() *Manager {
	return &Manager{
		httpClient: &http.Client{
			Timeout: 10 * time.Second,
		},
	}
}

// GetPrompt gets the prompt for a worktree.
// Priority: local .botster_prompt > remote cli/botster_prompt.*
func (m *Manager) GetPrompt(worktreePath string) (string, error) {
	// 1. Check for local .botster_prompt file
	localPath := filepath.Join(worktreePath, LocalPromptFile)
	if content, err := os.ReadFile(localPath); err == nil {
		return string(content), nil
	}

	// 2. Fetch from GitHub
	return m.fetchDefaultPrompt()
}

// GetPromptWithFallback gets prompt with a fallback if remote fails.
func (m *Manager) GetPromptWithFallback(worktreePath, fallback string) string {
	prompt, err := m.GetPrompt(worktreePath)
	if err != nil {
		return fallback
	}
	return prompt
}

// fetchDefaultPrompt fetches the default prompt from GitHub.
func (m *Manager) fetchDefaultPrompt() (string, error) {
	// Try common extensions in order
	extensions := []string{"md", "txt", ""}

	for _, ext := range extensions {
		filename := DefaultPromptPath
		if ext != "" {
			filename = fmt.Sprintf("%s.%s", DefaultPromptPath, ext)
		}

		url := fmt.Sprintf(
			"https://raw.githubusercontent.com/%s/main/%s",
			DefaultPromptRepo,
			filename,
		)

		resp, err := m.httpClient.Get(url)
		if err != nil {
			continue
		}
		defer resp.Body.Close()

		if resp.StatusCode == http.StatusOK {
			body, err := io.ReadAll(resp.Body)
			if err != nil {
				continue
			}
			return string(body), nil
		}
	}

	return "", fmt.Errorf(
		"could not find prompt file at %s. Tried extensions: %v",
		DefaultPromptPath,
		extensions,
	)
}

// GetLocalPrompt reads only the local prompt file if it exists.
// Returns empty string and nil if file doesn't exist.
func GetLocalPrompt(worktreePath string) (string, error) {
	localPath := filepath.Join(worktreePath, LocalPromptFile)
	content, err := os.ReadFile(localPath)
	if os.IsNotExist(err) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("failed to read local prompt: %w", err)
	}
	return string(content), nil
}

// WriteLocalPrompt writes a prompt to the local .botster_prompt file.
func WriteLocalPrompt(worktreePath, content string) error {
	localPath := filepath.Join(worktreePath, LocalPromptFile)
	if err := os.WriteFile(localPath, []byte(content), 0644); err != nil {
		return fmt.Errorf("failed to write local prompt: %w", err)
	}
	return nil
}

// HasLocalPrompt checks if a local prompt file exists in the worktree.
func HasLocalPrompt(worktreePath string) bool {
	localPath := filepath.Join(worktreePath, LocalPromptFile)
	_, err := os.Stat(localPath)
	return err == nil
}
