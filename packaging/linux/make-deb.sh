#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo 'Usage: make-deb.sh VERSION BIN_DIR [OUTPUT_DIR]' >&2
    exit 2
fi
version=${1#v}
bin_dir=$2
output=${3:-dist}
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ ]]; then
    echo 'Expected VERSION like 1.2.3.' >&2
    exit 2
fi
deb_version=$version
if [[ $version == *-* ]]; then
    deb_version="${version%%-*}~${version#*-}"
fi
arch=$(dpkg --print-architecture)
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg="$work/pkg"

install -d "$pkg/DEBIAN" "$pkg/usr/bin" "$pkg/usr/share/applications" \
    "$pkg/usr/share/icons/hicolor/scalable/apps" "$pkg/usr/share/doc/skylatch" \
    "$pkg/usr/share/skylatch" "$work/debian" "$output"
for bin in skylatch-client skylatch-agent skylatch-host-engine; do
    install -m 0755 "$bin_dir/$bin" "$pkg/usr/bin/$bin"
done
install -m 0644 "$here/skylatch.desktop" "$pkg/usr/share/applications/"
install -m 0644 "$repo/assets/icon/skylatch.svg" "$pkg/usr/share/icons/hicolor/scalable/apps/"
install -m 0644 "$repo/LICENSE-APACHE" "$repo/LICENSE-GPL-3.0" "$repo/NOTICE" "$pkg/usr/share/doc/skylatch/"
install -m 0644 "$here/skylatch-agent.service" "$pkg/usr/share/skylatch/"
install -m 0755 "$here/install-agent.sh" "$pkg/usr/share/skylatch/"

# Resolve dependency package names and minimum versions against the build distribution.
cat > "$work/debian/control" <<'CONTROL'
Source: skylatch

Package: skylatch
Architecture: any
Description: Native remote desktop client and host
CONTROL
depends=$(cd "$work" && dpkg-shlibdeps -O -e"$pkg/usr/bin/skylatch-client" \
    -e"$pkg/usr/bin/skylatch-agent" -e"$pkg/usr/bin/skylatch-host-engine")
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: skylatch
Version: $deb_version
Section: net
Priority: optional
Architecture: $arch
Maintainer: Skylatch contributors <maxesisn@users.noreply.github.com>
Homepage: https://github.com/maxesisn/skylatch
Depends: ${depends#shlibs:Depends=}
Description: Native remote desktop client and host
 Skylatch provides a native desktop viewer and an enrollable host agent.
CONTROL

dpkg-deb --root-owner-group --build "$pkg" "$output/Skylatch-$version-linux-$arch.deb"
