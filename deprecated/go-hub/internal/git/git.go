// Package git provides git worktree management for agent isolation.
//
// Each agent works in its own git worktree to prevent conflicts.
// Worktrees are created when an agent starts and cleaned up when it finishes.
//
// This package supports three special dotfiles for customizing agent environments:
//   - .botster_copy: Glob patterns for files to copy to the worktree
//   - .botster_init: Shell commands to run after worktree creation
//   - .botster_teardown: Shell commands to run before worktree deletion
package git

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/gobwas/glob"
)

// Manager handles git worktree operations.
type Manager struct {
	baseDir string // Directory for storing worktrees
	logger  *slog.Logger
}

// New creates a new git manager.
func New(baseDir string, logger *slog.Logger) *Manager {
	if logger == nil {
		logger = slog.Default()
	}
	return &Manager{
		baseDir: baseDir,
		logger:  logger,
	}
}

// Worktree represents a git worktree.
type Worktree struct {
	Path      string // Absolute path to the worktree
	Branch    string // Branch name
	AgentID   string // Agent ID that owns this worktree
	IssueRef  string // Issue reference (e.g., "owner/repo#123")
}

// RepoInfo contains information about a git repository.
type RepoInfo struct {
	Path string // Absolute path to the repository root
	Name string // Repository name in "owner/repo" format
}

// DetectCurrentRepo finds the git repository in the current directory.
// Returns the repo path and name (from origin remote or directory name).
func DetectCurrentRepo() (*RepoInfo, error) {
	cwd, err := os.Getwd()
	if err != nil {
		return nil, fmt.Errorf("getting current directory: %w", err)
	}

	// Find git repository root
	cmd := exec.Command("git", "rev-parse", "--show-toplevel")
	cmd.Dir = cwd
	output, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("not in a git repository: %w", err)
	}

	repoPath := strings.TrimSpace(string(output))

	// Get repository name from origin remote
	cmd = exec.Command("git", "remote", "get-url", "origin")
	cmd.Dir = repoPath
	output, err = cmd.Output()

	var repoName string
	if err == nil {
		url := strings.TrimSpace(string(output))
		repoName = extractRepoName(url)
	}

	// Fallback to directory name if no remote
	if repoName == "" {
		repoName = filepath.Base(repoPath)
	}

	return &RepoInfo{
		Path: repoPath,
		Name: repoName,
	}, nil
}

// extractRepoName extracts "owner/repo" from a git URL.
// Supports HTTPS and SSH URLs.
func extractRepoName(url string) string {
	// Remove .git suffix
	url = strings.TrimSuffix(url, ".git")

	// Handle HTTPS URLs: https://github.com/owner/repo
	if strings.HasPrefix(url, "https://") || strings.HasPrefix(url, "http://") {
		parts := strings.Split(url, "/")
		if len(parts) >= 2 {
			return parts[len(parts)-2] + "/" + parts[len(parts)-1]
		}
	}

	// Handle SSH URLs: git@github.com:owner/repo
	if strings.Contains(url, ":") {
		parts := strings.Split(url, ":")
		if len(parts) >= 2 {
			return parts[len(parts)-1]
		}
	}

	return ""
}

// ReadBotsterCopyPatterns reads the .botster_copy file and returns glob patterns.
// Lines starting with # are comments, empty lines are skipped.
func ReadBotsterCopyPatterns(repoPath string) ([]string, error) {
	path := filepath.Join(repoPath, ".botster_copy")

	file, err := os.Open(path)
	if os.IsNotExist(err) {
		return nil, nil // No patterns file
	}
	if err != nil {
		return nil, fmt.Errorf("opening .botster_copy: %w", err)
	}
	defer file.Close()

	var patterns []string
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line != "" && !strings.HasPrefix(line, "#") {
			patterns = append(patterns, line)
		}
	}

	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading .botster_copy: %w", err)
	}

	return patterns, nil
}

// ReadBotsterInitCommands reads the .botster_init file and returns shell commands.
// These commands are run after worktree creation.
func ReadBotsterInitCommands(repoPath string) ([]string, error) {
	return readCommandsFile(filepath.Join(repoPath, ".botster_init"))
}

// ReadBotsterTeardownCommands reads the .botster_teardown file and returns shell commands.
// These commands are run before worktree deletion.
func ReadBotsterTeardownCommands(repoPath string) ([]string, error) {
	return readCommandsFile(filepath.Join(repoPath, ".botster_teardown"))
}

func readCommandsFile(path string) ([]string, error) {
	file, err := os.Open(path)
	if os.IsNotExist(err) {
		return nil, nil // No commands file
	}
	if err != nil {
		return nil, fmt.Errorf("opening %s: %w", filepath.Base(path), err)
	}
	defer file.Close()

	var commands []string
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line != "" && !strings.HasPrefix(line, "#") {
			commands = append(commands, line)
		}
	}

	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading %s: %w", filepath.Base(path), err)
	}

	return commands, nil
}

// CopyBotsterFiles copies files matching .botster_copy patterns from source to dest.
func CopyBotsterFiles(sourceRepo, destWorktree string) error {
	patterns, err := ReadBotsterCopyPatterns(sourceRepo)
	if err != nil {
		return err
	}

	if len(patterns) == 0 {
		return nil // Nothing to copy
	}

	// Compile glob patterns
	var globs []glob.Glob
	for _, pattern := range patterns {
		g, err := glob.Compile(pattern, '/')
		if err != nil {
			// Log warning but continue with other patterns
			slog.Warn("Invalid glob pattern in .botster_copy", "pattern", pattern, "error", err)
			continue
		}
		globs = append(globs, g)
	}

	if len(globs) == 0 {
		return nil // No valid patterns
	}

	// Walk source repo and copy matching files
	return filepath.Walk(sourceRepo, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil // Continue on error
		}

		// Skip .git directory
		if info.IsDir() && info.Name() == ".git" {
			return filepath.SkipDir
		}

		// Skip directories
		if info.IsDir() {
			return nil
		}

		// Get relative path from source root
		relPath, err := filepath.Rel(sourceRepo, path)
		if err != nil {
			return nil // Continue on error
		}

		// Check if any glob matches
		for _, g := range globs {
			if g.Match(relPath) {
				// Copy file
				destPath := filepath.Join(destWorktree, relPath)

				// Create parent directories
				if err := os.MkdirAll(filepath.Dir(destPath), 0755); err != nil {
					slog.Warn("Failed to create directory", "path", filepath.Dir(destPath), "error", err)
					continue
				}

				// Copy file
				if err := copyFile(path, destPath); err != nil {
					slog.Warn("Failed to copy file", "src", path, "dest", destPath, "error", err)
				} else {
					slog.Info("Copied file", "src", relPath, "dest", destPath)
				}
				break // Only copy once even if multiple patterns match
			}
		}

		return nil
	})
}

func copyFile(src, dest string) error {
	data, err := os.ReadFile(src)
	if err != nil {
		return err
	}

	// Preserve file permissions
	info, err := os.Stat(src)
	if err != nil {
		return err
	}

	return os.WriteFile(dest, data, info.Mode())
}

// ClaudeSettings represents Claude pre-authorization settings.
type ClaudeSettings struct {
	AllowedDirectories []string `json:"allowedDirectories"`
	PermissionMode     string   `json:"permissionMode"`
}

// WriteClaudeSettings creates the .claude/settings.local.json file for pre-authorization.
func WriteClaudeSettings(worktreePath string) error {
	claudeDir := filepath.Join(worktreePath, ".claude")
	if err := os.MkdirAll(claudeDir, 0755); err != nil {
		return fmt.Errorf("creating .claude directory: %w", err)
	}

	settings := ClaudeSettings{
		AllowedDirectories: []string{worktreePath},
		PermissionMode:     "acceptEdits",
	}

	data, err := json.MarshalIndent(settings, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling settings: %w", err)
	}

	settingsPath := filepath.Join(claudeDir, "settings.local.json")
	if err := os.WriteFile(settingsPath, data, 0644); err != nil {
		return fmt.Errorf("writing settings: %w", err)
	}

	return nil
}

// CreateWorktreeFromCurrent creates a worktree for an issue from the current repository.
func (m *Manager) CreateWorktreeFromCurrent(issueNumber int) (string, error) {
	branchName := fmt.Sprintf("botster-issue-%d", issueNumber)
	return m.CreateWorktreeWithBranch(branchName)
}

// CreateWorktreeWithBranch creates a worktree with the specified branch name.
func (m *Manager) CreateWorktreeWithBranch(branchName string) (string, error) {
	repoInfo, err := DetectCurrentRepo()
	if err != nil {
		return "", err
	}

	// Sanitize names for path
	repoSafe := strings.ReplaceAll(repoInfo.Name, "/", "-")
	branchSafe := strings.ReplaceAll(branchName, "/", "-")
	worktreePath := filepath.Join(m.baseDir, fmt.Sprintf("%s-%s", repoSafe, branchSafe))

	// Ensure base directory exists
	if err := os.MkdirAll(m.baseDir, 0755); err != nil {
		return "", fmt.Errorf("creating base directory: %w", err)
	}

	// Remove existing worktree if present
	if err := m.CleanupWorktree(repoInfo.Path, worktreePath); err != nil {
		m.logger.Warn("Failed to cleanup existing worktree", "error", err)
	}

	// Check if branch exists
	branchExists := m.branchExists(repoInfo.Path, branchName)

	// Create worktree
	var cmd *exec.Cmd
	if branchExists {
		m.logger.Info("Using existing branch", "branch", branchName)
		cmd = exec.Command("git", "worktree", "add", worktreePath, branchName)
	} else {
		m.logger.Info("Creating new branch", "branch", branchName)
		cmd = exec.Command("git", "worktree", "add", "-b", branchName, worktreePath)
	}
	cmd.Dir = repoInfo.Path

	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("creating worktree: %s (%w)", string(output), err)
	}

	// Write Claude pre-authorization settings
	if err := WriteClaudeSettings(worktreePath); err != nil {
		m.logger.Warn("Failed to write Claude settings", "error", err)
	}

	// Copy files matching .botster_copy patterns
	if err := CopyBotsterFiles(repoInfo.Path, worktreePath); err != nil {
		m.logger.Warn("Failed to copy botster files", "error", err)
	}

	m.logger.Info("Created worktree",
		"path", worktreePath,
		"branch", branchName,
		"repo", repoInfo.Name,
	)

	return worktreePath, nil
}

func (m *Manager) branchExists(repoPath, branchName string) bool {
	cmd := exec.Command("git", "show-ref", "--verify", "--quiet", "refs/heads/"+branchName)
	cmd.Dir = repoPath
	return cmd.Run() == nil
}

// CleanupWorktree removes an existing worktree.
func (m *Manager) CleanupWorktree(repoPath, worktreePath string) error {
	if _, err := os.Stat(worktreePath); os.IsNotExist(err) {
		return nil // Nothing to clean up
	}

	m.logger.Info("Removing existing worktree", "path", worktreePath)

	// Try git worktree remove first
	cmd := exec.Command("git", "worktree", "remove", worktreePath, "--force")
	cmd.Dir = repoPath
	if err := cmd.Run(); err != nil {
		m.logger.Warn("git worktree remove failed, trying prune", "error", err)

		// Try prune
		cmd = exec.Command("git", "worktree", "prune")
		cmd.Dir = repoPath
		_ = cmd.Run()

		// Remove directory manually if it still exists
		if _, err := os.Stat(worktreePath); err == nil {
			if err := os.RemoveAll(worktreePath); err != nil {
				return fmt.Errorf("removing worktree directory: %w", err)
			}
		}
	}

	return nil
}

// DeleteWorktreeByPath deletes a worktree with safety checks and teardown scripts.
func (m *Manager) DeleteWorktreeByPath(worktreePath, branchName string) error {
	// SAFETY CHECK 1: Verify path is within managed base directory
	absWorktree, err := filepath.Abs(worktreePath)
	if err != nil {
		return fmt.Errorf("getting absolute path: %w", err)
	}
	absBase, err := filepath.Abs(m.baseDir)
	if err != nil {
		absBase = m.baseDir
	}

	if !strings.HasPrefix(absWorktree, absBase) {
		return fmt.Errorf("worktree path %s is outside managed directory %s", worktreePath, m.baseDir)
	}

	// SAFETY CHECK 2: Warn if branch doesn't follow convention
	if !strings.HasPrefix(branchName, "botster-") {
		m.logger.Warn("Branch name doesn't follow botster convention",
			"branch", branchName,
			"expected_prefix", "botster-",
		)
	}

	// SAFETY CHECK 3: Check for Claude settings marker file
	markerFile := filepath.Join(worktreePath, ".claude", "settings.local.json")
	if _, err := os.Stat(markerFile); os.IsNotExist(err) {
		m.logger.Warn("Missing botster marker file - may not be a managed worktree",
			"expected", markerFile,
		)
	}

	// SAFETY CHECK 4: Verify this is a worktree, not the main repo
	gitFile := filepath.Join(worktreePath, ".git")
	info, err := os.Stat(gitFile)
	if err != nil {
		return fmt.Errorf("checking .git: %w", err)
	}

	// A worktree has a .git *file*, not a .git *directory*
	if info.IsDir() {
		return fmt.Errorf("refusing to delete main repository at %s - this is not a worktree", worktreePath)
	}

	// Find the main repository from the worktree
	repoPath, err := m.findMainRepoFromWorktree(worktreePath)
	if err != nil {
		return fmt.Errorf("finding main repo: %w", err)
	}

	// Get repo name for teardown environment variables
	repoInfo, err := DetectCurrentRepo()
	repoName := ""
	if err == nil {
		repoName = repoInfo.Name
	}

	// Check if worktree exists
	if _, err := os.Stat(worktreePath); os.IsNotExist(err) {
		m.logger.Warn("Worktree does not exist", "path", worktreePath)
		return nil
	}

	m.logger.Info("Deleting worktree", "path", worktreePath)

	// Run teardown commands
	teardownCommands, err := ReadBotsterTeardownCommands(repoPath)
	if err != nil {
		m.logger.Warn("Failed to read teardown commands", "error", err)
	}

	if len(teardownCommands) > 0 {
		m.logger.Info("Running teardown commands", "count", len(teardownCommands))

		// Parse issue number from branch name
		issueNumber := 0
		if strings.HasPrefix(branchName, "botster-issue-") {
			fmt.Sscanf(branchName, "botster-issue-%d", &issueNumber)
		}

		for _, cmdStr := range teardownCommands {
			m.logger.Info("Running teardown", "command", cmdStr)

			cmd := exec.Command("sh", "-c", cmdStr)
			cmd.Dir = worktreePath
			cmd.Env = append(os.Environ(),
				fmt.Sprintf("BOTSTER_REPO=%s", repoName),
				fmt.Sprintf("BOTSTER_ISSUE_NUMBER=%d", issueNumber),
				fmt.Sprintf("BOTSTER_BRANCH_NAME=%s", branchName),
				fmt.Sprintf("BOTSTER_WORKTREE_PATH=%s", worktreePath),
			)

			output, err := cmd.CombinedOutput()
			if err != nil {
				m.logger.Warn("Teardown command failed",
					"command", cmdStr,
					"error", err,
					"output", string(output),
				)
			} else {
				m.logger.Debug("Teardown output", "output", string(output))
			}
		}
	}

	// Remove the worktree using git
	cmd := exec.Command("git", "worktree", "remove", worktreePath, "--force")
	cmd.Dir = repoPath
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("removing worktree: %s (%w)", string(output), err)
	}

	// Delete the branch
	m.logger.Info("Deleting branch", "branch", branchName)
	cmd = exec.Command("git", "branch", "-D", branchName)
	cmd.Dir = repoPath
	if output, err := cmd.CombinedOutput(); err != nil {
		m.logger.Warn("Failed to delete branch",
			"branch", branchName,
			"error", err,
			"output", string(output),
		)
	}

	m.logger.Info("Successfully deleted worktree", "path", worktreePath)
	return nil
}

// DeleteWorktreeByIssueNumber deletes a worktree for the specified issue.
func (m *Manager) DeleteWorktreeByIssueNumber(issueNumber int) error {
	repoInfo, err := DetectCurrentRepo()
	if err != nil {
		return err
	}

	repoSafe := strings.ReplaceAll(repoInfo.Name, "/", "-")
	branchName := fmt.Sprintf("botster-issue-%d", issueNumber)
	worktreePath := filepath.Join(m.baseDir, fmt.Sprintf("%s-%d", repoSafe, issueNumber))

	return m.DeleteWorktreeByPath(worktreePath, branchName)
}

func (m *Manager) findMainRepoFromWorktree(worktreePath string) (string, error) {
	// Read the .git file which contains the path to the main repo
	gitFile := filepath.Join(worktreePath, ".git")
	data, err := os.ReadFile(gitFile)
	if err != nil {
		return "", fmt.Errorf("reading .git file: %w", err)
	}

	// .git file contains: gitdir: /path/to/main/.git/worktrees/<name>
	content := strings.TrimSpace(string(data))
	if !strings.HasPrefix(content, "gitdir: ") {
		return "", fmt.Errorf("unexpected .git file format: %s", content)
	}

	gitDir := strings.TrimPrefix(content, "gitdir: ")

	// Navigate from .git/worktrees/<name> to .git to repo root
	// gitDir = /path/to/main/.git/worktrees/<name>
	// We want: /path/to/main
	mainGitDir := filepath.Dir(filepath.Dir(gitDir)) // .git
	repoPath := filepath.Dir(mainGitDir)              // repo root

	return repoPath, nil
}

// FindExistingWorktreeForIssue checks if a worktree already exists for the given issue.
func (m *Manager) FindExistingWorktreeForIssue(issueNumber int) (string, string, bool) {
	repoInfo, err := DetectCurrentRepo()
	if err != nil {
		return "", "", false
	}

	repoSafe := strings.ReplaceAll(repoInfo.Name, "/", "-")
	branchName := fmt.Sprintf("botster-issue-%d", issueNumber)
	worktreePath := filepath.Join(m.baseDir, fmt.Sprintf("%s-%s", repoSafe, branchName))

	// Check if directory exists
	if _, err := os.Stat(worktreePath); os.IsNotExist(err) {
		return "", "", false
	}

	// Check if .git file exists (worktree marker)
	gitFile := filepath.Join(worktreePath, ".git")
	if _, err := os.Stat(gitFile); os.IsNotExist(err) {
		return "", "", false
	}

	// Verify with git
	cmd := exec.Command("git", "worktree", "list", "--porcelain")
	cmd.Dir = repoInfo.Path
	output, err := cmd.Output()
	if err != nil {
		return "", "", false
	}

	// Check if our worktree is in the list
	for _, line := range strings.Split(string(output), "\n") {
		if strings.HasPrefix(line, "worktree ") {
			path := strings.TrimPrefix(line, "worktree ")
			if path == worktreePath {
				m.logger.Info("Found existing worktree",
					"issue", issueNumber,
					"path", worktreePath,
				)
				return worktreePath, branchName, true
			}
		}
	}

	return "", "", false
}

// ListWorktrees lists all worktrees managed by botster (with botster- prefix).
func (m *Manager) ListWorktrees() ([]*Worktree, error) {
	all, err := m.ListAllWorktrees()
	if err != nil {
		return nil, err
	}

	// Filter to only botster- prefixed branches
	var worktrees []*Worktree
	for _, wt := range all {
		if strings.HasPrefix(wt.Branch, "botster-") {
			worktrees = append(worktrees, wt)
		}
	}
	return worktrees, nil
}

// ListAllWorktrees lists ALL worktrees (not just botster- ones).
// Used by TUI for worktree selection where user can pick any existing worktree.
func (m *Manager) ListAllWorktrees() ([]*Worktree, error) {
	repoInfo, err := DetectCurrentRepo()
	if err != nil {
		return nil, err
	}

	cmd := exec.Command("git", "worktree", "list", "--porcelain")
	cmd.Dir = repoInfo.Path
	output, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("listing worktrees: %w", err)
	}

	var worktrees []*Worktree
	lines := strings.Split(string(output), "\n")

	var currentPath, currentBranch string
	for _, line := range lines {
		if strings.HasPrefix(line, "worktree ") {
			currentPath = strings.TrimPrefix(line, "worktree ")
		} else if strings.HasPrefix(line, "branch ") {
			currentBranch = strings.TrimPrefix(line, "branch refs/heads/")
		} else if line == "" && currentPath != "" {
			// Skip the main repo (where path == repoInfo.Path)
			if currentPath != repoInfo.Path {
				worktrees = append(worktrees, &Worktree{
					Path:   currentPath,
					Branch: currentBranch,
				})
			}
			currentPath = ""
			currentBranch = ""
		}
	}

	return worktrees, nil
}

// PruneStaleworktrees prunes worktrees that no longer exist.
func (m *Manager) PruneStaleWorktrees() error {
	repoInfo, err := DetectCurrentRepo()
	if err != nil {
		return err
	}

	cmd := exec.Command("git", "worktree", "prune")
	cmd.Dir = repoInfo.Path
	return cmd.Run()
}

// --- Legacy methods for backwards compatibility ---

// CreateWorktree creates a new worktree for an agent (legacy API).
func (m *Manager) CreateWorktree(agentID, issueRef, baseBranch string) (*Worktree, error) {
	// Sanitize issue ref for use in path/branch name
	sanitized := sanitizeRef(issueRef)
	branchName := fmt.Sprintf("botster/%s/%s", sanitized, agentID[:8])

	// Worktree path: baseDir/../.botster-worktrees/<sanitized>-<agentID[:8]>
	worktreesDir := filepath.Join(filepath.Dir(m.baseDir), ".botster-worktrees")
	if err := os.MkdirAll(worktreesDir, 0755); err != nil {
		return nil, fmt.Errorf("creating worktrees directory: %w", err)
	}

	worktreePath := filepath.Join(worktreesDir, fmt.Sprintf("%s-%s", sanitized, agentID[:8]))

	m.logger.Info("Creating worktree",
		"path", worktreePath,
		"branch", branchName,
		"base", baseBranch,
	)

	// Fetch latest from remote first
	if err := m.runGit("fetch", "origin", baseBranch); err != nil {
		m.logger.Warn("Failed to fetch, continuing anyway", "error", err)
	}

	// Create the worktree with a new branch
	if err := m.runGit("worktree", "add", "-b", branchName, worktreePath, "origin/"+baseBranch); err != nil {
		return nil, fmt.Errorf("creating worktree: %w", err)
	}

	return &Worktree{
		Path:     worktreePath,
		Branch:   branchName,
		AgentID:  agentID,
		IssueRef: issueRef,
	}, nil
}

// RemoveWorktree removes a worktree and its branch (legacy API).
func (m *Manager) RemoveWorktree(wt *Worktree) error {
	m.logger.Info("Removing worktree",
		"path", wt.Path,
		"branch", wt.Branch,
	)

	// Remove the worktree
	if err := m.runGit("worktree", "remove", "--force", wt.Path); err != nil {
		// Try to remove manually if git command fails
		if err := os.RemoveAll(wt.Path); err != nil {
			return fmt.Errorf("removing worktree directory: %w", err)
		}
		// Prune worktrees
		_ = m.runGit("worktree", "prune")
	}

	// Delete the branch
	if err := m.runGit("branch", "-D", wt.Branch); err != nil {
		m.logger.Warn("Failed to delete branch, may not exist", "branch", wt.Branch, "error", err)
	}

	return nil
}

// CleanupOrphaned removes worktrees that don't have an active agent.
func (m *Manager) CleanupOrphaned(activeAgentIDs map[string]bool) error {
	worktrees, err := m.ListWorktrees()
	if err != nil {
		return err
	}

	for _, wt := range worktrees {
		// Extract agent ID from branch name (last 8 chars after final /)
		parts := strings.Split(wt.Branch, "/")
		if len(parts) < 3 {
			continue
		}
		agentIDPrefix := parts[len(parts)-1]

		// Check if any active agent ID starts with this prefix
		found := false
		for id := range activeAgentIDs {
			if strings.HasPrefix(id, agentIDPrefix) {
				found = true
				break
			}
		}

		if !found {
			m.logger.Info("Removing orphaned worktree", "branch", wt.Branch)
			_ = m.RemoveWorktree(wt)
		}
	}

	return nil
}

func (m *Manager) runGit(args ...string) error {
	cmd := exec.Command("git", args...)
	cmd.Dir = m.baseDir
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("git %s: %s (%w)", strings.Join(args, " "), string(output), err)
	}
	return nil
}

// sanitizeRef converts an issue ref like "owner/repo#123" to "owner-repo-123"
func sanitizeRef(ref string) string {
	ref = strings.ReplaceAll(ref, "/", "-")
	ref = strings.ReplaceAll(ref, "#", "-")
	ref = strings.ReplaceAll(ref, " ", "-")
	return ref
}
