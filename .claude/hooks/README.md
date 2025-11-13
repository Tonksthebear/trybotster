# Hooks

Claude Code hooks that enable skill auto-activation, file tracking, and Rails validation.

---

## What Are Hooks?

Hooks are scripts that run at specific points in Claude's workflow:

- **UserPromptSubmit**: When user submits a prompt
- **PreToolUse**: Before a tool executes
- **PostToolUse**: After a tool completes
- **Stop**: When user requests to stop

**Key insight:** Hooks can modify prompts, block actions, and track state - enabling features Claude can't do alone.

---

## Active Hooks in This Project

### skill-activation-prompt (UserPromptSubmit)

**Purpose:** Automatically suggests relevant skills based on user prompts and file context

**How it works:**

1. Reads `skill-rules.json`
2. Matches user prompt against trigger patterns
3. Checks which files user is working with
4. Injects skill suggestions into Claude's context

**Why it's essential:** This is THE hook that makes skills auto-activate.

**Status:** ✅ Configured and executable

**Configuration in settings.json:**

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/skill-activation-prompt.sh"
          }
        ]
      }
    ]
  }
}
```

**Customization:** ✅ None needed - reads skill-rules.json automatically

---

### post-tool-use-tracker (PostToolUse)

**Purpose:** Tracks file changes to maintain context across sessions

**How it works:**

1. Monitors Edit/Write/MultiEdit tool calls
2. Records which files were modified
3. Creates cache for context management
4. Auto-detects Rails project structure (controllers, models, views, etc.)

**Why it's essential:** Helps Claude understand what parts of your Rails codebase are active.

**Status:** ✅ Configured and executable

**Configuration in settings.json:**

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|MultiEdit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/post-tool-use-tracker.sh"
          }
        ]
      }
    ]
  }
}
```

**Customization:** ✅ None needed - auto-detects Rails structure

---

### rubocop-check (Stop)

**Purpose:** Runs RuboCop linting and auto-correction when user stops

**How it works:**

1. Runs when user stops Claude Code session
2. Executes `rubocop --autocorrect-all --display-only-failed`
3. Auto-fixes safe Ruby style violations
4. Reports any remaining issues

**Why it's useful:** Keeps your Ruby code clean and following Rails best practices automatically.

**Status:** ✅ Configured and executable

**Configuration in settings.json:**

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/rubocop-check.sh"
          }
        ]
      }
    ]
  }
}
```

**Requirements:**

- RuboCop must be installed (`gem install rubocop` or add to Gemfile)
- Optional: `.rubocop.yml` for custom rules

**Customization:** ✅ Configure via `.rubocop.yml` in project root

---

## File Permissions

All hooks are executable:

```bash
-rwxr-xr-x  post-tool-use-tracker.sh
-rwxr-xr-x  rubocop-check.sh
-rwxr-xr-x  skill-activation-prompt.sh
```

To verify:

```bash
ls -la .claude/hooks/*.sh | grep rwx
```

---

## Maintenance

### Adding New Hooks

1. Create script in `.claude/hooks/`
2. Make executable: `chmod +x .claude/hooks/your-hook.sh`
3. Register in `.claude/settings.json`
4. Test by triggering the event

### Removing Hooks

1. Delete script from `.claude/hooks/`
2. Remove from `.claude/settings.json`

### Testing Hooks

**UserPromptSubmit:**

```bash
# Submit any prompt to Claude
```

**PostToolUse:**

```bash
# Edit any file using Claude
```

**Stop:**

```bash
# Request Claude to stop
```

---

## Rails-Specific Notes

This hooks configuration is optimized for Rails projects:

- **No TypeScript/Node.js dependencies** - Pure bash scripts
- **RuboCop integration** - Automatic Ruby linting
- **Rails structure detection** - Understands `app/`, `config/`, `db/` directories
- **Compatible with Hotwire** - Works with Stimulus controllers and Turbo

---

## For Claude Code

**When working in this project:**

1. ✅ All hooks are Rails-optimized
2. ✅ No Node.js/npm dependencies required
3. ✅ RuboCop runs automatically on Stop events
4. ✅ Skills auto-activate for Rails files

**Questions?** See [CLAUDE_INTEGRATION.md](../../CLAUDE_INTEGRATION.md)
