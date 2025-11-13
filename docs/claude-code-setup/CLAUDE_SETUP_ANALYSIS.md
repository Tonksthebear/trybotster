# Claude Code Infrastructure Analysis & Fix Plan

## Overview

This document analyzes the current Claude Code setup in the trybotster Rails project compared to the original showcase repository and provides a comprehensive plan to fix the issues.

## Current State

### What Exists
- ✅ `.claude/skills/skill-developer/` - Universal skill (correctly copied)
- ✅ `.claude/skills/rails-backend-guidelines/` - Renamed from backend-dev-guidelines
- ✅ `.claude/skills/rails-frontend-guidelines/` - Renamed from frontend-dev-guidelines
- ✅ `.claude/hooks/skill-activation-prompt.sh` - Universal hook
- ✅ `.claude/hooks/post-tool-use-tracker.sh` - Universal hook
- ✅ `.claude/hooks/rubocop-check.sh` - Custom Rails hook (good!)
- ✅ `.claude/settings.json` - Configured with Rails-appropriate hooks
- ✅ `.claude/skills/skill-rules.json` - Basic Rails adaptation

### What's Missing from Original
- ❌ `.claude/hooks/skill-activation-prompt.ts` - TypeScript implementation
- ❌ `.claude/hooks/package.json` - Node dependencies for TypeScript hooks
- ❌ `.claude/hooks/tsconfig.json` - TypeScript configuration
- ❌ `.claude/agents/` directory - No agents copied
- ❌ `.claude/commands/` directory - No slash commands copied
- ❌ Many original hooks deleted (error-handling-reminder, stop-build-check, etc.)

## Major Issues Identified

### 1. Skills Not Properly Adapted for Rails

**Problem:** The skills were renamed but the CONTENT is still Node.js/Express-specific

**Current State:**
- `rails-backend-guidelines/SKILL.md` - Still contains Express/Prisma examples
- `rails-backend-guidelines/resources/` - Still references Node.js patterns
- `rails-frontend-guidelines/SKILL.md` - Still contains React/MUI v7 examples

**What Should Happen:**
According to CLAUDE_INTEGRATION.md, when tech stacks differ, we should:
1. **Adapt** the skill for Rails (Option 1) - Keep structure, replace examples
2. **Extract** framework-agnostic patterns (Option 2) - Only architectural principles
3. **Reference** only (Option 3) - Use as template for new Rails-specific skill

**Recommended Fix:**
- Create NEW Rails-specific skills from scratch using the original structure as a template
- Keep the progressive disclosure pattern (main SKILL.md + resources/)
- Replace ALL code examples with Rails equivalents
- Update architectural patterns for Rails conventions

### 2. Missing Hook Dependencies

**Problem:** `skill-activation-prompt.sh` calls a TypeScript file that doesn't exist

**Current State:**
```bash
# skill-activation-prompt.sh calls:
npx tsx skill-activation-prompt.ts
```

**Missing:**
- `skill-activation-prompt.ts` - The actual implementation
- `package.json` - Defines tsx and dependencies
- `package-lock.json` - Lock file
- `tsconfig.json` - TypeScript config
- `node_modules/` - Dependencies (need to run `npm install`)

**Impact:**
- The UserPromptSubmit hook likely fails silently
- Skill auto-suggestion doesn't work

**Recommended Fix:**
- Copy the TypeScript files and package files from original
- Run `npm install` in `.claude/hooks/`
- Verify the hook works

### 3. Missing Agents (All of Them)

**Problem:** None of the 11 agents were copied from the showcase

**What's Missing:**
1. `auth-route-debugger.md` - Debug auth routes
2. `auth-route-tester.md` - Test auth routes
3. `auto-error-resolver.md` - Auto-fix errors
4. `code-architecture-reviewer.md` - Review architecture
5. `code-refactor-master.md` - Comprehensive refactoring
6. `documentation-architect.md` - Create docs
7. `frontend-error-fixer.md` - Fix frontend errors
8. `plan-reviewer.md` - Review implementation plans
9. `refactor-planner.md` - Plan refactoring
10. `web-research-specialist.md` - Research online
11. (Plus README.md explaining agents)

**Which Agents Apply to Rails:**

✅ **Universal (Copy as-is):**
- `code-architecture-reviewer.md`
- `code-refactor-master.md`
- `documentation-architect.md`
- `plan-reviewer.md`
- `refactor-planner.md`
- `web-research-specialist.md`
- `auto-error-resolver.md`

⚠️ **Need Adaptation:**
- `auth-route-tester.md` - If Rails uses JWT cookies (check project)
- `auth-route-debugger.md` - Same as above
- `frontend-error-fixer.md` - Adapt for Hotwire/Stimulus instead of React

❌ **Skip:**
- None! Most agents are framework-agnostic

**Recommended Fix:**
- Copy all universal agents immediately
- Check if project uses JWT cookie auth for auth agents
- Adapt frontend-error-fixer for Hotwire

### 4. Missing Slash Commands

**Problem:** No slash commands copied

**What's Missing:**
1. `/dev-docs` - Create dev documentation
2. `/dev-docs-update` - Update dev docs before compaction
3. `/route-research-for-testing` - Research routes for testing

**Which Apply to Rails:**

✅ `/dev-docs` - Universal, works for any project
✅ `/dev-docs-update` - Universal
⚠️ `/route-research-for-testing` - Need to adapt for Rails routes

**Recommended Fix:**
- Copy dev-docs commands (they're universal)
- Adapt route-research for Rails routing (`config/routes.rb`, `rails routes`)

### 5. Skill Content Issues

**Problem:** The skill STRUCTURE is there but CONTENT wasn't adapted

Let me check what's actually in the Rails skills:

**Current rails-backend-guidelines/**
- Has SKILL.md (but likely still Node.js content)
- Has resources/ directory (but likely still Express/Prisma examples)

**Should Have (Rails-specific):**
- SKILL.md - Overview with Rails patterns
- resources/
  - `architecture-overview.md` - MVC in Rails context
  - `controllers-and-routing.md` - Rails controllers & routes.rb
  - `models-and-active-record.md` - ActiveRecord patterns
  - `services-and-concerns.md` - Service objects & concerns
  - `validations-and-callbacks.md` - Rails validations
  - `background-jobs.md` - Solid Queue / Active Job
  - `testing-guide.md` - RSpec / Minitest
  - `api-patterns.md` - Rails API patterns
  - `database-migrations.md` - Rails migrations
  - `action-cable.md` - WebSockets with Action Cable

**Recommended Fix:**
- COMPLETE REWRITE of both skills for Rails
- Use original structure/organization as template
- All examples must be Rails-specific

### 6. Hook Configuration Issues

**Problem:** Missing TypeScript support for hooks

**Current hooks configuration:**
```json
{
  "UserPromptSubmit": [...],  // Calls .ts file without TypeScript setup
  "PostToolUse": [...],        // ✅ Works (bash only)
  "Stop": [...]                // ✅ Works (rubocop)
}
```

**Missing:**
- The actual TypeScript implementation files
- Node.js dependencies to run TypeScript
- Error handling if TypeScript fails

**Recommended Fix:**
- Copy all TypeScript hook files
- Set up Node.js environment in hooks/
- Test each hook individually

## Comparison: Original vs Current

### Original Showcase Structure
```
.claude/
├── settings.json ✅ (adapted)
├── skills/
│   ├── skill-rules.json ✅ (adapted)
│   ├── skill-developer/ ✅ (copied)
│   ├── backend-dev-guidelines/ ⚠️ (renamed but not adapted)
│   ├── frontend-dev-guidelines/ ⚠️ (renamed but not adapted)
│   ├── route-tester/ ❌ (not copied)
│   └── error-tracking/ ❌ (not copied)
├── hooks/
│   ├── skill-activation-prompt.sh ✅
│   ├── skill-activation-prompt.ts ❌
│   ├── post-tool-use-tracker.sh ✅
│   ├── package.json ❌
│   ├── tsconfig.json ❌
│   ├── error-handling-reminder.sh ❌
│   ├── error-handling-reminder.ts ❌
│   ├── tsc-check.sh ❌ (Rails doesn't need)
│   ├── trigger-build-resolver.sh ❌ (Rails doesn't need)
│   └── stop-build-check-enhanced.sh ❌ (Rails doesn't need)
├── agents/ ❌ (entire directory missing)
│   ��── 11 agent files ❌
└── commands/ ❌ (entire directory missing)
    └── 3 command files ❌
```

### Current Rails Project Structure
```
.claude/
├── settings.json ✅ (good!)
├── skills/
│   ├── skill-rules.json ✅ (basic adaptation)
│   ├── skill-developer/ ✅ (correct)
│   ├── rails-backend-guidelines/ ⚠️ (exists but wrong content)
│   └── rails-frontend-guidelines/ ⚠️ (exists but wrong content)
├── hooks/
│   ├── skill-activation-prompt.sh ✅
│   ├── post-tool-use-tracker.sh ✅
│   └── rubocop-check.sh ✅ (good Rails addition!)
└── (no agents or commands)
```

## Recommended Action Plan

### Phase 1: Fix Hook Infrastructure (High Priority)
1. Copy TypeScript hook files from original
2. Copy package.json and tsconfig.json
3. Run `npm install` in hooks/
4. Test skill-activation-prompt hook
5. Consider adding error-handling-reminder hook

### Phase 2: Add Universal Agents (High Priority)
1. Create `.claude/agents/` directory
2. Copy these agents as-is:
   - code-architecture-reviewer.md
   - code-refactor-master.md
   - documentation-architect.md
   - plan-reviewer.md
   - refactor-planner.md
   - web-research-specialist.md
   - auto-error-resolver.md
3. Copy agents/README.md
4. Test one agent to verify they work

### Phase 3: Add Slash Commands (Medium Priority)
1. Create `.claude/commands/` directory
2. Copy dev-docs.md (universal)
3. Copy dev-docs-update.md (universal)
4. Adapt route-research-for-testing.md for Rails

### Phase 4: Completely Rewrite Skills (High Priority, Time-Consuming)
1. Keep the skill directory structure
2. Rewrite `rails-backend-guidelines/SKILL.md`:
   - Remove ALL Node.js/Express references
   - Add Rails MVC patterns
   - Add ActiveRecord examples
   - Add Rails routing patterns
3. Rewrite each resource file in `rails-backend-guidelines/resources/`:
   - `architecture-overview.md` - Rails MVC
   - `controllers-and-routing.md` - Rails controllers
   - `models-and-active-record.md` - AR patterns
   - `services-and-concerns.md` - Service objects
   - `validations.md` - Rails validations
   - `background-jobs.md` - Solid Queue/Active Job
   - `testing.md` - RSpec patterns
   - `api-patterns.md` - Rails API
   - `database.md` - Migrations & schema
4. Rewrite `rails-frontend-guidelines/SKILL.md`:
   - Remove ALL React/MUI references
   - Add Hotwire (Turbo + Stimulus) patterns
   - Add Tailwind CSS patterns
   - Add ERB templating patterns
5. Rewrite each resource file in `rails-frontend-guidelines/resources/`:
   - `hotwire-overview.md` - Turbo + Stimulus
   - `turbo-frames.md` - Turbo Frame patterns
   - `turbo-streams.md` - Turbo Stream patterns
   - `stimulus-controllers.md` - Stimulus patterns
   - `view-components.md` - ViewComponent patterns
   - `tailwind-patterns.md` - Tailwind CSS
   - `forms.md` - Rails forms + Turbo
   - `javascript-integration.md` - Import maps

### Phase 5: Optional Rails-Specific Components
1. Consider adding:
   - `rails-testing` skill (RSpec/Minitest patterns)
   - `rails-api` skill (API-specific patterns)
   - Rails-specific agents if needed
2. Update skill-rules.json with better triggers

## Key Principles from CLAUDE_INTEGRATION.md

### What the Integration Guide Says:

1. **NEVER copy settings.json as-is** ✅ (we didn't, good!)
2. **ALWAYS adapt pathPatterns** ✅ (we did for Rails)
3. **Check tech stack compatibility** ❌ (we renamed but didn't adapt content)
4. **Most agents are universal** ❌ (we didn't copy any!)
5. **Hooks need their dependencies** ❌ (missing TypeScript setup)

### What Transfers Across Tech Stacks:
✅ Layered architecture (Routes→Controllers→Services) - YES for Rails!
✅ Separation of concerns - YES
✅ File organization - YES (but Rails has different conventions)
✅ Error handling philosophy - YES
✅ Testing strategies - YES

### What DOESN'T Transfer:
❌ Express middleware → Rails middleware/concerns (similar concept, different syntax)
❌ Prisma ORM → ActiveRecord (similar concept, different syntax)
❌ React components → ERB partials/ViewComponents (completely different)
❌ MUI v7 → Tailwind CSS (completely different)
❌ Node.js patterns → Ruby patterns (different language!)

## Estimated Effort

### Quick Wins (1-2 hours):
- Copy missing hook files
- Run npm install
- Copy universal agents
- Copy slash commands
- Test that hooks work

### Medium Effort (4-6 hours):
- Rewrite rails-backend-guidelines SKILL.md
- Rewrite 5-7 key resource files
- Test skill activation

### Large Effort (8-12 hours):
- Complete rewrite of all backend resources
- Complete rewrite of all frontend resources
- Comprehensive testing
- Add Rails-specific examples throughout

## Testing Plan

After each fix:
1. Test skill activation by editing relevant files
2. Test hooks by triggering the appropriate events
3. Test agents by calling Task tool
4. Test commands by using SlashCommand tool
5. Verify no errors in console/logs

## Questions to Answer

1. **Does this Rails project use JWT cookie authentication?**
   - YES → Copy auth agents
   - NO → Skip auth agents or adapt for your auth

2. **Do you want ALL agents or just some?**
   - Recommend: Start with universal ones, add more as needed

3. **How much time to invest in skills?**
   - Minimal: Use skill-developer only, create Rails skills as you go
   - Moderate: Rewrite main SKILL.md files with Rails examples
   - Maximum: Complete rewrite of all resources with comprehensive Rails patterns

4. **Do you use Sentry or error tracking?**
   - YES → Consider copying error-tracking skill and adapting
   - NO → Skip it

## Next Steps

Recommend starting with:
1. Fix hooks (copy TypeScript files, npm install)
2. Copy universal agents
3. Test that everything works
4. Then decide: completely rewrite skills OR create new ones gradually using skill-developer

## Summary

**The main issue:** The conversion was **structural** (renamed files, updated paths) but not **content-based** (all examples still Node.js/Express/React instead of Rails/ActiveRecord/Hotwire).

**Quick fix:** Copy missing pieces (hooks, agents, commands)

**Proper fix:** Completely rewrite both skills with Rails-specific content while keeping the excellent organizational structure from the original.
