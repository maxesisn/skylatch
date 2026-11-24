#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# One-line Skylatch host installer for Debian/Ubuntu x86_64 (amd64).
#   curl -fsSL https://raw.githubusercontent.com/maxesisn/skylatch/main/packaging/install-host.sh |
#     sudo sh -s -- --hub https://hub.example.com --code 123456 [--virtual-desktop]
set -eu
# shellcheck disable=SC3040
(set -o pipefail) 2>/dev/null && set -o pipefail

repo=maxesisn/skylatch
version=
hub=
code=
room=
resolution=1920x1080
virtual=

die() { echo "install-host: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case $1 in
        --virtual-desktop) virtual=1; shift ;;
        --hub|--code|--room|--resolution|--version|--repo)
            [ $# -ge 2 ] || die "$1 needs a value"
            case $1 in
                --hub) hub=$2 ;;
                --code) code=$2 ;;
                --room) room=$2 ;;
                --resolution) resolution=$2 ;;
                --version) version=${2#v} ;;
                --repo) repo=$2 ;;
            esac
            shift 2 ;;
        -h|--help)
            echo 'Usage: sudo sh install-host.sh [--hub URL --code 6-DIGIT] [--room NAME] [--virtual-desktop] [--resolution WxH] [--version VER] [--repo OWNER/REPO]'
            exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die 'run as root (use sudo)'
[ "$(uname -s)" = Linux ] || die 'Linux only'
command -v apt-get >/dev/null 2>&1 || die 'Debian/Ubuntu (apt-get) required'
[ "$(dpkg --print-architecture)" = amd64 ] || die 'amd64 only'
[ -d /run/systemd/system ] || die 'a running systemd is required'
for tool in curl sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done
case $resolution in
    [1-9]*x[1-9]*) case $resolution in *[!0-9x]*) die 'invalid --resolution' ;; esac ;;
    *) die 'invalid --resolution (expected e.g. 1920x1080)' ;;
esac
if [ -n "$hub$code" ]; then
    [ -n "$hub" ] && [ -n "$code" ] || die '--hub and --code must be given together'
    code_digits=$(printf '%s' "$code" | tr -d -- '-')
    case $code_digits in
        [0-9][0-9][0-9][0-9][0-9][0-9]) ;;
        *) die '--code must be a 6-digit pairing code' ;;
    esac
fi

if [ -z "$version" ]; then
    version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || die 'could not resolve the latest release; pass --version'
fi
echo "Installing Skylatch $version"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
deb="Skylatch-$version-linux-amd64.deb"
base="https://github.com/$repo/releases/download/v$version"
curl -fsSL -o "$tmp/$deb" "$base/$deb"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
expected=$(awk -v f="$deb" '{ n = $2; sub(/^\*/, "", n); if (n == f) { print $1; exit } }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$deb is not listed in SHA256SUMS"
actual=$(sha256sum "$tmp/$deb" | awk '{ print $1 }')
[ "$expected" = "$actual" ] || die "checksum mismatch for $deb"

export DEBIAN_FRONTEND=noninteractive
apt-get install -y "$tmp/$deb"
/usr/share/skylatch/install-agent.sh

if [ -n "$virtual" ]; then
    apt-get install -y xvfb openbox x11-xserver-utils dbus-x11 xterm
    cat >/etc/systemd/system/skylatch-xvfb.service <<UNIT
[Unit]
Description=Skylatch virtual desktop (Xvfb + openbox)
After=network.target

[Service]
User=skylatch
Group=skylatch
ExecStart=/bin/sh -c 'Xvfb :99 -screen 0 ${resolution}x24 -nolisten tcp & xpid=\$!; trap "kill \$xpid" TERM INT; for i in 1 2 3 4 5 6 7 8 9 10; do xdpyinfo -display :99 >/dev/null 2>&1 && break; sleep 1; done; DISPLAY=:99 dbus-launch --exit-with-session openbox-session & wait \$xpid'
Restart=on-failure
RestartSec=3s

[Install]
WantedBy=multi-user.target
UNIT
    install -d /etc/systemd/system/skylatch-agent.service.d
    cat >/etc/systemd/system/skylatch-agent.service.d/virtual-desktop.conf <<'DROPIN'
[Unit]
After=skylatch-xvfb.service
Wants=skylatch-xvfb.service

[Service]
Environment=DISPLAY=:99
DROPIN
    systemctl daemon-reload
elif [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] || ls /tmp/.X11-unix/X* >/dev/null 2>&1; then
    echo 'A desktop session was detected; the agent streams it if the skylatch user can access that display.'
else
    echo 'No desktop session detected. On a pure headless server, re-run with --virtual-desktop'
    echo 'to start an Xvfb + openbox desktop for the agent.'
fi

if [ -n "$hub" ]; then
    set -- join "$hub" --code "$code"
    [ -z "$room" ] || set -- "$@" --room "$room"
    sudo -u skylatch env XDG_CONFIG_HOME=/var/lib/skylatch skylatch-agent "$@"
    [ -z "$virtual" ] || systemctl enable --now skylatch-xvfb.service
    systemctl enable --now skylatch-agent.service
    echo 'Skylatch host enrolled and running.'
else
    echo 'Installed. Enroll with: sudo -u skylatch env XDG_CONFIG_HOME=/var/lib/skylatch skylatch-agent join <hub> --code <6-digit>'
    echo 'Then: sudo systemctl enable --now skylatch-agent.service'
fi
