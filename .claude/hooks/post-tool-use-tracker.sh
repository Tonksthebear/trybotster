#!/bin/bash
set -e

# Post-tool-use hook that tracks edited files in Rails projects
# This runs after Edit, MultiEdit, or Write tools complete successfully

# Read tool information from stdin
tool_info=$(cat)

# Extract relevant data
tool_name=$(echo "$tool_info" | jq -r '.tool_name // empty')
file_path=$(echo "$tool_info" | jq -r '.tool_input.file_path // empty')
session_id=$(echo "$tool_info" | jq -r '.session_id // empty')

# Skip if not an edit tool or no file path
if [[ ! "$tool_name" =~ ^(Edit|MultiEdit|Write)$ ]] || [[ -z "$file_path" ]]; then
    exit 0
fi

# Skip markdown files
if [[ "$file_path" =~ \.(md|markdown)$ ]]; then
    exit 0
fi

# Create cache directory in project
cache_dir="$CLAUDE_PROJECT_DIR/.claude/cache/${session_id:-default}"
mkdir -p "$cache_dir"

# Function to detect Rails component from file path
detect_rails_component() {
    local file="$1"
    local project_root="$CLAUDE_PROJECT_DIR"

    # Remove project root from path
    local relative_path="${file#$project_root/}"

    # Detect Rails component type
    case "$relative_path" in
        # Controllers
        app/controllers/*)
            echo "controllers"
            ;;
        # Models
        app/models/*)
            echo "models"
            ;;
        # Views
        app/views/*)
            echo "views"
            ;;
        # JavaScript/Stimulus
        app/javascript/*)
            echo "javascript"
            ;;
        # Services
        app/services/*)
            echo "services"
            ;;
        # Jobs
        app/jobs/*)
            echo "jobs"
            ;;
        # Mailers
        app/mailers/*)
            echo "mailers"
            ;;
        # Channels (Action Cable)
        app/channels/*)
            echo "channels"
            ;;
        # Components (ViewComponent)
        app/components/*)
            echo "components"
            ;;
        # Helpers
        app/helpers/*)
            echo "helpers"
            ;;
        # Config
        config/*)
            echo "config"
            ;;
        # Database migrations
        db/migrate/*)
            echo "migrations"
            ;;
        # Database schema
        db/schema.rb)
            echo "schema"
            ;;
        # Lib
        lib/*)
            echo "lib"
            ;;
        # Specs/Tests
        spec/*|test/*)
            echo "tests"
            ;;
        # Assets
        app/assets/*)
            echo "assets"
            ;;
        # Root level Ruby files
        *.rb)
            if [[ ! "$relative_path" =~ / ]]; then
                echo "root"
            else
                echo "unknown"
            fi
            ;;
        *)
            echo "unknown"
            ;;
    esac
}

# Function to get suggested checks for Rails components
get_rails_checks() {
    local component="$1"
    local file_path="$2"

    case "$component" in
        controllers|models|services|jobs|mailers|channels|helpers|lib)
            # Ruby files that should be checked with RuboCop
            echo "rubocop"
            ;;
        migrations)
            # Migrations - suggest running migrations
            echo "check_migrations"
            ;;
        javascript)
            # JavaScript files - could add ESLint check in future
            echo "javascript_check"
            ;;
        views|components)
            # View files - could check ERB syntax
            echo "view_check"
            ;;
        config)
            # Config files - especially routes
            if [[ "$file_path" =~ routes\.rb$ ]]; then
                echo "check_routes"
            fi
            ;;
        schema)
            # Schema changed - probably need to check migrations
            echo "check_schema"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Detect component
component=$(detect_rails_component "$file_path")

# Skip if unknown component
if [[ "$component" == "unknown" ]] || [[ -z "$component" ]]; then
    exit 0
fi

# Log edited file with timestamp
echo "$(date +%s):$file_path:$component" >> "$cache_dir/edited-files.log"

# Update affected components list
if ! grep -q "^$component$" "$cache_dir/affected-components.txt" 2>/dev/null; then
    echo "$component" >> "$cache_dir/affected-components.txt"
fi

# Store suggested checks
checks=$(get_rails_checks "$component" "$file_path")
if [[ -n "$checks" ]]; then
    echo "$component:$checks:$file_path" >> "$cache_dir/checks.txt.tmp"
fi

# Remove duplicates from checks
if [[ -f "$cache_dir/checks.txt.tmp" ]]; then
    sort -u "$cache_dir/checks.txt.tmp" > "$cache_dir/checks.txt"
    rm -f "$cache_dir/checks.txt.tmp"
fi

# Exit cleanly
exit 0
