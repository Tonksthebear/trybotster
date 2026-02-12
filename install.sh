#!/bin/sh
# Botster installer — detects platform, downloads latest release, installs binary.
# Usage: curl -fsSL https://raw.githubusercontent.com/Tonksthebear/trybotster/main/install.sh | sh
set -e

REPO="Tonksthebear/trybotster"
BINARY_NAME="botster"

# --- Platform detection ---

detect_platform() {
  OS=$(uname -s)
  ARCH=$(uname -m)

  case "$OS" in
    Darwin) OS_NAME="macos" ;;
    Linux)  OS_NAME="linux" ;;
    *)
      echo "Error: Unsupported operating system: $OS" >&2
      echo "Botster supports macOS and Linux." >&2
      exit 1
      ;;
  esac

  case "$ARCH" in
    arm64|aarch64) ARCH_NAME="arm64" ;;
    x86_64|amd64)  ARCH_NAME="x86_64" ;;
    *)
      echo "Error: Unsupported architecture: $ARCH" >&2
      echo "Botster supports arm64 and x86_64." >&2
      exit 1
      ;;
  esac

  # Detect Rosetta 2 on macOS — prefer native arm64 binary
  if [ "$OS_NAME" = "macos" ] && [ "$ARCH_NAME" = "x86_64" ]; then
    if sysctl -n sysctl.proc_translated 2>/dev/null | grep -q 1; then
      echo "Detected Rosetta 2 — installing native arm64 binary instead."
      ARCH_NAME="arm64"
    fi
  fi

  ASSET_NAME="${BINARY_NAME}-${OS_NAME}-${ARCH_NAME}"
}

# --- Download helpers ---

download() {
  url="$1"
  output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$output" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$output" "$url"
  else
    echo "Error: curl or wget is required." >&2
    exit 1
  fi
}

# --- Checksum verification ---

verify_checksum() {
  binary_path="$1"
  checksum_path="$2"

  expected=$(awk '{print $1}' "$checksum_path")

  if command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$binary_path" | awk '{print $1}')
  elif command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$binary_path" | awk '{print $1}')
  else
    echo "Warning: Cannot verify checksum (no shasum or sha256sum found)."
    return 0
  fi

  if [ "$actual" != "$expected" ]; then
    echo "Error: Checksum verification failed!" >&2
    echo "  Expected: $expected" >&2
    echo "  Got:      $actual" >&2
    exit 1
  fi

  echo "Checksum verified."
}

# --- Install location ---

determine_install_dir() {
  # Allow override via environment variable
  if [ -n "$INSTALL_DIR" ]; then
    echo "$INSTALL_DIR"
    return
  fi

  # Default to /usr/local/bin
  echo "/usr/local/bin"
}

# --- Main ---

main() {
  detect_platform

  echo "Detected platform: ${OS_NAME}-${ARCH_NAME}"
  echo "Downloading ${ASSET_NAME}..."

  TMPDIR=$(mktemp -d)
  trap 'rm -rf "$TMPDIR"' EXIT

  LATEST_URL="https://github.com/${REPO}/releases/latest/download"

  download "${LATEST_URL}/${ASSET_NAME}" "${TMPDIR}/${BINARY_NAME}"
  download "${LATEST_URL}/${ASSET_NAME}.sha256" "${TMPDIR}/${ASSET_NAME}.sha256"

  verify_checksum "${TMPDIR}/${BINARY_NAME}" "${TMPDIR}/${ASSET_NAME}.sha256"

  chmod +x "${TMPDIR}/${BINARY_NAME}"

  INSTALL_DIR=$(determine_install_dir)
  TARGET="${INSTALL_DIR}/${BINARY_NAME}"

  # Install — use sudo if needed
  if [ -w "$INSTALL_DIR" ]; then
    mv "${TMPDIR}/${BINARY_NAME}" "$TARGET"
  else
    echo "Installing to ${INSTALL_DIR} requires elevated permissions."
    sudo mv "${TMPDIR}/${BINARY_NAME}" "$TARGET"
  fi

  # Verify it's in PATH
  if ! command -v "$BINARY_NAME" >/dev/null 2>&1; then
    echo ""
    echo "Warning: ${INSTALL_DIR} is not in your PATH."
    echo "Add it with:  export PATH=\"${INSTALL_DIR}:\$PATH\""
  fi

  VERSION=$("$TARGET" --version 2>/dev/null || echo "unknown")
  echo ""
  echo "Botster installed successfully! (${VERSION})"
  echo "Run 'botster start' to get started."
}

main
