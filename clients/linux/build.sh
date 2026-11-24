#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

skip_rust=0
if [[ "${1:-}" == "--skip-rust" ]]; then
    skip_rust=1
    shift
fi
if [[ $# -ne 0 ]]; then
    echo 'Usage: build.sh [--skip-rust]' >&2
    exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib_dir="${SKYLATCH_CORE_LIB_DIR:-$here/../../target/release}"
if [[ "$lib_dir" != /* ]]; then lib_dir="$PWD/$lib_dir"; fi
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
make -C "$here" SKIP_RUST="$skip_rust" CORE_LIB_DIR="$lib_dir"
