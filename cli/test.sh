#!/bin/bash
# Test script for Botster Hub
#
# IMPORTANT: Always use this script instead of running `cargo test` directly.
# This ensures BOTSTER_ENV=test is set, which prevents keyring access prompts.

set -e

# Always set test mode to prevent keyring access
export BOTSTER_ENV=test

# Parse arguments
RUN_UNIT=false
RUN_INTEGRATION=false
RUN_BUILD_CHECK=false
CARGO_ARGS=""

show_help() {
    echo "Usage: ./test.sh [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --unit, -u       Run unit tests only (cargo test --lib)"
    echo "  --integration    Run integration tests only"
    echo "  --all, -a        Run all tests (default if no option given)"
    echo "  --check, -c      Just check compilation, don't run tests"
    echo "  --help, -h       Show this help"
    echo ""
    echo "Any additional arguments are passed to cargo test."
    echo ""
    echo "Examples:"
    echo "  ./test.sh                    # Run all tests"
    echo "  ./test.sh --unit             # Run unit tests only"
    echo "  ./test.sh --unit -- scroll   # Run unit tests matching 'scroll'"
    echo "  ./test.sh --check            # Just check compilation"
}

# Default to running all tests if no args
if [ $# -eq 0 ]; then
    RUN_UNIT=true
    RUN_INTEGRATION=true
fi

while [[ $# -gt 0 ]]; do
    case $1 in
        --unit|-u)
            RUN_UNIT=true
            shift
            ;;
        --integration)
            RUN_INTEGRATION=true
            shift
            ;;
        --all|-a)
            RUN_UNIT=true
            RUN_INTEGRATION=true
            shift
            ;;
        --check|-c)
            RUN_BUILD_CHECK=true
            shift
            ;;
        --help|-h)
            show_help
            exit 0
            ;;
        --)
            shift
            CARGO_ARGS="$*"
            break
            ;;
        *)
            CARGO_ARGS="$CARGO_ARGS $1"
            shift
            ;;
    esac
done

echo "BOTSTER_ENV=$BOTSTER_ENV (keyring access disabled)"
echo ""

# Check if cargo is installed
if ! command -v cargo &> /dev/null; then
    echo "Cargo not found. Please install Rust:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

if [ "$RUN_BUILD_CHECK" = true ]; then
    echo "Checking compilation..."
    cargo check
    echo ""
    echo "Compilation OK"
    exit 0
fi

if [ "$RUN_UNIT" = true ]; then
    echo "Running unit tests..."
    cargo test --lib $CARGO_ARGS
    echo ""
fi

if [ "$RUN_INTEGRATION" = true ]; then
    # Touch Lua dir so cargo detects changes (git operations can preserve mtimes)
    touch lua/
    echo "Building release binary (required by PTY integration tests)..."
    cargo build --release
    # Clear cached Lua so the binary re-extracts fresh embedded files
    rm -f ~/.botster/lua/.version
    echo ""
    echo "Running integration tests..."
    cargo test --test '*' $CARGO_ARGS
    echo ""
fi

echo "All tests passed"
