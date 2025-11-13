#!/bin/bash
# Quick test for terminal spawning
# This creates a temporary script and tests if it can be spawned in Terminal.app
# Run with: bash test/test_terminal_spawn.sh

set -euo pipefail

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Terminal Spawn Test"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Test context with challenging characters
TEST_CONTEXT="You have been mentioned in a GitHub issue. Your task is to:
1. Review the issue details below
2. Investigate and understand the problem
3. Implement a solution if needed
4. Either submit a Pull Request with the fix OR post a comment with your findings/answer

IMPORTANT: Only use the 'trybotster' MCP server if it exists in your configuration.
If the trybotster MCP server is not available, respond with a comment explaining
that you cannot interact with GitHub and the user should check their MCP server configuration.

Issue: Test Issue with 'single quotes' and \"double quotes\"

Description:
This is a test issue to verify terminal spawning works correctly.

Comment that mentioned you:
@trybotster please just answer

Please proceed to address this issue."

echo "Creating test files..."

# Create temporary files
SCRIPT_FILE=$(mktemp)
CONTEXT_FILE=$(mktemp)
MARKER_FILE=$(mktemp)

echo "Script file: $SCRIPT_FILE"
echo "Context file: $CONTEXT_FILE"
echo "Marker file: $MARKER_FILE"
echo ""

# Write context to file
printf '%s' "$TEST_CONTEXT" > "$CONTEXT_FILE"

# Create test script
cat > "$SCRIPT_FILE" <<SCRIPT_EOF
#!/bin/bash
echo "═══════════════════════════════════════"
echo "  Botster Hub Terminal Spawn Test"
echo "═══════════════════════════════════════"
echo ""
echo "Working directory: \$(pwd)"
echo ""
echo "Reading context from file..."
CONTEXT=\$(cat '$CONTEXT_FILE')
echo ""
echo "Context received (first 100 chars):"
echo "\${CONTEXT:0:100}..."
echo ""
echo "Context length: \${#CONTEXT} characters"
echo ""
echo "Simulating agent execution..."
echo "claude \"\$CONTEXT\""
echo ""
echo "SUCCESS: Terminal spawn test completed!"
echo "DONE" > '$MARKER_FILE'
echo ""
echo "Cleaning up temp files..."
rm -f '$CONTEXT_FILE' '$SCRIPT_FILE'
echo ""
echo "Test completed. You can close this terminal."
echo "Press Enter to exit..."
read
SCRIPT_EOF

chmod +x "$SCRIPT_FILE"

echo "Spawning terminal with AppleScript..."
echo ""

# Spawn terminal
TERMINAL_ID=$(osascript <<EOF 2>&1
tell application "Terminal"
    activate
    set newWindow to do script "$SCRIPT_FILE"
    set custom title of newWindow to "Botster Test"
    return id of front window
end tell
EOF
)

EXIT_CODE=$?

if [[ $EXIT_CODE -eq 0 ]]; then
    echo "✓ Terminal spawned successfully!"
    echo "  Terminal ID: $TERMINAL_ID"
    echo ""
    echo "Check the new terminal window for test output."
    echo ""
    echo "Waiting 10 seconds for test to complete..."
    sleep 10

    # Check if marker file was created
    if [[ -f "$MARKER_FILE" ]]; then
        echo "✓ Test completed successfully (marker file found)"
        MARKER_CONTENT=$(cat "$MARKER_FILE")
        echo "  Marker content: $MARKER_CONTENT"
        rm -f "$MARKER_FILE"
    else
        echo "⚠ Test may still be running (marker file not found yet)"
        echo "  Check the terminal window for status"
    fi
else
    echo "✗ Failed to spawn terminal!"
    echo "  Error: $TERMINAL_ID"
    echo ""
    echo "Cleaning up temp files..."
    rm -f "$SCRIPT_FILE" "$CONTEXT_FILE" "$MARKER_FILE"
    exit 1
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Test Complete"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
