# Skills

Rails-focused skills for Claude Code that auto-activate based on context.

---

## What Are Skills?

Skills are modular knowledge bases that Claude loads when needed. They provide:

- Domain-specific guidelines
- Best practices
- Code examples
- Anti-patterns to avoid

**Problem:** Skills don't activate automatically by default.

**Solution:** This project includes the hooks + configuration to make them activate for Rails development.

---

## Active Skills in This Project

### skill-developer (Meta-Skill)

**Purpose:** Creating and managing Claude Code skills

**Files:** 7 resource files (426 lines total)

**Use when:**

- Creating new skills
- Understanding skill structure
- Working with skill-rules.json
- Debugging skill activation

**Customization:** ✅ None - works as-is

**Status:** ✅ Installed and configured

**[View Skill →](skill-developer/)**

---

### rails-backend-guidelines

**Purpose:** Rails backend development patterns and best practices

**Files:** 11 resource files covering comprehensive Rails patterns

**Covers:**

- Rails MVC architecture
- Controllers and routing (RESTful conventions)
- ActiveRecord models (associations, validations, scopes)
- Service objects and business logic
- Database patterns and migrations
- Background jobs (Solid Queue)
- Action Cable (WebSockets)
- Error handling and Sentry integration
- Testing with Minitest/RSpec
- Rails conventions and best practices

**Use when:**

- Creating/modifying controllers
- Building models and migrations
- Writing service objects
- Database operations with ActiveRecord
- Setting up background jobs
- Implementing WebSocket features

**Auto-activates on:**

- Editing files in `app/controllers/`, `app/models/`, `app/services/`
- Working with `db/migrate/`, `config/routes.rb`
- Keywords: "controller", "model", "service", "migration", "ActiveRecord"

**Status:** ✅ Installed and configured

**[View Skill →](rails-backend-guidelines/)**

---

### rails-frontend-guidelines

**Purpose:** Rails frontend with Hotwire (Turbo + Stimulus) and Tailwind CSS

**Files:** 12 resource files covering modern Rails frontend patterns

**Covers:**

- Hotwire (Turbo Frames, Turbo Streams)
- Stimulus controllers and actions
- Server-rendered HTML with progressive enhancement
- Tailwind CSS styling patterns
- ViewComponent architecture
- ERB templates and partials
- Data attributes and targets
- Performance optimization
- Loading and error states
- File organization

**Use when:**

- Creating views and partials
- Building Stimulus controllers
- Using Turbo Frames/Streams
- Styling with Tailwind CSS
- Implementing ViewComponents
- Frontend JavaScript interactions

**Auto-activates on:**

- Editing files in `app/views/`, `app/javascript/controllers/`
- Working with `.html.erb`, `.turbo_stream.erb` files
- Keywords: "view", "Turbo", "Stimulus", "Hotwire", "Tailwind"

**Status:** ✅ Installed and configured

**[View Skill →](rails-frontend-guidelines/)**

---

## How Skills Auto-Activate

Skills activate automatically when:

1. **File-based triggers:**
   - You edit a file in `app/controllers/` → rails-backend-guidelines activates
   - You edit a file in `app/views/` → rails-frontend-guidelines activates
   - You edit a file in `app/javascript/controllers/` → rails-frontend-guidelines activates

2. **Keyword triggers:**
   - You mention "controller", "model", "migration" → rails-backend-guidelines
   - You mention "Turbo", "Stimulus", "view" → rails-frontend-guidelines

3. **Intent pattern matching:**
   - "Create a new controller" → rails-backend-guidelines
   - "Add a Turbo Frame" → rails-frontend-guidelines

All this is configured in `skill-rules.json` with Rails-specific patterns.

---

## skill-rules.json Configuration

This project's skill-rules.json is configured for Rails:

```json
{
  "rails-backend-guidelines": {
    "fileTriggers": {
      "pathPatterns": [
        "app/controllers/**/*.rb",
        "app/models/**/*.rb",
        "app/services/**/*.rb",
        "db/migrate/**/*.rb",
        "config/routes.rb"
      ]
    },
    "promptTriggers": {
      "keywords": [
        "controller",
        "model",
        "service",
        "migration",
        "ActiveRecord"
      ]
    }
  },
  "rails-frontend-guidelines": {
    "fileTriggers": {
      "pathPatterns": [
        "app/views/**/*.html.erb",
        "app/javascript/controllers/**/*_controller.js"
      ]
    },
    "promptTriggers": {
      "keywords": ["view", "Turbo", "Stimulus", "Hotwire", "Tailwind"]
    }
  }
}
```

**Enforcement Levels:**

- **suggest**: Skill appears as suggestion (most skills)
- **block**: Must use skill before proceeding (guardrails for critical changes)

---

## Adding New Skills

### Step 1: Create skill directory

```bash
mkdir -p .claude/skills/my-skill/resources
```

### Step 2: Create SKILL.md

```markdown
---
name: my-skill
description: What this skill does
---

# My Skill Title

## Purpose

[Why this skill exists]

## When to Use This Skill

[Auto-activation scenarios]

## Quick Reference

[Key patterns and examples]

## Resource Files

- [topic-1.md](resources/topic-1.md)
- [topic-2.md](resources/topic-2.md)
```

### Step 3: Add to skill-rules.json

```json
{
  "my-skill": {
    "type": "domain",
    "enforcement": "suggest",
    "priority": "high",
    "fileTriggers": {
      "pathPatterns": ["path/to/files/**/*.rb"]
    },
    "promptTriggers": {
      "keywords": ["keyword1", "keyword2"]
    }
  }
}
```

### Step 4: Test

- Edit a file matching your path pattern
- The skill should suggest automatically

---

## Troubleshooting

### Skill isn't activating

**Check:**

1. Is skill directory in `.claude/skills/`?
2. Is skill listed in `skill-rules.json`?
3. Do `pathPatterns` match Rails file structure?
4. Are hooks installed and executable?
5. Is settings.json configured correctly?

**Debug:**

```bash
# Check skill exists
ls -la .claude/skills/

# Validate skill-rules.json
cat .claude/skills/skill-rules.json | jq .

# Check hooks are executable
ls -la .claude/hooks/*.sh

# Should see rwxr-xr-x permissions
```

### Skill activates too often

Update `skill-rules.json`:

- Make keywords more specific
- Narrow `pathPatterns`
- Increase specificity of `intentPatterns`

### Skill never activates

Update `skill-rules.json`:

- Add more keywords
- Broaden `pathPatterns`
- Add more `intentPatterns`

---

## Rails-Specific Notes

This skills configuration is optimized for Rails projects:

- **No TypeScript/Node.js patterns** - Pure Rails focus
- **Rails conventions** - Follows Rails file structure (`app/`, `db/`, `config/`)
- **Hotwire patterns** - Modern Rails frontend with Turbo/Stimulus
- **ActiveRecord focus** - Database patterns using ActiveRecord, not Prisma/ORMs
- **Rails testing** - Minitest/RSpec patterns, not Jest/Vitest

**File patterns match Rails structure:**

```
app/
  controllers/  → rails-backend-guidelines
  models/       → rails-backend-guidelines
  services/     → rails-backend-guidelines
  views/        → rails-frontend-guidelines
  javascript/   → rails-frontend-guidelines
  components/   → rails-frontend-guidelines
db/
  migrate/      → rails-backend-guidelines
config/
  routes.rb     → rails-backend-guidelines
```

---

## For Claude Code

**When working in this project:**

1. ✅ Skills are Rails-optimized
2. ✅ Auto-activation configured for Rails file structure
3. ✅ No TypeScript/React patterns (use Rails patterns)
4. ✅ Hotwire/Turbo/Stimulus patterns included

**Skills will automatically suggest when:**

- You edit Rails controllers, models, or views
- You mention Rails-specific keywords
- You work with migrations or routes

**Questions?** See [CLAUDE_INTEGRATION.md](../../CLAUDE_INTEGRATION.md) for complete integration details.
