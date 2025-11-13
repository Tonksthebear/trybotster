#!/bin/bash
# Pure bash skill activation - no dependencies!

# Get the directory where this script lives
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Execute the simple bash implementation
exec "$SCRIPT_DIR/skill-activation-prompt-simple.sh"
