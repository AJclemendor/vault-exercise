#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

cleanup() {
    echo ""
    echo "Shutting down..."
    [[ -n "${HARNESS_PID:-}" ]] && kill "$HARNESS_PID" 2>/dev/null
    [[ -n "${ANVIL_PID:-}" ]] && kill "$ANVIL_PID" 2>/dev/null
    wait 2>/dev/null
}
trap cleanup EXIT

# 1. Start anvil in the background
echo "Starting anvil..."
anvil --silent &
ANVIL_PID=$!
sleep 1
if ! kill -0 "$ANVIL_PID" 2>/dev/null; then
    echo "anvil failed to start"
    exit 1
fi
echo "anvil running (pid $ANVIL_PID)"

# 2. Deploy contracts
echo "Running setup..."
bash "$SCRIPT_DIR/setup.sh"
echo ""

# 3. Build service and harness in parallel
echo "Building service and harness..."
cargo build --manifest-path "$ROOT_DIR/service/Cargo.toml" &
BUILD_SERVICE_PID=$!
cargo build --manifest-path "$ROOT_DIR/harness/Cargo.toml" &
BUILD_HARNESS_PID=$!
wait "$BUILD_SERVICE_PID" "$BUILD_HARNESS_PID"

# 4. Start harness in the background
cargo run --manifest-path "$ROOT_DIR/harness/Cargo.toml" &>/dev/null &
HARNESS_PID=$!
echo "harness running (pid $HARNESS_PID)"

# 5. Run service in the foreground
echo "Starting service..."
echo ""
cargo run --manifest-path "$ROOT_DIR/service/Cargo.toml"
