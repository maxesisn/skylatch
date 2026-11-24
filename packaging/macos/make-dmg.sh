#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo 'Usage: make-dmg.sh VERSION ARCH BIN_DIR [OUTPUT_DIR]' >&2
    exit 2
fi
version=${1#v}
arch=$2
bin_dir=$3
output=${4:-dist}
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ || ! $arch =~ ^(arm64|x86_64)$ ]]; then
    echo 'Expected VERSION like 1.2.3 and ARCH of arm64 or x86_64.' >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/skylatch-dmg.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
app="$work/image/Skylatch.app"
macos="$app/Contents/MacOS"
frameworks="$app/Contents/Frameworks"
resources="$app/Contents/Resources"
iconset="$work/Skylatch.iconset"
mkdir -p "$macos" "$frameworks" "$resources" "$iconset" "$output"

# Use the highest deployment target among the binaries and their bundled libraries.
min_version=12.3
update_min_version() {
    local versions newest
    versions=$(otool -arch "$arch" -l "$1" | awk '
        $1 == "cmd" { build = ($2 == "LC_BUILD_VERSION"); legacy = ($2 == "LC_VERSION_MIN_MACOSX") }
        (build && $1 == "minos") || (legacy && $1 == "version") { print $2 }
    ')
    newest=$(printf '%s\n%s\n' "$min_version" "$versions" | awk -F. '
        NF { v = $1 * 1000000 + $2 * 1000 + $3; if (v > max) { max = v; version = $0 } }
        END { print version }
    ')
    min_version=$newest
}

# Replace absolute non-system install names with paths relative to the bundle.
# Recurse through dylib imports so the installed app does not need Homebrew.
bundle_imports() {
    local file=$1 prefix=$2 imports dep base bundled own_id
    update_min_version "$file"
    own_id=$(otool -arch "$arch" -D "$file" | sed -n '2p')
    imports=$(otool -arch "$arch" -L "$file" | sed -n '2,$s/^[[:space:]]*\(.*\) (compatibility version .*/\1/p')
    while IFS= read -r dep; do
        case "$dep" in
            ''|"$own_id"|/System/Library/*|/usr/lib/*) continue ;;
            /*) ;;
            *) echo "Unsupported library install name in ${file##*/}: $dep" >&2; return 1 ;;
        esac
        base=${dep##*/}
        bundled="$frameworks/$base"
        if [[ ! -f "$bundled" ]]; then
            lipo "$dep" -verify_arch "$arch"
            cp -L "$dep" "$bundled"
            chmod u+w "$bundled"
            bundle_imports "$bundled" '@loader_path'
            install_name_tool -id "@rpath/$base" "$bundled"
        fi
        install_name_tool -change "$dep" "$prefix/$base" "$file"
    done <<< "$imports"
}

for name in skylatch-ui skylatch-client skylatch-agent skylatch-host-engine; do
    lipo "$bin_dir/$name" -verify_arch "$arch"
    install -m 0755 "$bin_dir/$name" "$macos/$name"
    bundle_imports "$macos/$name" '@loader_path/../Frameworks'
done

for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$repo/assets/icon/skylatch-256.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$repo/assets/icon/skylatch-256.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$resources/Skylatch.icns"
cp "$repo/LICENSE-APACHE" "$repo/LICENSE-GPL-3.0" "$repo/NOTICE" "$resources/"
sed -e "s/@VERSION@/${version%%-*}/g" -e "s/@BUILD_VERSION@/${version%%-*}/g" \
    -e "s/@MIN_VERSION@/$min_version/g" "$here/Info.plist.in" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

# Without CODESIGN_IDENTITY (or with "-") use an ad-hoc signature, which also
# repairs signatures invalidated by relocated dylibs. With a Developer ID
# identity, sign with Hardened Runtime and a secure timestamp.
identity=${CODESIGN_IDENTITY:--}
entitlements="$here/entitlements.plist"
if [[ $identity == - ]]; then
    for item in "$frameworks/"*.dylib "$macos/"*; do
        [[ -f $item ]] || continue
        codesign --force --sign - --timestamp=none "$item"
    done
    codesign --force --sign - --timestamp=none "$app"
    codesign --verify --deep --strict "$app"
else
    # Nested code first: dylibs, then helper executables, then the bundle.
    for item in "$frameworks/"*.dylib; do
        [[ -f $item ]] || continue
        codesign --force --timestamp --options runtime --sign "$identity" "$item"
    done
    for item in "$macos/"*; do
        [[ -f $item ]] || continue
        codesign --force --timestamp --options runtime \
            --entitlements "$entitlements" --sign "$identity" "$item"
    done
    codesign --force --timestamp --options runtime \
        --entitlements "$entitlements" --sign "$identity" "$app"
    codesign --verify --deep --strict --verbose=2 "$app"
fi
ln -s /Applications "$work/image/Applications"
name="Skylatch-$version-macos-$arch.dmg"
dmg_path="$work/$name"
hdiutil create -volname Skylatch -srcfolder "$work/image" -format UDZO "$dmg_path"
if [[ $identity != - ]]; then
    codesign --force --timestamp --sign "$identity" "$dmg_path"
    if [[ -n ${NOTARY_KEY_PATH:-} && -n ${NOTARY_KEY_ID:-} && -n ${NOTARY_ISSUER_ID:-} ]]; then
        xcrun notarytool submit "$dmg_path" --key "$NOTARY_KEY_PATH" \
            --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" --wait
        xcrun stapler staple "$dmg_path"
        xcrun stapler validate "$dmg_path"
        spctl --assess --type open --context context:primary-signature -vv "$dmg_path"
    fi
fi
mv -f "$dmg_path" "$output/$name"
printf 'Built %s\n' "$output/$name"
