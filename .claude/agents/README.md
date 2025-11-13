# Agents

Specialized agents for complex, multi-step tasks.

---

## What Are Agents?

Agents are autonomous Claude instances that handle specific complex tasks. Unlike skills (which provide inline guidance), agents:

- Run as separate sub-tasks
- Work autonomously with minimal supervision
- Have specialized tool access
- Return comprehensive reports when complete

**Key advantage:** Agents are **standalone** - just copy the `.md` file and use immediately!

---

## Available Agents

### code-architecture-reviewer

**Purpose:** Review code for architectural consistency and best practices

**When to use:**

- After implementing a new feature
- Before merging significant changes
- When refactoring code
- To validate architectural decisions

**Integration:** ✅ Copy as-is

---

### code-refactor-master

**Purpose:** Plan and execute comprehensive refactoring

**When to use:**

- Reorganizing file structures
- Breaking down large components
- Updating import paths after moves
- Improving code maintainability

**Integration:** ✅ Copy as-is

---

### documentation-architect

**Purpose:** Create comprehensive documentation

**When to use:**

- Documenting new features
- Creating API documentation
- Writing developer guides
- Generating architectural overviews

**Integration:** ✅ Copy as-is

---

### plan-reviewer

**Purpose:** Review development plans before implementation

**When to use:**

- Before starting complex features
- Validating architectural plans
- Identifying potential issues early
- Getting second opinion on approach

**Integration:** ✅ Copy as-is

---

### refactor-planner

**Purpose:** Create comprehensive refactoring strategies

**When to use:**

- Planning code reorganization
- Modernizing legacy code
- Breaking down large files
- Improving code structure

**Integration:** ✅ Copy as-is

---

### web-research-specialist

**Purpose:** Research technical issues online

**When to use:**

- Debugging obscure errors
- Finding solutions to problems
- Researching best practices
- Comparing implementation approaches

**Integration:** ✅ Copy as-is

---

### auto-error-resolver

**Purpose:** Automatically fix compilation errors

**When to use:**

- Build failures with errors
- After refactoring that breaks code
- Systematic error resolution needed

**Integration:** ⚠️ Adapted for Rails (checks for Ruby/Rails errors)

---

## How to Use an Agent

Ask Claude: "Use the [agent-name] agent to [task]"

Examples:

- "Use the code-architecture-reviewer agent to review my new controller"
- "Use the documentation-architect agent to document the authentication flow"
- "Use the refactor-planner agent to plan reorganizing the models directory"

---

## When to Use Agents vs Skills

| Use Agents When...                | Use Skills When...              |
| --------------------------------- | ------------------------------- |
| Task requires multiple steps      | Need inline guidance            |
| Complex analysis needed           | Checking best practices         |
| Autonomous work preferred         | Want to maintain control        |
| Task has clear end goal           | Ongoing development work        |
| Example: "Review all controllers" | Example: "Creating a new route" |

**Both can work together:**

- Skill provides patterns during development
- Agent reviews the result when complete

---

## For Claude Code

**When integrating agents for a user:**

1. **Just copy the .md file** - agents are standalone
2. **Check for hardcoded paths** and update to `$CLAUDE_PROJECT_DIR` or `.`
3. **Verify tech stack compatibility** for framework-specific agents

---

## Creating Your Own Agents

Agents are markdown files with optional YAML frontmatter:

```markdown
---
name: agent-name
description: What this agent does
model: sonnet
color: blue
---

# Agent Instructions

Detailed instructions for autonomous execution...
```

**Tips:**

- Be very specific in instructions
- Break complex tasks into numbered steps
- Specify exactly what to return
- Include examples of good output
- List available tools explicitly
