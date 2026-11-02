#!/bin/bash
# Build agentgate for all platforms
# Usage: ./build-all-platforms.sh
set -e

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="$PROJECT_DIR/releases"
mkdir -p "$OUT_DIR"

echo "=== Building agentgate for all platforms ==="

# 1. Linux x86_64 (native)
echo ""
echo "[1/3] Building Linux x86_64..."
cargo build --release --target x86_64-unknown-linux-gnu
tar -czf "$OUT_DIR/agentgate-linux-x86_64.tar.gz" \
    -C "$PROJECT_DIR/target/x86_64-unknown-linux-gnu/release" agentgate
echo "  ✓ agentgate-linux-x86_64.tar.gz"

# 2. ARM64 (cross-compile)
echo ""
echo "[2/3] Building ARM64..."
cargo build --release --target aarch64-unknown-linux-gnu
tar -czf "$OUT_DIR/agentgate-linux-aarch64.tar.gz" \
    -C "$PROJECT_DIR/target/aarch64-unknown-linux-gnu/release" agentgate
echo "  ✓ agentgate-linux-aarch64.tar.gz"

# 3. Windows x86_64 (cross-compile)
echo ""
echo "[3/3] Building Windows x86_64..."
cargo build --release --target x86_64-pc-windows-gnu
tar -czf "$OUT_DIR/agentgate-windows-x86_64.tar.gz" \
    -C "$PROJECT_DIR/target/x86_64-pc-windows-gnu/release" agentgate.exe
echo "  ✓ agentgate-windows-x86_64.tar.gz"

echo ""
echo "=== Build Complete ==="
ls -lh "$OUT_DIR"