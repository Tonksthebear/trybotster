#!/bin/bash
# Test init script for integration tests
# This simulates a session initialization script without requiring claude

echo "=== Test Botster Init ==="
echo "BOTSTER_WORKTREE_PATH: $BOTSTER_WORKTREE_PATH"
echo "BOTSTER_TASK_DESCRIPTION: $BOTSTER_TASK_DESCRIPTION"
echo "BOTSTER_BRANCH_NAME: $BOTSTER_BRANCH_NAME"

# Change to worktree if set
if [ -n "$BOTSTER_WORKTREE_PATH" ]; then
    cd "$BOTSTER_WORKTREE_PATH" 2>/dev/null || echo "Could not cd to worktree"
fi

# Generate some output to test scrollback
echo ""
echo "Generating test output..."
for i in $(seq 1 100); do
    echo "Line $i: Lorem ipsum dolor sit amet, consectetur adipiscing elit"
    sleep 0.01
done

echo ""
echo "Test init complete."

# Don't exec bash for tests - that would hang
# Tests should run quickly and exit
