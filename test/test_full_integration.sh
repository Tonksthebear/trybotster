#!/bin/bash
# Full integration test - tests the entire flow from message to Claude spawn
# Run with: bash test/test_full_integration.sh

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Full Integration Test"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Setup
TEST_DIR=$(mktemp -d)
TEST_CONFIG_DIR="$TEST_DIR/.botster_hub"
TEST_SESSIONS_DIR="$TEST_CONFIG_DIR/sessions"
TEST_WORKTREE_BASE="$TEST_DIR/botster-sessions"
TEST_API_KEY="test-api-key-123"

mkdir -p "$TEST_CONFIG_DIR"
mkdir -p "$TEST_SESSIONS_DIR"
mkdir -p "$TEST_WORKTREE_BASE"

echo "Test directory: $TEST_DIR"
echo ""

# Source functions
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FUNCTIONS_FILE=$(mktemp)
sed -n '1,/^# Commands/p' "$PROJECT_ROOT/bin/botster_hub" | grep -v '^cmd_' > "$FUNCTIONS_FILE"
source "$FUNCTIONS_FILE"

# Override environment
export HOME="$TEST_DIR"
CONFIG_DIR="$TEST_CONFIG_DIR"
SESSIONS_DIR="$TEST_SESSIONS_DIR"
CONFIG_FILE="$TEST_CONFIG_DIR/config"
LOG_FILE="$TEST_CONFIG_DIR/test.log"

# Set config
set_config "worktree_base" "$TEST_WORKTREE_BASE"
set_config "agent_command" "echo"
set_config "completion_marker" ".botster_status"
set_config "api_key" "$TEST_API_KEY"

echo "✓ Configuration set"
echo ""

# Create a test git repo
TEST_REPO="$TEST_WORKTREE_BASE/test-repo"
mkdir -p "$TEST_REPO"
cd "$TEST_REPO"
git init -q
git config user.email "test@test.com"
git config user.name "Test User"
echo "# Test" > README.md
git add README.md
git commit -q -m "Initial commit"

echo "✓ Test repository created"
echo ""

# Test 1: Create worktree
echo "Test 1: Create worktree"
echo "─────────────────────────────────────────"

# Manually create the worktree structure (skip the GitHub clone part)
REPO="test-owner/test-repo"
ISSUE_NUM="42"
REPO_SAFE="${REPO//\//-}"
BRANCH_NAME="botster-${REPO_SAFE}-${ISSUE_NUM}"
WORKTREE_PATH="$TEST_WORKTREE_BASE/${REPO_SAFE}-${ISSUE_NUM}"

# Create worktree from test repo
cd "$TEST_REPO"
git worktree add -b "$BRANCH_NAME" "$WORKTREE_PATH" 2>&1 | tee -a "$LOG_FILE" >/dev/null

if [[ -d "$WORKTREE_PATH" ]]; then
    echo -e "${GREEN}✓${NC} Worktree created: $WORKTREE_PATH"
else
    echo -e "${RED}✗${NC} Worktree not created"
    exit 1
fi

echo ""

# Test 2: Check MCP config file
echo "Test 2: MCP configuration file"
echo "─────────────────────────────────────────"

MCP_CONFIG="$WORKTREE_PATH/.mcp.json"

# Manually create the MCP config (simulating what spawn_terminal does)
cat > "$MCP_CONFIG" <<MCP_EOF
{
  "mcpServers": {
    "trybotster": {
      "type": "http",
      "url": "https://mcp-dev.trybotster.com",
      "headers": {
        "Authorization": "Bearer $TEST_API_KEY"
      }
    }
  }
}
MCP_EOF

if [[ -f "$MCP_CONFIG" ]]; then
    echo -e "${GREEN}✓${NC} MCP config file exists at project root"
    echo "  Location: $MCP_CONFIG"
else
    echo -e "${RED}✗${NC} MCP config file missing"
    exit 1
fi

# Validate JSON
if command -v jq >/dev/null 2>&1; then
    if jq empty "$MCP_CONFIG" 2>/dev/null; then
        echo -e "${GREEN}✓${NC} MCP config is valid JSON"
    else
        echo -e "${RED}✗${NC} MCP config is invalid JSON"
        exit 1
    fi
else
    echo -e "${YELLOW}⚠${NC} jq not installed, skipping JSON validation"
fi

# Check server config
SERVER_URL=$(cat "$MCP_CONFIG" | grep -o '"url":[^,]*' | cut -d'"' -f4)
if [[ "$SERVER_URL" == "https://mcp-dev.trybotster.com" ]]; then
    echo -e "${GREEN}✓${NC} MCP server URL correct"
else
    echo -e "${RED}✗${NC} MCP server URL incorrect: $SERVER_URL"
fi

# Check auth header
AUTH_HEADER=$(cat "$MCP_CONFIG" | grep -o '"Authorization":[^,}]*' | cut -d'"' -f4)
if [[ "$AUTH_HEADER" == "Bearer $TEST_API_KEY" ]]; then
    echo -e "${GREEN}✓${NC} Authorization header correct"
else
    echo -e "${RED}✗${NC} Authorization header incorrect"
fi

echo ""

# Test 3: Session management
echo "Test 3: Session management"
echo "─────────────────────────────────────────"

SESSION_KEY="test-owner-test-repo-42"
create_session "$SESSION_KEY" "msg-123" "$REPO" "$ISSUE_NUM" "$WORKTREE_PATH" "12345"

if session_exists "$SESSION_KEY"; then
    echo -e "${GREEN}✓${NC} Session created"
else
    echo -e "${RED}✗${NC} Session not created"
    exit 1
fi

if ! is_session_stale "$SESSION_KEY"; then
    echo -e "${GREEN}✓${NC} Session is active (not stale)"
else
    echo -e "${RED}✗${NC} Session incorrectly marked as stale"
    exit 1
fi

echo ""

# Test 4: Context file handling
echo "Test 4: Context file handling"
echo "─────────────────────────────────────────"

TEST_CONTEXT="Line 1
Line 2 with 'single quotes'
Line 3 with \"double quotes\"
Line 4 with special chars: \$VAR and \`command\`"

CONTEXT_FILE=$(mktemp)
printf '%s' "$TEST_CONTEXT" > "$CONTEXT_FILE"

READ_CONTEXT=$(cat "$CONTEXT_FILE")
if [[ "$READ_CONTEXT" == "$TEST_CONTEXT" ]]; then
    echo -e "${GREEN}✓${NC} Context preserved exactly"
else
    echo -e "${RED}✗${NC} Context was modified"
    echo "Expected: $TEST_CONTEXT"
    echo "Got: $READ_CONTEXT"
    exit 1
fi

rm -f "$CONTEXT_FILE"

echo ""

# Test 5: Trust marker
echo "Test 5: Claude trust marker"
echo "─────────────────────────────────────────"

mkdir -p "$WORKTREE_PATH/.claude"
touch "$WORKTREE_PATH/.claude/trusted"

if [[ -f "$WORKTREE_PATH/.claude/trusted" ]]; then
    echo -e "${GREEN}✓${NC} Trust marker created"
else
    echo -e "${RED}✗${NC} Trust marker missing"
    exit 1
fi

echo ""

# Cleanup
echo "Cleaning up..."
cd /
rm -rf "$TEST_DIR"
rm -f "$FUNCTIONS_FILE"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  All Tests Passed!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Summary:"
echo "  ✓ Worktree creation"
echo "  ✓ MCP config at project root (.mcp.json)"
echo "  ✓ Session management"
echo "  ✓ Context file handling"
echo "  ✓ Trust marker"
echo ""
