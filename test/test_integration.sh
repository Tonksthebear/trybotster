#!/bin/bash
# Integration test for full message processing flow
# This simulates the entire flow from message to terminal spawn
# Run with: bash test/test_integration.sh

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Integration Test - Full Message Flow"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Setup test environment
TEST_DIR=$(mktemp -d)
TEST_CONFIG_DIR="$TEST_DIR/.botster_hub"
TEST_SESSIONS_DIR="$TEST_CONFIG_DIR/sessions"
TEST_WORKTREE_BASE="$TEST_DIR/botster-sessions"
TEST_REPO_DIR="$TEST_DIR/test-repo"

mkdir -p "$TEST_SESSIONS_DIR"
mkdir -p "$TEST_WORKTREE_BASE"

echo "Test directory: $TEST_DIR"
echo "Config dir: $TEST_CONFIG_DIR"
echo "Worktree base: $TEST_WORKTREE_BASE"
echo ""

# Find project root first (before changing directories)
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Source functions from botster_hub
FUNCTIONS_FILE=$(mktemp)
sed -n '1,/^# Commands/p' "$PROJECT_ROOT/bin/botster_hub" | grep -v '^cmd_' > "$FUNCTIONS_FILE"
source "$FUNCTIONS_FILE"

# Create a test git repository to simulate the real repo
echo "Setting up test git repository..."
mkdir -p "$TEST_REPO_DIR"
cd "$TEST_REPO_DIR"
git init -q
git config user.email "test@test.com"
git config user.name "Test User"
echo "# Test Repo" > README.md
git add README.md
git commit -q -m "Initial commit"
echo "✓ Test repository created"
echo ""

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

echo "Testing: Create Worktree Function"
echo "─────────────────────────────────────────"

# Copy test repo to simulate cloned repo
CLONE_TARGET="$TEST_WORKTREE_BASE/test-owner-test-repo"
cp -r "$TEST_REPO_DIR" "$CLONE_TARGET"

cd "$CLONE_TARGET"

# Now test create_worktree
REPO="test-owner/test-repo"
ISSUE_NUM="42"

echo "Calling create_worktree with repo='$REPO' issue='$ISSUE_NUM'"
echo ""

# Capture the output of create_worktree
WORKTREE_OUTPUT=$(create_worktree "$REPO" "$ISSUE_NUM" 2>&1)
WORKTREE_EXIT_CODE=$?

echo "Exit code: $WORKTREE_EXIT_CODE"
echo "Output:"
echo "$WORKTREE_OUTPUT"
echo ""

# Check if output contains log messages
if echo "$WORKTREE_OUTPUT" | grep -q "\[2025"; then
    echo -e "${RED}✗${NC} PROBLEM: Output contains log messages!"
    echo -e "${YELLOW}This means log output is leaking to stdout and will be captured in variables${NC}"
    echo ""
fi

# Try to extract just the path
WORKTREE_PATH=$(echo "$WORKTREE_OUTPUT" | tail -1)
echo "Extracted worktree path: $WORKTREE_PATH"

# Check if it's a valid path (no log messages)
if [[ "$WORKTREE_PATH" =~ ^\[2025 ]]; then
    echo -e "${RED}✗${NC} Path still contains log messages!"
elif [[ -z "$WORKTREE_PATH" ]]; then
    echo -e "${RED}✗${NC} No path returned!"
elif [[ -d "$WORKTREE_PATH" ]]; then
    echo -e "${GREEN}✓${NC} Valid worktree path returned and exists"
else
    echo -e "${YELLOW}⚠${NC} Path returned but directory doesn't exist: $WORKTREE_PATH"
fi

echo ""

# Test 2: Simulate what happens in the script
echo "Testing: Simulated Script Execution"
echo "─────────────────────────────────────────"

SESSION_KEY="test-owner-test-repo-42"
MESSAGE_ID="msg-123"
CONTEXT="Test context message"

# This simulates the actual code in process_messages
echo "Step 1: Create worktree (simulating process_messages code)"
worktree_path=$(create_worktree "$REPO" "$ISSUE_NUM")
worktree_exit=$?

echo "Worktree path captured: '$worktree_path'"
echo "Exit code: $worktree_exit"
echo ""

if [[ $worktree_exit -ne 0 ]]; then
    echo -e "${RED}✗${NC} create_worktree failed"
else
    echo -e "${GREEN}✓${NC} create_worktree succeeded"
fi

# Check if path is valid
if [[ "$worktree_path" =~ ^\[ ]]; then
    echo -e "${RED}✗${NC} CRITICAL: Worktree path contains log messages: $worktree_path"
    echo ""
    echo "This is what causes the 'cd: ... No such file or directory' error"
    echo "The script tries to cd to the log messages instead of the actual path"
fi

echo ""

# Test 3: Test the terminal spawn with the captured path
echo "Testing: Terminal Spawn Script Generation"
echo "─────────────────────────────────────────"

# Create temp files like spawn_terminal does
script_file=$(mktemp)
context_file=$(mktemp)

printf '%s' "$CONTEXT" > "$context_file"

# This is what the script looks like
cat > "$script_file" <<SCRIPT_EOF
#!/bin/bash
echo "Attempting to cd to: '$worktree_path'"
cd '$worktree_path' || { echo "FATAL: Cannot cd to worktree: $worktree_path"; exit 1; }
echo "Successfully changed to: \$(pwd)"
CONTEXT=\$(cat '$context_file')
echo "Context loaded: \${#CONTEXT} characters"
rm -f '$context_file' '$script_file'
SCRIPT_EOF

chmod +x "$script_file"

echo "Script content:"
cat "$script_file"
echo ""

# Try to execute it
echo "Executing script..."
if "$script_file" 2>&1; then
    echo -e "${GREEN}✓${NC} Script executed successfully"
else
    echo -e "${RED}✗${NC} Script failed (this is the bug!)"
fi

echo ""

# Cleanup
echo "Cleaning up..."
cd /
rm -rf "$TEST_DIR"
rm -f "$FUNCTIONS_FILE"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Test Summary"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "This test simulates the exact flow that causes the error."
echo "If create_worktree outputs log messages to stdout, they"
echo "get captured in the worktree_path variable, causing the"
echo "'cd: No such file or directory' error."
echo ""
