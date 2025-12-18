# Git Hooks

This directory contains shared git hooks for the project.

## Setup

To enable these hooks, run:

```bash
git config core.hooksPath .githooks
```

Or run the setup script:

```bash
bin/setup-hooks
```

## Available Hooks

### pre-push

Runs before each `git push`. Executes:

1. **Rubocop** - Ruby style and lint checks
2. **Brakeman** - Security vulnerability scanner

If any check fails, the push is blocked. To bypass (not recommended):

```bash
git push --no-verify
```

## Adding New Hooks

1. Create a new file in `.githooks/` named after the hook (e.g., `pre-commit`)
2. Make it executable: `chmod +x .githooks/<hook-name>`
3. The hook should exit with code 0 on success, non-zero on failure
