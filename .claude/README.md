# Claude Code Configuration for Trybotster

This directory contains Claude Code configuration adapted specifically for the Trybotster Rails application.

## Project Overview

**Tech Stack:**
- Ruby on Rails 8.1+
- Hotwire (Turbo + Stimulus)
- Tailwind CSS
- PostgreSQL
- Solid Queue (background jobs)
- Action Cable (WebSockets)
- ActionMCP (MCP server integration)

**Architecture:**
- Single Rails application
- Server-rendered HTML with progressive enhancement
- Zero-build JavaScript (Importmap)
- RESTful API endpoints

---

## Directory Structure

```
.claude/
├── settings.json           # Claude Code settings & hooks
├── skills/                 # Development guidelines
│   ├── skill-rules.json   # Skill activation rules
│   ├── rails-frontend-guidelines/  # Hotwire, Stimulus, Tailwind
│   ├── rails-backend-guidelines/   # Rails MVC patterns
│   └── skill-developer/   # Meta-skill for creating skills
├── agents/                 # Specialized task agents
│   ├── code-architecture-reviewer.md
│   ├── code-refactor-master.md
│   ├── documentation-architect.md
│   ├── plan-reviewer.md
│   ├── refactor-planner.md
│   └── web-research-specialist.md
├── commands/               # Slash commands
│   ├── dev-docs.md        # Planning command
│   └── dev-docs-update.md # Documentation updates
└── hooks/                  # Event hooks
    ├── skill-activation-prompt.sh
    ├── post-tool-use-tracker.sh
    └── rubocop-check.sh
```

---

## Skills

### rails-frontend-guidelines

**Purpose:** Guidelines for Rails frontend development using Hotwire (Turbo + Stimulus) and Tailwind CSS.

**Triggers:**
- Working with `.html.erb` views
- Creating Stimulus controllers (`*_controller.js`)
- Using Turbo Frames or Turbo Streams
- Keywords: "view", "partial", "Turbo", "Stimulus", "Hotwire", "Tailwind"

**Key Resources:**
- `turbo-guide.md` - Turbo Drive, Frames, and Streams
- `stimulus-guide.md` - Stimulus controllers and patterns
- `complete-examples.md` - Real-world examples

### rails-backend-guidelines

**Purpose:** Guidelines for Rails backend development following Rails conventions and best practices.

**Triggers:**
- Working with controllers, models, services
- Database migrations
- Keywords: "controller", "model", "ActiveRecord", "migration", "service"

**Key Resources:**
- `SKILL.md` - Quick reference and navigation
- Additional resources TBD (create as needed)

### skill-developer

**Purpose:** Meta-skill for creating and managing Claude Code skills.

**When to use:** Creating new skills or modifying skill configurations.

---

## Agents

All agents are framework-agnostic and work with any codebase:

- **code-architecture-reviewer**: Reviews code for best practices and architectural consistency
- **code-refactor-master**: Helps refactor code for better organization
- **documentation-architect**: Creates comprehensive documentation
- **plan-reviewer**: Reviews development plans before implementation
- **refactor-planner**: Creates refactoring plans
- **web-research-specialist**: Researches technical information online

---

## Hooks

### UserPromptSubmit Hook
**File:** `skill-activation-prompt.sh`  
**Purpose:** Auto-suggests relevant skills based on your prompts  
**Status:** ✅ Active

### PostToolUse Hook
**File:** `post-tool-use-tracker.sh`  
**Purpose:** Tracks file changes for context management  
**Status:** ✅ Active

### Stop Hook
**File:** `rubocop-check.sh`  
**Purpose:** Runs Rubocop checks before stopping work  
**Status:** ✅ Active (graceful - won't block if Rubocop not installed)

---

## Commands

### /dev-docs [description]
Create a comprehensive strategic plan with structured task breakdown.

**Example:** `/dev-docs implement user authentication system`

### /dev-docs-update [focus area]
Update development documentation before context compaction.

**Example:** `/dev-docs-update authentication flow`

---

## Configuration Notes

### Removed from Showcase
The following were removed as they're not relevant to Rails:

**Skills:**
- ❌ frontend-dev-guidelines (React/MUI) → ✅ Replaced with rails-frontend-guidelines
- ❌ backend-dev-guidelines (Express/Node) → ✅ Replaced with rails-backend-guidelines
- ❌ error-tracking (Sentry-specific) → Rails has built-in error handling
- ❌ route-tester (JWT cookie auth) → Rails testing is different

**Agents:**
- ❌ auth-route-debugger (JWT cookie-specific)
- ❌ auth-route-tester (JWT cookie-specific)
- ❌ auto-error-resolver (TypeScript-specific)
- ❌ frontend-error-fixer (React/TypeScript-specific)

**Commands:**
- ❌ route-research-for-testing (Node/TypeScript-specific)

**Hooks:**
- ❌ tsc-check.sh (TypeScript-specific) → ✅ Replaced with rubocop-check.sh
- ❌ trigger-build-resolver.sh (TypeScript-specific)

---

## Getting Started

### 1. Test Skill Activation

Try editing a file in `app/controllers/` or `app/views/` - you should see skill suggestions appear.

### 2. Try a Command

Run `/dev-docs implement a new feature` to see the planning command in action.

### 3. Use an Agent

When you need code reviewed, just say "review this code" and Claude will use the code-architecture-reviewer agent.

---

## Customization

### Adding Rails-Specific Resources

The frontend and backend skills have placeholder resource files. To add more detailed resources:

1. Create `.md` files in `.claude/skills/rails-*/resources/`
2. Link them from the main `SKILL.md` file
3. Follow the progressive disclosure pattern (keep main skill < 500 lines)

### Adapting for Your Workflow

- **Add more skills**: Copy `skill-developer` skill and create new ones
- **Customize triggers**: Edit `.claude/skills/skill-rules.json`
- **Add commands**: Create `.md` files in `.claude/commands/`
- **Add hooks**: Create shell scripts in `.claude/hooks/` and register in `settings.json`

---

## Troubleshooting

### Skills not triggering?
- Check `.claude/skills/skill-rules.json` path patterns match your files
- Verify JSON is valid: `cat .claude/skills/skill-rules.json | jq .`

### Hooks not running?
- Ensure hooks are executable: `chmod +x .claude/hooks/*.sh`
- Check logs in Claude Code output

### Rubocop hook failing?
- Install Rubocop: `gem install rubocop` or add to Gemfile
- Hook won't block work if Rubocop isn't installed (graceful degradation)

---

## Next Steps

1. **Complete the resource files** - The skills have placeholder resources that can be expanded
2. **Add more Rails patterns** - Document your team's specific patterns
3. **Create project-specific skills** - Add skills for your MCP integration, memory system, etc.
4. **Test and iterate** - Use the skills and refine based on what works

---

## Questions or Issues?

This configuration was adapted from the Claude Code infrastructure showcase. If you need help:

1. Check the skill-developer skill for guidance on creating/modifying skills
2. Refer to the integration guide: `CLAUDE_INTEGRATION_GUIDE.md`
3. Ask Claude! The skills and agents are designed to help you work with this system

---

**Configuration Status:** ✅ Adapted for Rails + Hotwire  
**Last Updated:** 2025-01-12  
**Rails Version:** 8.1+  
**Hotwire:** Turbo 8.0+ / Stimulus 3.0+
