---
description: Update dev documentation before context compaction
argument-hint: Optional - specific context or tasks to focus on (leave empty for comprehensive update)
---

We're approaching context limits. Please update the development documentation to ensure seamless continuation after context reset.

## Required Updates

### 1. Update Active Task Documentation

For each task in `/dev/active/`:

- Update `[task-name]-context.md` with:
  - Current implementation state
  - Key decisions made this session
  - Files modified and why
  - Any blockers or issues discovered
  - Next immediate steps
  - Last Updated timestamp

- Update `[task-name]-tasks.md` with:
  - Mark completed tasks as ✅
  - Add any new tasks discovered
  - Update in-progress tasks with current status
  - Reorder priorities if needed

### 2. Capture Session Context

Include any relevant information about:

- Complex problems solved
- Architectural decisions made
- Tricky bugs found and fixed
- Integration points discovered
- Testing approaches used
- Performance optimizations made
- Rails-specific patterns used

### 3. Update Documentation

- Store any new patterns or solutions in project documentation
- Update any architecture diagrams or overviews
- Document new API endpoints or routes
- Add notes about database schema changes

### 4. Document Unfinished Work

- What was being worked on when context limit approached
- Exact state of any partially completed features
- Commands that need to be run on restart (migrations, etc.)
- Any temporary workarounds that need permanent fixes
- Uncommitted changes that need attention

### 5. Create Handoff Notes

If switching to a new conversation:

- Exact file and line being edited
- The goal of current changes
- Any database migrations pending
- Test commands to verify work
- Rails server state or configuration changes

## Additional Context: $ARGUMENTS

**Priority**: Focus on capturing information that would be hard to rediscover or reconstruct from code alone. For Rails projects, especially note database schema changes, routing updates, and service integrations.
