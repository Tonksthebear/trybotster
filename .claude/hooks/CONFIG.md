# Hooks Configuration Guide

This guide explains how to configure and customize the hooks system for Rails projects.

## Quick Start Configuration

### 1. Register Hooks in .claude/settings.json

Your `.claude/settings.json` is already configured with Rails-optimized hooks:

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
    ],
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
    ],
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

### 2. Dependencies

No npm/Node.js dependencies needed! All hooks are pure bash scripts.

**Optional:** Install RuboCop for linting:

```bash
# Via Bundler (recommended)
bundle add rubocop --group development

# Or globally
gem install rubocop
```

### 3. File Permissions

All hooks are already executable:

```bash
chmod +x .claude/hooks/*.sh
```

## Customization Options

### Rails Project Structure Detection

By default, hooks detect these Rails directory patterns:

**Rails App:** `app/controllers/`, `app/models/`, `app/views/`, `app/services/`
**Frontend:** `app/javascript/`, `app/assets/`
**Config:** `config/`, `lib/`
**Database:** `db/migrate/`, `db/schema.rb`

#### Adding Custom Directory Patterns

Edit `.claude/hooks/post-tool-use-tracker.sh`, function `detect_rails_component()`:

```bash
case "$file_path" in
    */app/custom_dir/*)
        echo "custom_component"
        ;;
    # ... existing patterns
esac
```

### RuboCop Configuration

The RuboCop hook uses your project's `.rubocop.yml` configuration.

#### Customizing RuboCop Behavior

Edit `.rubocop.yml` in your project root:

```yaml
AllCops:
  TargetRubyVersion: 3.2
  NewCops: enable
  Exclude:
    - "db/schema.rb"
    - "node_modules/**/*"
    - "vendor/**/*"

Style/Documentation:
  Enabled: false

Metrics/MethodLength:
  Max: 15
```

#### Customizing RuboCop Hook Behavior

Edit `.claude/hooks/rubocop-check.sh`:

```bash
# Change autocorrect behavior (line ~23)
if rubocop --autocorrect-all --display-only-failed; then

# Options:
# --autocorrect-all : Fix all safe violations
# --autocorrect     : Fix only safe violations (deprecated in newer RuboCop)
# --safe-autocorrect: Only safe fixes
# No flag           : Just report, don't fix
```

### Adding Rails Test Hooks

You can add a hook to run Rails tests:

**Create `.claude/hooks/rails-test.sh`:**

```bash
#!/bin/bash

set -e

cd "$CLAUDE_PROJECT_DIR" || exit 1

echo "🧪 Running Rails tests..."

# For Minitest (Rails default)
if bin/rails test; then
    echo "✅ Tests passed!"
    exit 0
else
    echo "❌ Tests failed"
    exit 1
fi

# For RSpec (if you use it)
# if bundle exec rspec; then
#     echo "✅ Tests passed!"
#     exit 0
# else
#     echo "❌ Tests failed"
#     exit 1
# fi
```

**Make executable:**

```bash
chmod +x .claude/hooks/rails-test.sh
```

**Add to settings.json:**

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/rubocop-check.sh"
          },
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/rails-test.sh"
          }
        ]
      }
    ]
  }
}
```

## Environment Variables

### Global Environment Variables

Set in your shell profile (`.bashrc`, `.zshrc`, etc.):

```bash
# Custom project directory (if not using default)
export CLAUDE_PROJECT_DIR=/path/to/your/rails/project

# Skip RuboCop checks
export SKIP_RUBOCOP=1
```

### Per-Session Environment Variables

Set before starting Claude Code:

```bash
SKIP_RUBOCOP=1 claude-code
```

## Hook Execution Order

Stop hooks run in the order specified in `settings.json`:

```json
"Stop": [
  {
    "hooks": [
      { "command": "...rubocop-check.sh" },  // Runs FIRST
      { "command": "...rails-test.sh" }      // Runs SECOND
    ]
  }
]
```

**Why this order matters:**

1. Fix code style first (RuboCop)
2. Then run tests (with clean code)

## Selective Hook Enabling

You don't need all hooks. Choose what works for your workflow:

### Minimal Setup (Skill Activation Only)

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

### Without RuboCop (No Linting)

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
    ],
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

## Cache Management

### Cache Location

```
$CLAUDE_PROJECT_DIR/.claude/cache/[session_id]/
```

### Manual Cache Cleanup

```bash
# Remove all cached data
rm -rf $CLAUDE_PROJECT_DIR/.claude/cache/*

# Remove specific session
rm -rf $CLAUDE_PROJECT_DIR/.claude/cache/[session-id]
```

## Troubleshooting Configuration

### Hook Not Executing

1. **Check registration:** Verify hook is in `.claude/settings.json`
2. **Check permissions:** Run `chmod +x .claude/hooks/*.sh`
3. **Check path:** Ensure `$CLAUDE_PROJECT_DIR` is set correctly
4. **Check dependencies:** For RuboCop hook, verify RuboCop is installed

### RuboCop Issues

**Issue:** RuboCop not found

**Solution:** Install RuboCop:

```bash
bundle add rubocop --group development
# or
gem install rubocop
```

**Issue:** Too many RuboCop violations

**Solution:** Configure `.rubocop.yml` to disable certain cops:

```yaml
Style/FrozenStringLiteralComment:
  Enabled: false
```

### Performance Issues

**Issue:** Hooks are slow

**Solutions:**

1. Configure RuboCop to exclude directories (add to `.rubocop.yml`):
   ```yaml
   AllCops:
     Exclude:
       - "db/schema.rb"
       - "node_modules/**/*"
       - "vendor/**/*"
   ```
2. Use `--autocorrect` instead of `--autocorrect-all` for faster runs

### Debugging Hooks

Add debug output to any hook:

```bash
# At the top of the hook script
set -x  # Enable debug mode

# Or add specific debug lines
echo "DEBUG: file_path=$file_path" >&2
echo "DEBUG: CLAUDE_PROJECT_DIR=$CLAUDE_PROJECT_DIR" >&2
```

View hook execution in Claude Code's logs.

## Advanced Configuration

### Custom Hook Event Handlers

You can create your own hooks for other events:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/my-custom-bash-guard.sh"
          }
        ]
      }
    ]
  }
}
```

### Rails Engine Projects

For Rails engines or modular monoliths:

```bash
# In post-tool-use-tracker.sh
case "$file_path" in
    */engines/*)
        engine_name=$(echo "$file_path" | sed 's|.*/engines/\([^/]*\)/.*|\1|')
        echo "engine:$engine_name"
        ;;
esac
```

### Docker/Container Projects

If your Rails app runs in containers:

```bash
# In rubocop-check.sh, replace the rubocop command
if docker-compose exec web rubocop --autocorrect-all --display-only-failed; then
    echo -e "${GREEN}✅ Rubocop checks passed!${NC}"
    exit 0
fi
```

## Best Practices

1. **Start minimal** - Enable hooks one at a time
2. **Test thoroughly** - Make changes and verify hooks work
3. **Document customizations** - Add comments to explain custom logic
4. **Version control** - Commit `.claude/` directory to git
5. **Team consistency** - Share configuration across team
6. **Configure RuboCop** - Use `.rubocop.yml` for project-specific rules

## Rails-Specific Best Practices

1. **Exclude generated files** - Add `db/schema.rb` to RuboCop excludes
2. **Use Bundler** - Add RuboCop to Gemfile for team consistency
3. **Configure for Rails** - Use `rubocop-rails` gem for Rails-specific cops
4. **Test hooks** - Verify hooks work with Rails structure (controllers, models, etc.)

## See Also

- [README.md](./README.md) - Hooks overview
- [../../CLAUDE_INTEGRATION.md](../../CLAUDE_INTEGRATION.md) - Complete integration guide
