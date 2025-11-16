#!/bin/bash
#
# Test script for spawning Claude Code sessions without webhooks
# This simulates the full flow: create bot message → daemon processes → spawn terminal
#

set -e

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}=== Botster Spawn Test ===${NC}"
echo

# Get the project root directory
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

# Check if Rails is running
if ! lsof -ti:3000 > /dev/null 2>&1; then
    echo -e "${RED}✗ Rails server not running on port 3000${NC}"
    echo "  Start it with: bin/dev"
    exit 1
fi

echo -e "${GREEN}✓ Rails server is running${NC}"

# Check if botster_hub is running (Ruby or Rust version)
if pgrep -f "bin/botster_hub" > /dev/null || pgrep -f "botster-hub" > /dev/null; then
    if pgrep -f "botster-hub" > /dev/null; then
        echo -e "${GREEN}✓ botster_hub daemon is running (Rust version)${NC}"
    else
        echo -e "${GREEN}✓ botster_hub daemon is running (Ruby version)${NC}"
    fi
else
    echo -e "${RED}✗ botster_hub daemon not running${NC}"
    echo "  Start with Ruby: bin/botster_hub start"
    echo "  Or start with Rust: cd botster_hub_rs && ./target/release/botster-hub start"
    exit 1
fi
echo

# Get test parameters (or use defaults)
REPO="${1:-Tonksthebear/trybotster}"
ISSUE_NUMBER="${2:-6}"

echo "Test parameters:"
echo "  Repository: $REPO"
echo "  Issue Number: $ISSUE_NUMBER"
echo

# Create a test bot message directly in the database
echo "Creating test bot message..."

# Create temporary Ruby script
TEMP_SCRIPT=$(mktemp /tmp/botster_test.XXXXXX.rb)

cat > "$TEMP_SCRIPT" <<'RUBY_SCRIPT'
context = "You have been mentioned in a GitHub issue.

Repository: #{ENV['TEST_REPO']}
Issue Number: ##{ENV['TEST_ISSUE_NUMBER']}

Your task is to:
1. Use the trybotster MCP server to fetch the issue details
2. Review and understand the problem
3. Investigate the codebase if needed
4. Implement a solution if appropriate
5. Either submit a Pull Request with the fix OR post a comment with your findings/answer

IMPORTANT:
- Use the trybotster MCP tools to interact with GitHub (fetch issue, post comments, create PRs)
- The trybotster MCP server is already configured in this project
- If you cannot access the trybotster MCP server, explain that you need it to interact with GitHub

Start by fetching the issue details using the trybotster MCP server."

message = Bot::Message.create!(
  event_type: 'github_mention',
  payload: {
    repo: ENV['TEST_REPO'],
    issue_number: ENV['TEST_ISSUE_NUMBER'].to_i,
    comment_id: 999999,
    comment_body: '@trybotster test spawn',
    comment_author: 'test-user',
    issue_title: 'Test Issue',
    issue_body: 'Test issue body',
    issue_url: "https://github.com/#{ENV['TEST_REPO']}/issues/#{ENV['TEST_ISSUE_NUMBER']}",
    is_pr: false,
    context: context
  }
)

puts "Created Bot::Message #{message.id}"
puts "Status: #{message.status}"
puts "Claimed: #{message.claimed? ? 'yes' : 'no'}"
RUBY_SCRIPT

# Run with environment variables
TEST_REPO="$REPO" TEST_ISSUE_NUMBER="$ISSUE_NUMBER" rails runner "$TEMP_SCRIPT"
RESULT=$?

# Cleanup
rm -f "$TEMP_SCRIPT"

if [ $RESULT -eq 0 ]; then
    echo -e "${GREEN}✓ Bot message created successfully${NC}"
else
    echo -e "${RED}✗ Failed to create bot message${NC}"
    exit 1
fi

echo
echo "Waiting for botster_hub to process the message..."
echo "(Watch for a new Terminal window to spawn)"
echo
echo "Press Ctrl+C to stop watching..."

# Watch the logs for spawn activity
tail -f log/botster_hub.log 2>/dev/null || echo "Note: No log file yet"
