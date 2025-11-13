# Claude Code Setup - Quick Start Guide

**Status**: ✅ Ready to Use (with optional enhancements)

---

## 🎉 What's Working Right Now

Your Claude Code infrastructure is **functional and ready to use**. Here's what you have:

### ✅ Skill Auto-Activation
When you edit Rails files or ask Rails questions, the appropriate skill will automatically activate.

**Try it:**
```bash
# Edit a controller
vim app/controllers/bots/messages_controller.rb

# Or ask:
"How should I structure a RESTful controller?"

# Expected: rails-backend-guidelines skill activates
```

### ✅ 7 Specialized Agents
All adapted for Rails, ready to use immediately.

**Try it:**
```bash
# In conversation with Claude:
"Use the code-architecture-reviewer agent to review app/models/bot/message.rb"

# Agent will analyze your code against Rails conventions
```

### ✅ 2 Slash Commands
For planning and documentation management.

**Try it:**
```bash
/dev-docs implement GitHub webhook retry logic

# Creates task breakdown in dev/active/
```

### ✅ Comprehensive Rails Backend Skill
Main SKILL.md is complete with all Rails patterns.

---

## 🚀 Quick Usage Examples

### Example 1: Getting Rails Guidance

**You:** "I need to add a publish action to my PostsController. Should I create a new action?"

**Claude (with skill):** "❌ No! Following Rails conventions and the Botster Hub rule of RESTful-only controllers, you should NOT add a custom `publish` action. Instead:

**Option 1:** Add a `publish!` method to your Post model:
```ruby
class Post < ApplicationRecord
  def publish!(user)
    transaction do
      update!(published_at: Time.current, status: :published)
      notify_subscribers
    end
  end
end
```

Then call it from your update action or create a separate `PublishedPostsController`."

### Example 2: Using an Agent

**You:** "Use the refactor-planner agent to analyze my models directory"

**Agent:** 
1. Scans all models
2. Identifies large files, missing validations, N+1 queries
3. Creates a detailed refactoring plan in markdown
4. Returns comprehensive report with phases and priorities

### Example 3: Creating a Development Plan

**You:** `/dev-docs implement webhook retry with exponential backoff`

**Result:**
```
✓ Created: dev/active/webhook-retry/
  ├── webhook-retry-plan.md (strategic overview)
  ├── webhook-retry-context.md (key files, decisions)
  └── webhook-retry-tasks.md (actionable checklist)
```

---

## 📋 Available Agents

### 1. code-architecture-reviewer
**Use when:** After implementing a feature, want architectural review

**Example:**
```
"Use the code-architecture-reviewer agent to review my new WebhooksController"
```

**Output:** Detailed review checking:
- Rails conventions followed?
- RESTful design?
- Proper use of strong parameters?
- Security issues?
- Performance concerns?

### 2. code-refactor-master
**Use when:** Need to reorganize code, extract concerns, break down large files

**Example:**
```
"Use the code-refactor-master agent to reorganize my models directory"
```

**Output:** Step-by-step refactoring plan with file moves, dependency tracking

### 3. documentation-architect
**Use when:** Need to document a feature, API, or system flow

**Example:**
```
"Use the documentation-architect agent to document the bot message acknowledgment flow"
```

**Output:** Comprehensive documentation with diagrams, code examples, troubleshooting

### 4. plan-reviewer
**Use when:** Have a development plan, want critique before implementing

**Example:**
```
"Use the plan-reviewer agent to review my plan for integrating Stripe"
```

**Output:** Critical analysis: what's missing, risks, better alternatives

### 5. refactor-planner
**Use when:** Want to refactor but need a solid plan first

**Example:**
```
"Use the refactor-planner agent to plan refactoring the authentication system"
```

**Output:** Phased refactoring plan with risk assessment, rollback strategies

### 6. web-research-specialist
**Use when:** Debugging obscure errors, researching best practices

**Example:**
```
"Use the web-research-specialist agent to research Action Cable authentication patterns"
```

**Output:** Curated findings from GitHub, Stack Overflow, Rails forums, official docs

### 7. auto-error-resolver
**Use when:** Have errors preventing server from starting, tests failing

**Example:**
```
"Use the auto-error-resolver agent to fix the migration errors"
```

**Output:** Automatically identifies and fixes Ruby/Rails errors

---

## 📚 Available Slash Commands

### /dev-docs
**Purpose:** Create comprehensive development plans with task breakdown

**Syntax:**
```
/dev-docs [description of what to plan]
```

**Examples:**
```
/dev-docs implement user authentication with Devise
/dev-docs refactor bot message queue system
/dev-docs add rate limiting to API endpoints
```

**Output:**
- `dev/active/[task-name]/[task-name]-plan.md` - Full strategic plan
- `dev/active/[task-name]/[task-name]-context.md` - Key files, decisions, dependencies
- `dev/active/[task-name]/[task-name]-tasks.md` - Checklist format for tracking

### /dev-docs-update
**Purpose:** Update documentation before context reset (approaching token limits)

**Syntax:**
```
/dev-docs-update [optional: specific focus]
```

**Examples:**
```
/dev-docs-update
/dev-docs-update bot message acknowledgment flow
```

**Output:** Updates all active task documentation with current state, decisions made, next steps

---

## 🎯 Skill Activation Keywords

The skills will **auto-activate** when you mention these keywords:

### Backend Skill Triggers:
- controller, model, ActiveRecord
- migration, database, schema
- validation, association, scope
- has_many, belongs_to, has_one
- route, routing, RESTful
- webhook, strong params
- Solid Queue, Active Job, Action Cable

### Frontend Skill Triggers (when frontend skill is complete):
- Turbo, Turbo Frame, Turbo Stream
- Stimulus, Hotwire
- turbo_frame_tag, data-controller
- ViewComponent, partial
- form_with, dom_id

---

## 🏗️ Project-Specific Architectural Rules

The skills enforce **Botster Hub architectural principles**:

### ✅ DO:

**Put business logic in models:**
```ruby
class Bot::Message < ApplicationRecord
  def acknowledge!
    update!(acknowledged_at: Time.current, status: 'acknowledged')
  end
end
```

**Use RESTful controllers only:**
```ruby
class Bots::MessagesController < ApplicationController
  # Only: index, show, create, update, destroy
  def update
    @message.acknowledge!
    head :ok
  end
end
```

**Generic utilities in lib/:**
```ruby
# lib/rate_limiter.rb
module RateLimiter
  def self.check(key, limit:, period:)
    # Generic rate limiting
  end
end
```

### ❌ DON'T:

**NO app/services/ directory:**
```ruby
# ❌ Don't create: app/services/message_acknowledger.rb
# ✅ Use models: app/models/bot/message.rb with acknowledge! method
```

**NO custom controller actions (except webhooks):**
```ruby
class PostsController
  def publish  # ❌ Not RESTful!
  end
  
  def archive  # ❌ Not RESTful!
  end
end

# ✅ Instead: Put logic in model, call from update action
# ✅ OR: Create PublishedPostsController (resourceful)
```

**NO generic code in models:**
```ruby
# ❌ app/models/jwt_handler.rb - This is generic!
# ✅ lib/json_web_token.rb - Generic belongs in lib/
```

---

## 🧪 Testing Your Setup

### Test 1: Skill Activation
```bash
# Say: "Help me create a RESTful controller"
# Expected: 
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# 🎯 SKILL ACTIVATION CHECK
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# 
# 📚 RECOMMENDED SKILLS:
#   → rails-backend-guidelines
```

### Test 2: Agent Usage
```bash
# Say: "Use the code-architecture-reviewer agent to review app/models/user.rb"
# Expected: Agent spawns, reviews file, returns detailed report
```

### Test 3: Slash Command
```bash
# Say: "/dev-docs test feature"
# Expected: Creates dev/active/test-feature/ with 3 markdown files
```

### Test 4: Architectural Guidance
```bash
# Say: "Should I create a PostPublisher service in app/services/?"
# Expected: "❌ No! Don't create app/services/. Put the logic in the Post model..."
```

---

## 📈 What's Different Now?

### Before Setup:
```
You: "How do I publish a post?"
Claude: "You could create a service object in app/services/post_publisher.rb..."
```

### After Setup:
```
You: "How do I publish a post?"
Claude (with skill): "Following Botster Hub's no-services rule, add a publish! 
method to your Post model:

class Post < ApplicationRecord
  def publish!(user)
    transaction do
      update!(published_at: Time.current)
      PostNotificationJob.perform_later(id)
    end
  end
end

Then call it from your controller's update action or create a 
PublishedPostsController if you need a separate endpoint."
```

---

## 🔧 Troubleshooting

### Skill Not Activating

**Check hook is running:**
```bash
ps aux | grep skill-activation-prompt
# Should show node process

# Check dependencies installed:
ls .claude/hooks/node_modules/
# Should show: tsx, typescript, @types/node
```

**Manually test:**
```bash
cd .claude/hooks
npm run check
# Should compile without errors
```

### Agent Doesn't Work

**Check agent file exists:**
```bash
ls -la .claude/agents/code-architecture-reviewer.md
# Should exist with -rw-r--r-- permissions
```

**Try explicit invocation:**
```
"Use the Task tool with subagent_type='code-architecture-reviewer' 
and prompt='Review app/models/user.rb'"
```

### Slash Command Not Found

**Check command file exists:**
```bash
ls -la .claude/commands/dev-docs.md
# Should exist
```

**Try SlashCommand tool directly:**
```
"Use the SlashCommand tool with command='/dev-docs test'"
```

---

## 📊 Setup Summary

**What You Have:**
- ✅ 3 Hook files (skill-activation working)
- ✅ 7 Agent files (all Rails-adapted)
- ✅ 2 Slash command files
- ✅ 1 Complete backend skill (main file)
- ✅ Enhanced skill-rules.json with Rails triggers
- ✅ 28 total files added/modified

**What's Optional:**
- ⏳ Backend skill resource files (8 deep-dive guides)
- ⏳ Frontend skill complete rewrite (Hotwire/Turbo/Stimulus)

**Current Status:**
- **Fully functional** for Rails backend development
- **Main SKILL.md is comprehensive** - resource files are nice-to-have extras
- **All agents working** - ready for complex tasks
- **Slash commands ready** - task management enabled

---

## 🎓 Learning the Setup

### Start Small:
1. **Week 1:** Just ask Rails questions, notice skills activating
2. **Week 2:** Try one agent (code-architecture-reviewer after a feature)
3. **Week 3:** Use /dev-docs for a new feature
4. **Week 4:** Experiment with other agents

### Gradually Build Resources:
When you encounter a topic you want deeper guidance on:
```
"Can you create the controllers-and-routing.md resource file 
for the rails-backend-guidelines skill? Include examples from 
my project's webhooks/github_controller.rb"
```

This way you build the resources **as you need them**, not all at once.

---

## 🚀 Next-Level Usage

### Combine Tools:
```bash
# 1. Create plan
/dev-docs implement two-factor authentication

# 2. Review plan
"Use the plan-reviewer agent to check dev/active/two-factor-auth/two-factor-auth-plan.md"

# 3. Implement following plan
# ... do the work ...

# 4. Review implementation
"Use the code-architecture-reviewer agent to review my 2FA implementation"

# 5. Document it
"Use the documentation-architect agent to document the 2FA system"
```

### Weekly Workflow:
```bash
# Monday: Plan the week
/dev-docs weekly features: user dashboard, notification preferences, API rate limiting

# During week: Ask questions
"How should I structure the notification preferences model?"
# → Skill provides guidance

# Friday: Review before merge
"Use the code-architecture-reviewer agent to review this week's changes"

# Before context reset:
/dev-docs-update weekly progress
```

---

## 📞 Getting Help

**If something isn't working:**

1. **Check documentation:** `CLAUDE_CODE_SETUP_COMPLETE.md` has detailed troubleshooting
2. **Test components:** Run the test commands in this guide
3. **Use web-research-specialist:** "Use the web-research-specialist agent to research [your issue]"
4. **Ask Claude:** The skill system itself can explain how it works!

**For enhancements:**
```
"Can you create the [topic].md resource file for the backend skill?"
"Can you add more triggers for [specific pattern] to skill-rules.json?"
```

---

## 🎉 You're Ready!

The setup is **complete and functional**. You have:

- ✅ Automatic skill activation
- ✅ 7 powerful agents for complex tasks
- ✅ Task management with slash commands
- ✅ Project-specific architectural enforcement
- ✅ Rails-specific guidance at your fingertips

**Start using it!** The more you use it, the more valuable it becomes. And you can always enhance it later by adding resource files or adapting the frontend skill when you work on Hotwire features.

---

**Happy coding with your enhanced Claude Code setup!** 🚀
