#!/bin/bash
# Simple bash-only skill activation checker
# No dependencies, just pattern matching

# Read the prompt from stdin (JSON input from Claude)
input=$(cat)

# Extract prompt field from JSON - handle both escaped and unescaped quotes
prompt=$(echo "$input" | sed -n 's/.*"prompt"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | tr '[:upper:]' '[:lower:]')

# If we can't extract prompt, exit silently
if [ -z "$prompt" ]; then
    exit 0
fi

# Track which skills matched
backend_match=0
frontend_match=0
skill_dev_match=0

# Backend skill keywords
if echo "$prompt" | grep -qE "(controller|model|activerecord|migration|database|validation|association|scope|has_many|belongs_to|restful|route|routing|webhook|rails generate|rails g|db:migrate|schema|strong param|callback|concern|background job|solid queue|active job|action cable)"; then
    backend_match=1
fi

# Frontend skill keywords
if echo "$prompt" | grep -qE "(view|partial|turbo|turbo frame|turbo stream|stimulus|hotwire|tailwind|erb|template|frontend|data-controller|data-action|viewcomponent|form_with|dom_id)"; then
    frontend_match=1
fi

# Skill developer keywords
if echo "$prompt" | grep -qE "(skill system|create skill|add skill|skill trigger|skill rule|hook system|skill development|skill-rules)"; then
    skill_dev_match=1
fi

# Intent pattern matching (common Rails questions)
if echo "$prompt" | grep -qiE "(how (do|does|should) (i|we).*(controller|model|route|migration|association))"; then
    backend_match=1
fi

if echo "$prompt" | grep -qiE "(create|add|implement|build).*(controller|model|service|migration)"; then
    backend_match=1
fi

if echo "$prompt" | grep -qiE "(create|add|make|build).*(view|partial|component|stimulus)"; then
    frontend_match=1
fi

# If any matches found, output suggestion
if [ $backend_match -eq 1 ] || [ $frontend_match -eq 1 ] || [ $skill_dev_match -eq 1 ]; then
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "🎯 SKILL ACTIVATION CHECK"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    # Critical skills (none currently)

    # High priority skills
    if [ $backend_match -eq 1 ] || [ $frontend_match -eq 1 ] || [ $skill_dev_match -eq 1 ]; then
        echo "📚 RECOMMENDED SKILLS:"
        [ $skill_dev_match -eq 1 ] && echo "  → skill-developer"
        [ $backend_match -eq 1 ] && echo "  → rails-backend-guidelines"
        [ $frontend_match -eq 1 ] && echo "  → rails-frontend-guidelines"
        echo ""
    fi

    echo "ACTION: Use Skill tool BEFORE responding"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
fi

exit 0
