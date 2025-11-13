#!/bin/bash
# Test session management edge cases
# Run with: bash test/test_session_management.sh

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Session Management Test"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Test directory setup
TEST_DIR="$(mktemp -d)"
TEST_CONFIG_DIR="$TEST_DIR/.botster_hub"
TEST_SESSIONS_DIR="$TEST_CONFIG_DIR/sessions"
TEST_WORKTREES_DIR="$TEST_DIR/worktrees"

mkdir -p "$TEST_SESSIONS_DIR"
mkdir -p "$TEST_WORKTREES_DIR"

export HOME="$TEST_DIR"

echo "Test directory: $TEST_DIR"
echo "Sessions dir: $TEST_SESSIONS_DIR"
echo "Worktrees dir: $TEST_WORKTREES_DIR"
echo ""

# Source functions from botster_hub
FUNCTIONS_FILE=$(mktemp)
sed -n '1,/^# Commands/p' bin/botster_hub | grep -v '^cmd_' > "$FUNCTIONS_FILE"
source "$FUNCTIONS_FILE"

# Override directories
CONFIG_DIR="$TEST_CONFIG_DIR"
SESSIONS_DIR="$TEST_SESSIONS_DIR"
CONFIG_FILE="$TEST_CONFIG_DIR/config"
LOG_FILE="$TEST_CONFIG_DIR/test.log"

# Test 1: Create session
echo "Test 1: Create session"
echo "─────────────────────────────────────────"

SESSION_KEY="test-repo-1"
MESSAGE_ID="msg-123"
REPO="owner/repo"
ISSUE_NUM="1"
WORKTREE="$TEST_WORKTREES_DIR/test-1"
TERMINAL_ID="12345"

mkdir -p "$WORKTREE"

create_session "$SESSION_KEY" "$MESSAGE_ID" "$REPO" "$ISSUE_NUM" "$WORKTREE" "$TERMINAL_ID"

if session_exists "$SESSION_KEY"; then
    echo -e "${GREEN}✓${NC} Session created"
else
    echo -e "${RED}✗${NC} Session not created"
fi

# Verify fields
STORED_REPO=$(get_session_field "$SESSION_KEY" "repo")
STORED_ISSUE=$(get_session_field "$SESSION_KEY" "issue_number")
STORED_WORKTREE=$(get_session_field "$SESSION_KEY" "worktree_path")
STORED_TERMINAL=$(get_session_field "$SESSION_KEY" "terminal_id")

if [[ "$STORED_REPO" == "$REPO" ]]; then
    echo -e "${GREEN}✓${NC} Repo field stored correctly"
else
    echo -e "${RED}✗${NC} Repo field incorrect: $STORED_REPO"
fi

if [[ "$STORED_ISSUE" == "$ISSUE_NUM" ]]; then
    echo -e "${GREEN}✓${NC} Issue number stored correctly"
else
    echo -e "${RED}✗${NC} Issue number incorrect: $STORED_ISSUE"
fi

if [[ "$STORED_WORKTREE" == "$WORKTREE" ]]; then
    echo -e "${GREEN}✓${NC} Worktree path stored correctly"
else
    echo -e "${RED}✗${NC} Worktree path incorrect: $STORED_WORKTREE"
fi

if [[ "$STORED_TERMINAL" == "$TERMINAL_ID" ]]; then
    echo -e "${GREEN}✓${NC} Terminal ID stored correctly"
else
    echo -e "${RED}✗${NC} Terminal ID incorrect: $STORED_TERMINAL"
fi

echo ""

# Test 2: Session with missing worktree (stale)
echo "Test 2: Session with missing worktree"
echo "─────────────────────────────────────────"

SESSION_KEY_2="test-repo-2"
WORKTREE_2="$TEST_WORKTREES_DIR/nonexistent"

create_session "$SESSION_KEY_2" "msg-124" "owner/repo" "2" "$WORKTREE_2" "67890"

if is_session_stale "$SESSION_KEY_2"; then
    echo -e "${GREEN}✓${NC} Session correctly detected as stale (missing worktree)"
else
    echo -e "${RED}✗${NC} Session should be stale but isn't"
fi

echo ""

# Test 3: Session with completion marker (stale)
echo "Test 3: Session with completion marker"
echo "─────────────────────────────────────────"

SESSION_KEY_3="test-repo-3"
WORKTREE_3="$TEST_WORKTREES_DIR/test-3"

mkdir -p "$WORKTREE_3"
create_session "$SESSION_KEY_3" "msg-125" "owner/repo" "3" "$WORKTREE_3" "11111"

# Add completion marker
echo "DONE" > "$WORKTREE_3/.botster_status"

if is_session_stale "$SESSION_KEY_3"; then
    echo -e "${GREEN}✓${NC} Session correctly detected as stale (has completion marker)"
else
    echo -e "${RED}✗${NC} Session should be stale but isn't"
fi

echo ""

# Test 4: Active session (not stale)
echo "Test 4: Active session (not stale)"
echo "─────────────────────────────────────────"

SESSION_KEY_4="test-repo-4"
WORKTREE_4="$TEST_WORKTREES_DIR/test-4"

mkdir -p "$WORKTREE_4"
create_session "$SESSION_KEY_4" "msg-126" "owner/repo" "4" "$WORKTREE_4" "22222"

if ! is_session_stale "$SESSION_KEY_4"; then
    echo -e "${GREEN}✓${NC} Session correctly detected as active (not stale)"
else
    echo -e "${RED}✗${NC} Session should not be stale but is"
fi

echo ""

# Test 5: List sessions
echo "Test 5: List all sessions"
echo "─────────────────────────────────────────"

ALL_SESSIONS=$(list_sessions)
SESSION_COUNT=$(echo "$ALL_SESSIONS" | wc -l | tr -d ' ')

echo "Found $SESSION_COUNT sessions:"
echo "$ALL_SESSIONS"

if [[ "$SESSION_COUNT" -ge 4 ]]; then
    echo -e "${GREEN}✓${NC} All sessions listed"
else
    echo -e "${RED}✗${NC} Expected 4 sessions, found $SESSION_COUNT"
fi

echo ""

# Test 6: Remove session
echo "Test 6: Remove session"
echo "─────────────────────────────────────────"

remove_session "$SESSION_KEY"

if ! session_exists "$SESSION_KEY"; then
    echo -e "${GREEN}✓${NC} Session removed successfully"
else
    echo -e "${RED}✗${NC} Session still exists after removal"
fi

echo ""

# Test 7: Count sessions
echo "Test 7: Count sessions"
echo "─────────────────────────────────────────"

COUNT=$(count_sessions)
echo "Active sessions: $COUNT"

if [[ "$COUNT" == "3" ]]; then
    echo -e "${GREEN}✓${NC} Session count correct (3 remaining after removal)"
else
    echo -e "${RED}✗${NC} Expected 3 sessions, counted $COUNT"
fi

echo ""

# Test 8: Session with special characters in repo name
echo "Test 8: Session with special characters"
echo "─────────────────────────────────────────"

REPO_WITH_SLASH="owner/my-repo-name"
REPO_SAFE="${REPO_WITH_SLASH//\//-}"
SESSION_KEY_SAFE="${REPO_SAFE}-99"

echo "Original repo: $REPO_WITH_SLASH"
echo "Sanitized session key: $SESSION_KEY_SAFE"

WORKTREE_SAFE="$TEST_WORKTREES_DIR/$SESSION_KEY_SAFE"
mkdir -p "$WORKTREE_SAFE"

create_session "$SESSION_KEY_SAFE" "msg-999" "$REPO_WITH_SLASH" "99" "$WORKTREE_SAFE" "99999"

if session_exists "$SESSION_KEY_SAFE"; then
    echo -e "${GREEN}✓${NC} Session with sanitized key created"

    STORED_REPO_ORIG=$(get_session_field "$SESSION_KEY_SAFE" "repo")
    if [[ "$STORED_REPO_ORIG" == "$REPO_WITH_SLASH" ]]; then
        echo -e "${GREEN}✓${NC} Original repo name preserved in session"
    else
        echo -e "${RED}✗${NC} Repo name not preserved: $STORED_REPO_ORIG"
    fi
else
    echo -e "${RED}✗${NC} Session with sanitized key not created"
fi

echo ""

# Test 9: Session file corruption handling
echo "Test 9: Corrupt session file handling"
echo "─────────────────────────────────────────"

SESSION_KEY_CORRUPT="corrupt-session"
SESSION_FILE="$TEST_SESSIONS_DIR/$SESSION_KEY_CORRUPT"

# Create a corrupt session file
echo "garbage data" > "$SESSION_FILE"
echo "more garbage" >> "$SESSION_FILE"

if session_exists "$SESSION_KEY_CORRUPT"; then
    echo -e "${GREEN}✓${NC} Corrupt session file exists"

    # Try to read fields from corrupt session
    CORRUPT_REPO=$(get_session_field "$SESSION_KEY_CORRUPT" "repo" "default")
    if [[ "$CORRUPT_REPO" == "default" ]]; then
        echo -e "${GREEN}✓${NC} Corrupt session returns default value"
    else
        echo -e "${YELLOW}⚠${NC} Unexpected value from corrupt session: $CORRUPT_REPO"
    fi
else
    echo -e "${RED}✗${NC} Corrupt session file not found"
fi

echo ""

# Cleanup
echo "Cleaning up..."
rm -rf "$TEST_DIR"
rm -f "$FUNCTIONS_FILE"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Test Complete"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
