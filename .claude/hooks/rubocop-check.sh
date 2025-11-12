#!/bin/bash

# Rubocop Check Hook for Rails Projects
# Runs before Stop event to catch Ruby style and lint issues

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Change to project root
cd "$CLAUDE_PROJECT_DIR" || exit 1

echo -e "${YELLOW}🔍 Running Rubocop checks...${NC}"

# Check if rubocop is available
if ! command -v rubocop &> /dev/null; then
    echo -e "${YELLOW}⚠️  Rubocop not found. Skipping checks.${NC}"
    echo -e "${YELLOW}💡 Install with: gem install rubocop or add to Gemfile${NC}"
    exit 0
fi

# Run rubocop with autocorrect for safe fixes
if rubocop --autocorrect-all --display-only-failed; then
    echo -e "${GREEN}✅ Rubocop checks passed!${NC}"
    exit 0
else
    RUBOCOP_EXIT=$?
    echo -e "${RED}❌ Rubocop found issues${NC}"
    echo -e "${YELLOW}💡 Review the issues above. Some may have been auto-corrected.${NC}"

    # Don't fail the Stop event, just warn
    exit 0
fi
