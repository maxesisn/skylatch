#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Builds skylatch-client-core (Rust) and the native macOS client, producing clients/macos/skylatch-client.
# The Rust core is linked statically, so the binary runs from anywhere without an rpath or a copied dylib.
set -euo pipefail

SKIP_RUST=0
if [[ "${1:-}" == "--skip-rust" ]]; then
    SKIP_RUST=1
    shift
fi
if [[ $# -ne 0 ]]; then
    echo "Usage: build.sh [--skip-rust]" >&2
    exit 2
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
LIB_DIR="${SKYLATCH_CORE_LIB_DIR:-$REPO/target/release}"
# Keep the Rust objects and the Swift package on the same minimum macOS version.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

if [[ $SKIP_RUST -eq 0 ]]; then
    cargo build --release -p skylatch-client-core --manifest-path "$REPO/Cargo.toml" --target-dir "$REPO/target"
fi

if [[ ! -f "$LIB_DIR/libskylatch_client_core.a" ]]; then
    echo "Missing $LIB_DIR/libskylatch_client_core.a. Run build.sh without --skip-rust." >&2
    exit 1
fi

cd "$HERE"
export SKYLATCH_CORE_LIB_DIR="$LIB_DIR"
swift build -c release --package-path "$HERE"
BIN_DIR="$(swift build -c release --package-path "$HERE" --show-bin-path)"
cp -f "$BIN_DIR/skylatch-macos-client" "$HERE/skylatch-client"
echo "Built $HERE/skylatch-client"
