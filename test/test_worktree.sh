#!/bin/bash
# Test git worktree operations
# Run with: bash test/test_worktree.sh

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Git Worktree Test"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Create a test directory
TEST_DIR=$(mktemp -d)
echo "Test directory: $TEST_DIR"

# Create a test git repo
TEST_REPO="$TEST_DIR/test-repo"
mkdir -p "$TEST_REPO"
cd "$TEST_REPO"
git init
echo "# Test Repo" > README.md
git add README.md
git config user.email "test@test.com"
git config user.name "Test User"
git commit -m "Initial commit"

echo "✓ Created test git repository"
echo ""

# Test 1: Create a new worktree
echo "Test 1: Creating new worktree with new branch"
echo "─────────────────────────────────────────"

WORKTREE_PATH="$TEST_DIR/worktrees/test-1"
BRANCH_NAME="test-branch-1"

if git worktree add -b "$BRANCH_NAME" "$WORKTREE_PATH" 2>&1; then
    echo -e "${GREEN}✓${NC} Created worktree: $WORKTREE_PATH"
    echo -e "${GREEN}✓${NC} Created branch: $BRANCH_NAME"

    # Verify worktree exists
    if [[ -d "$WORKTREE_PATH" ]]; then
        echo -e "${GREEN}✓${NC} Worktree directory exists"
    else
        echo -e "${RED}✗${NC} Worktree directory missing!"
    fi

    # Verify branch exists
    if git show-ref --verify --quiet "refs/heads/$BRANCH_NAME"; then
        echo -e "${GREEN}✓${NC} Branch exists in git"
    else
        echo -e "${RED}✗${NC} Branch missing from git!"
    fi
else
    echo -e "${RED}✗${NC} Failed to create worktree"
fi

echo ""

# Test 2: Reuse existing worktree
echo "Test 2: Reusing existing worktree"
echo "─────────────────────────────────────────"

if [[ -d "$WORKTREE_PATH" ]]; then
    echo -e "${GREEN}✓${NC} Worktree already exists (expected)"

    # Try to add again (should fail)
    if git worktree add "$WORKTREE_PATH" "$BRANCH_NAME" 2>&1 | grep -q "already exists"; then
        echo -e "${GREEN}✓${NC} Git correctly reports worktree already exists"
    else
        echo -e "${YELLOW}⚠${NC} Unexpected git worktree behavior"
    fi
else
    echo -e "${RED}✗${NC} Worktree should exist but doesn't!"
fi

echo ""

# Test 3: Create worktree from existing branch
echo "Test 3: Creating worktree from existing branch"
echo "─────────────────────────────────────────"

WORKTREE_PATH_2="$TEST_DIR/worktrees/test-2"

if git worktree add "$WORKTREE_PATH_2" "$BRANCH_NAME" 2>&1; then
    echo -e "${GREEN}✓${NC} Created second worktree from existing branch"

    if [[ -d "$WORKTREE_PATH_2" ]]; then
        echo -e "${GREEN}✓${NC} Second worktree directory exists"
    else
        echo -e "${RED}✗${NC} Second worktree directory missing!"
    fi
else
    echo -e "${RED}✗${NC} Failed to create worktree from existing branch"
fi

echo ""

# Test 4: List worktrees
echo "Test 4: Listing worktrees"
echo "─────────────────────────────────────────"

WORKTREE_LIST=$(git worktree list)
echo "$WORKTREE_LIST"

if echo "$WORKTREE_LIST" | grep -q "$WORKTREE_PATH"; then
    echo -e "${GREEN}✓${NC} First worktree listed"
else
    echo -e "${RED}✗${NC} First worktree not in list"
fi

if echo "$WORKTREE_LIST" | grep -q "$WORKTREE_PATH_2"; then
    echo -e "${GREEN}✓${NC} Second worktree listed"
else
    echo -e "${RED}✗${NC} Second worktree not in list"
fi

echo ""

# Test 5: Remove worktree
echo "Test 5: Removing worktree"
echo "─────────────────────────────────────────"

if git worktree remove "$WORKTREE_PATH" --force 2>&1; then
    echo -e "${GREEN}✓${NC} Removed first worktree"

    if [[ ! -d "$WORKTREE_PATH" ]]; then
        echo -e "${GREEN}✓${NC} Worktree directory cleaned up"
    else
        echo -e "${RED}✗${NC} Worktree directory still exists!"
    fi
else
    echo -e "${RED}✗${NC} Failed to remove worktree"
fi

echo ""

# Test 6: Worktree with sanitized repo name
echo "Test 6: Worktree with sanitized repo name (slash handling)"
echo "─────────────────────────────────────────"

REPO_NAME="owner/repository"
REPO_SAFE="${REPO_NAME//\//-}"
ISSUE_NUMBER="42"
BRANCH_NAME_SAFE="botster-${REPO_SAFE}-${ISSUE_NUMBER}"
WORKTREE_PATH_SAFE="$TEST_DIR/worktrees/${REPO_SAFE}-${ISSUE_NUMBER}"

echo "Original repo: $REPO_NAME"
echo "Sanitized: $REPO_SAFE"
echo "Branch: $BRANCH_NAME_SAFE"
echo "Worktree: $WORKTREE_PATH_SAFE"

if git worktree add -b "$BRANCH_NAME_SAFE" "$WORKTREE_PATH_SAFE" 2>&1; then
    echo -e "${GREEN}✓${NC} Created worktree with sanitized name"

    if [[ -d "$WORKTREE_PATH_SAFE" ]]; then
        echo -e "${GREEN}✓${NC} Worktree with sanitized path exists"
    else
        echo -e "${RED}✗${NC} Worktree directory missing!"
    fi
else
    echo -e "${RED}✗${NC} Failed to create worktree with sanitized name"
fi

echo ""

# Test 7: Trust marker file
echo "Test 7: Creating Claude trust marker"
echo "─────────────────────────────────────────"

mkdir -p "$WORKTREE_PATH_SAFE/.claude"
touch "$WORKTREE_PATH_SAFE/.claude/trusted"

if [[ -f "$WORKTREE_PATH_SAFE/.claude/trusted" ]]; then
    echo -e "${GREEN}✓${NC} Trust marker file created"
else
    echo -e "${RED}✗${NC} Trust marker file missing!"
fi

echo ""

# Cleanup
echo "Cleaning up test directory..."
cd /
rm -rf "$TEST_DIR"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Test Complete"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
