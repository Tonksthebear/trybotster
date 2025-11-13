#!/bin/bash
# Test suite for botster_hub
# Run with: bash test/botster_hub_test.sh

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Test counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Test directory setup
TEST_DIR="$(mktemp -d)"
TEST_CONFIG_DIR="$TEST_DIR/.botster_hub"
TEST_SESSIONS_DIR="$TEST_CONFIG_DIR/sessions"
TEST_CONFIG_FILE="$TEST_CONFIG_DIR/config"
TEST_LOG_FILE="$TEST_CONFIG_DIR/botster_hub.log"

mkdir -p "$TEST_SESSIONS_DIR"

# Override config directory for testing
export HOME="$TEST_DIR"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Botster Hub Test Suite"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Test directory: $TEST_DIR"
echo ""

# Create a temporary file with just the functions we need
FUNCTIONS_FILE=$(mktemp)

# Extract all function definitions from botster_hub
# Stop before the "Commands" section to avoid executing main code
sed -n '1,/^# Commands/p' bin/botster_hub | grep -v '^cmd_' > "$FUNCTIONS_FILE"

# Source the functions
source "$FUNCTIONS_FILE"

# Override directories for testing
CONFIG_DIR="$TEST_CONFIG_DIR"
SESSIONS_DIR="$TEST_SESSIONS_DIR"
CONFIG_FILE="$TEST_CONFIG_FILE"
LOG_FILE="$TEST_LOG_FILE"

#############################################################################
# Test Helpers
#############################################################################

assert_equals() {
    local expected="$1"
    local actual="$2"
    local test_name="$3"

    TESTS_RUN=$((TESTS_RUN + 1))

    if [[ "$expected" == "$actual" ]]; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        echo -e "${GREEN}✓${NC} $test_name"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        echo -e "${RED}✗${NC} $test_name"
        echo "  Expected: $expected"
        echo "  Actual:   $actual"
    fi
}

assert_true() {
    local condition="$1"
    local test_name="$2"

    TESTS_RUN=$((TESTS_RUN + 1))

    if eval "$condition"; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        echo -e "${GREEN}✓${NC} $test_name"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        echo -e "${RED}✗${NC} $test_name"
        echo "  Condition failed: $condition"
    fi
}

assert_false() {
    local condition="$1"
    local test_name="$2"

    TESTS_RUN=$((TESTS_RUN + 1))

    if ! eval "$condition"; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        echo -e "${GREEN}✓${NC} $test_name"
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        echo -e "${RED}✗${NC} $test_name"
        echo "  Condition should be false: $condition"
    fi
}

#############################################################################
# Config Tests
#############################################################################

test_config() {
    echo ""
    echo "Testing: Config Management"
    echo "─────────────────────────────────────────"

    # Test setting and getting config
    set_config "test_key" "test_value"
    local result=$(get_config "test_key")
    assert_equals "test_value" "$result" "set_config and get_config"

    # Test default value
    local result=$(get_config "nonexistent_key" "default_value")
    assert_equals "default_value" "$result" "get_config with default"
}

#############################################################################
# Session Tests
#############################################################################

test_sessions() {
    echo ""
    echo "Testing: Session Management"
    echo "─────────────────────────────────────────"

    # Test session creation
    create_session "test-repo-1" "msg-123" "owner/repo" "1" "/tmp/worktree-1" "terminal-456"
    assert_true "session_exists 'test-repo-1'" "create_session creates session file"

    # Test getting session fields
    local repo=$(get_session_field "test-repo-1" "repo")
    assert_equals "owner/repo" "$repo" "get_session_field retrieves repo"

    local issue=$(get_session_field "test-repo-1" "issue_number")
    assert_equals "1" "$issue" "get_session_field retrieves issue_number"

    # Test session removal
    remove_session "test-repo-1"
    assert_false "session_exists 'test-repo-1'" "remove_session deletes session file"
}

#############################################################################
# Stale Session Tests
#############################################################################

test_stale_sessions() {
    echo ""
    echo "Testing: Stale Session Detection"
    echo "─────────────────────────────────────────"

    # Create a test worktree
    local test_worktree="$TEST_DIR/worktrees/test-worktree"
    mkdir -p "$test_worktree"

    # Test session with existing worktree (not stale)
    create_session "test-repo-2" "msg-124" "owner/repo" "2" "$test_worktree" "terminal-789"
    assert_false "is_session_stale 'test-repo-2'" "session with worktree is not stale"

    # Test session with missing worktree (stale)
    create_session "test-repo-3" "msg-125" "owner/repo" "3" "/nonexistent/worktree" "terminal-999"
    assert_true "is_session_stale 'test-repo-3'" "session without worktree is stale"

    # Test session with completion marker (stale)
    echo "DONE" > "$test_worktree/.botster_status"
    assert_true "is_session_stale 'test-repo-2'" "session with completion marker is stale"

    # Cleanup
    rm -rf "$test_worktree"
    remove_session "test-repo-2"
    remove_session "test-repo-3"
}

#############################################################################
# JSON Parsing Tests
#############################################################################

test_json_parsing() {
    echo ""
    echo "Testing: JSON Parsing"
    echo "─────────────────────────────────────────"

    # Use JSON without spaces (as APIs typically return)
    local test_json='{"id":123,"name":"test","nested":{"key":"value"}}'

    # Test number extraction
    local id=$(json_extract_number "$test_json" "id")
    assert_equals "123" "$id" "json_extract_number extracts number"

    # Test string extraction
    local name=$(json_extract_string "$test_json" "name")
    assert_equals "test" "$name" "json_extract_string extracts string"
}

#############################################################################
# Terminal Spawn Test (Mock)
#############################################################################

test_terminal_spawn_mock() {
    echo ""
    echo "Testing: Terminal Spawn (Mock)"
    echo "─────────────────────────────────────────"

    # Create test context
    local test_context="Line 1
Line 2 with 'quotes'
Line 3 with \"double quotes\""

    # Create temp files like spawn_terminal does
    local script_file=$(mktemp)
    local context_file=$(mktemp)

    # Write context to file
    printf '%s' "$test_context" > "$context_file"

    # Verify context was written correctly
    local read_context=$(cat "$context_file")
    assert_equals "$test_context" "$read_context" "Context file preserves content exactly"

    # Create mock script
    cat > "$script_file" <<SCRIPT_EOF
#!/bin/bash
cd '/tmp/test'
export BOTSTER_API_KEY='test-key'
CONTEXT=\$(cat '$context_file')
echo "Context loaded successfully"
rm -f '$context_file' '$script_file'
SCRIPT_EOF

    chmod +x "$script_file"

    # Verify script is executable
    assert_true "[[ -x '$script_file' ]]" "Script file is executable"

    # Test script execution (without terminal)
    local output=$("$script_file" 2>&1 || true)
    assert_true "[[ '$output' == *'Context loaded successfully'* ]]" "Script executes and reads context"
}

#############################################################################
# Repo Name Sanitization Test
#############################################################################

test_repo_sanitization() {
    echo ""
    echo "Testing: Repo Name Sanitization"
    echo "─────────────────────────────────────────"

    local repo="owner/repository"
    local repo_safe="${repo//\//-}"
    assert_equals "owner-repository" "$repo_safe" "Repo name sanitizes slashes"

    local session_key="${repo_safe}-42"
    assert_equals "owner-repository-42" "$session_key" "Session key format is correct"
}

#############################################################################
# Run All Tests
#############################################################################

test_config
test_sessions
test_stale_sessions
test_json_parsing
test_terminal_spawn_mock
test_repo_sanitization

#############################################################################
# Summary
#############################################################################

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Test Results"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Total:  $TESTS_RUN"
echo -e "Passed: ${GREEN}$TESTS_PASSED${NC}"
echo -e "Failed: ${RED}$TESTS_FAILED${NC}"
echo ""

# Cleanup
rm -rf "$TEST_DIR"
rm -f "$FUNCTIONS_FILE"

if [[ $TESTS_FAILED -eq 0 ]]; then
    echo -e "${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed!${NC}"
    exit 1
fi
