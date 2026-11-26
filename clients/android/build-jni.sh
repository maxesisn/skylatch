#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Cross-compiles clients/android/jni into app/src/main/jniLibs/<abi>/libskylatch_android.so
# with the Android NDK (API 26, 16 KB ELF page alignment for Android 15+).
#
#   clients/android/build-jni.sh [--abi arm64-v8a|x86_64|all] [--release|--debug]
#
# NDK lookup: $ANDROID_NDK_HOME, $ANDROID_NDK_ROOT, newest $ANDROID_HOME/ndk/* (or
# $ANDROID_SDK_ROOT/ndk/*), then /Volumes/MaxStore/offload/android-ndk-r27c.
set -euo pipefail

API_LEVEL=26
PAGE_SIZE=16384
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="$HERE/jni"
JNI_LIBS="$HERE/app/src/main/jniLibs"

abi=arm64-v8a
profile=release
while [ $# -gt 0 ]; do
  case "$1" in
    --abi) abi="${2:?--abi needs a value}"; shift 2 ;;
    --abi=*) abi="${1#--abi=}"; shift ;;
    --release) profile=release; shift ;;
    --debug) profile=debug; shift ;;
    -h|--help) sed -n '4,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$abi" in
  arm64-v8a|x86_64) abis=("$abi") ;;
  all) abis=(arm64-v8a x86_64) ;;
  *) echo "unsupported ABI: $abi (arm64-v8a, x86_64 or all)" >&2; exit 2 ;;
esac

find_ndk() {
  local candidate sdk
  for candidate in "${ANDROID_NDK_HOME:-}" "${ANDROID_NDK_ROOT:-}"; do
    [ -n "$candidate" ] && [ -f "$candidate/build/cmake/android.toolchain.cmake" ] && { echo "$candidate"; return; }
  done
  for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}"; do
    [ -n "$sdk" ] && [ -d "$sdk/ndk" ] || continue
    candidate="$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
    [ -n "$candidate" ] && [ -f "$candidate/build/cmake/android.toolchain.cmake" ] && { echo "$candidate"; return; }
  done
  candidate=/Volumes/MaxStore/offload/android-ndk-r27c
  [ -f "$candidate/build/cmake/android.toolchain.cmake" ] && { echo "$candidate"; return; }
  return 1
}

NDK="$(find_ndk)" || { echo "Android NDK not found; set ANDROID_NDK_HOME" >&2; exit 1; }
case "$(uname -s)" in
  Darwin) host_tag=darwin-x86_64 ;;
  Linux) host_tag=linux-x86_64 ;;
  *) echo "unsupported build host: $(uname -s)" >&2; exit 1 ;;
esac
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$host_tag"
[ -d "$TOOLCHAIN/bin" ] || { echo "NDK toolchain missing: $TOOLCHAIN" >&2; exit 1; }
echo "NDK: $NDK ($(sed -n 's/^Pkg.Revision *= *//p' "$NDK/source.properties" 2>/dev/null))"

# opusic-sys builds libopus with CMake and reads these to select the NDK toolchain file.
export ANDROID_NDK_HOME="$NDK" ANDROID_NDK_ROOT="$NDK" ANDROID_PLATFORM="android-$API_LEVEL"
command -v ninja >/dev/null || [ ! -x /opt/homebrew/bin/ninja ] || export PATH="/opt/homebrew/bin:$PATH"

profile_flag=()
[ "$profile" = release ] && profile_flag=(--release)

for abi in "${abis[@]}"; do
  case "$abi" in
    arm64-v8a) target=aarch64-linux-android ;;
    x86_64) target=x86_64-linux-android ;;
  esac
  rustup target list --installed 2>/dev/null | grep -qx "$target" || rustup target add "$target"

  target_env="${target//-/_}"
  target_upper="$(echo "$target_env" | tr '[:lower:]' '[:upper:]')"
  clang="$TOOLCHAIN/bin/$target$API_LEVEL-clang"
  [ -x "$clang" ] || { echo "missing NDK compiler: $clang" >&2; exit 1; }
  export "CC_$target_env=$clang"
  export "CXX_$target_env=$clang++"
  export "AR_$target_env=$TOOLCHAIN/bin/llvm-ar"
  export "RANLIB_$target_env=$TOOLCHAIN/bin/llvm-ranlib"
  export "CMAKE_TOOLCHAIN_FILE_$target_env=$NDK/build/cmake/android.toolchain.cmake"
  export "CARGO_TARGET_${target_upper}_LINKER=$clang"
  export "CARGO_TARGET_${target_upper}_RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=$PAGE_SIZE"
  export ANDROID_ABI="$abi"

  echo "==> $abi ($target, $profile)"
  cargo build --manifest-path "$CRATE/Cargo.toml" --target "$target" --lib "${profile_flag[@]}"

  built="$CRATE/target/$target/$profile/libskylatch_android.so"
  out="$JNI_LIBS/$abi/libskylatch_android.so"
  mkdir -p "$(dirname "$out")"
  cp "$built" "$out"

  echo "    $out"
  echo "    size: $(wc -c <"$out" | tr -d ' ') bytes"
  echo "    file: $(file -b "$out")"
  exports="$("$TOOLCHAIN/bin/llvm-nm" -D --defined-only "$out" | grep -c ' Java_io_github_maxesisn_skylatch_NativeBridge_' || true)"
  echo "    JNI exports: $exports"
  [ "$exports" -gt 0 ] || { echo "no NativeBridge JNI exports in $out" >&2; exit 1; }

  echo "    PT_LOAD alignment:"
  loads=0
  while read -r line; do
    echo "      $line"
    align="${line##* }"
    loads=$((loads + 1))
    [ $((align)) -ge "$PAGE_SIZE" ] || { echo "LOAD segment aligned to $align, below $PAGE_SIZE" >&2; exit 1; }
  done < <("$TOOLCHAIN/bin/llvm-readelf" -lW "$out" | awk '$1 == "LOAD"')
  [ "$loads" -gt 0 ] || { echo "no LOAD segments in $out" >&2; exit 1; }
  echo "    16 KB page alignment: OK ($loads LOAD segments)"
done
