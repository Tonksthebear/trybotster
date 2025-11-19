#!/bin/bash
# Quick test script for Botster Hub Rust version

set -e

echo "🦀 Botster Hub Rust - Test Script"
echo "=================================="
echo ""

# Check if cargo is installed
if ! command -v cargo &> /dev/null; then
    echo "❌ Cargo not found. Please install Rust:"
    echo "   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

echo "✅ Rust toolchain found"
echo ""

# Check compilation
echo "📦 Checking if code compiles..."
if cargo check --quiet 2>&1; then
    echo "✅ Code compiles successfully"
else
    echo "❌ Compilation failed"
    exit 1
fi
echo ""

# Build release binary
echo "🔨 Building release binary..."
cargo build --release --quiet
echo "✅ Release binary built"
echo ""

# Check binary
BINARY="./target/release/botster-hub"
if [ -f "$BINARY" ]; then
    SIZE=$(ls -lh "$BINARY" | awk '{print $5}')
    echo "✅ Binary created: $SIZE"
else
    echo "❌ Binary not found"
    exit 1
fi
echo ""

# Test binary
echo "🧪 Testing binary..."
if "$BINARY" --version &> /dev/null; then
    VERSION=$("$BINARY" --version)
    echo "✅ $VERSION"
else
    echo "❌ Binary execution failed"
    exit 1
fi
echo ""

# Test config
echo "⚙️  Testing config..."
if "$BINARY" config &> /dev/null; then
    echo "✅ Config command works"
else
    echo "❌ Config command failed"
    exit 1
fi
echo ""

# Summary
echo "=================================="
echo "🎉 All tests passed!"
echo ""
echo "Next steps:"
echo "  1. Configure: $BINARY config"
echo "  2. Edit: nano ~/.botster_hub/config.json"
echo "  3. Run: $BINARY start"
echo ""
echo "Or install globally:"
echo "  cargo install --path ."
echo "  botster-hub start"
