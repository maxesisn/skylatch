#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

# Collect the PE import closure of the MinGW host, excluding Windows system DLLs.
engine=${1:?Usage: stage-runtime.sh HOST_ENGINE OUTPUT_DIR}
output=${2:?Usage: stage-runtime.sh HOST_ENGINE OUTPUT_DIR}
: "${MINGW_PREFIX:?Run this script in the MSYS2 build environment}"
mkdir -p "$output"

stage_imports() {
    local imports dll
    imports=$(objdump -p "$1" | awk '/DLL Name:/ {print $3}')
    for dll in $imports; do
        if [[ -f "$MINGW_PREFIX/bin/$dll" && ! -f "$output/$dll" ]]; then
            cp "$MINGW_PREFIX/bin/$dll" "$output/$dll"
            stage_imports "$output/$dll"
        fi
    done
}
stage_imports "$engine"
