#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# One-line Skylatch Hub installer for x86_64 Linux with systemd.
#   curl -fsSL https://raw.githubusercontent.com/maxesisn/skylatch/main/packaging/install-hub.sh | sudo sh
#   ... | sudo sh -s -- --version 1.2.3 --user admin --password '...' --port 17920
set -eu
# shellcheck disable=SC3040
(set -o pipefail) 2>/dev/null && set -o pipefail

repo=maxesisn/skylatch
version=
user=
password=
port=17920
mode=relay
generated=

die() { echo "install-hub: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case $1 in
        --version|--user|--password|--port|--repo|--mode)
            [ $# -ge 2 ] || die "$1 needs a value"
            case $1 in
                --version) version=${2#v} ;;
                --user) user=$2 ;;
                --password) password=$2 ;;
                --port) port=$2 ;;
                --repo) repo=$2 ;;
                --mode) mode=$2 ;;
            esac
            shift 2 ;;
        -h|--help)
            echo 'Usage: sudo sh install-hub.sh [--version VER] [--user NAME] [--password PASS] [--port PORT] [--repo OWNER/REPO] [--mode relay|signaling-only]'
            exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

case $mode in relay|signaling-only) ;; *) die "--mode must be relay or signaling-only" ;; esac

[ "$(id -u)" -eq 0 ] || die 'run as root (use sudo)'
[ "$(uname -s)" = Linux ] || die 'Linux only'
[ "$(uname -m)" = x86_64 ] || die 'x86_64 only'
[ -d /run/systemd/system ] || die 'a running systemd is required'
for tool in curl sha256sum tar systemctl; do
    command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done
case $port in ''|*[!0-9]*) die 'invalid --port' ;; esac
case $repo in */*) ;; *) die '--repo must be owner/repo' ;; esac

if [ -z "$version" ]; then
    version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || die 'could not resolve the latest release; pass --version'
fi
echo "Installing skylatch-hub $version"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
archive="skylatch-hub-$version-linux-x86_64-musl.tar.gz"
base="https://github.com/$repo/releases/download/v$version"
curl -fsSL -o "$tmp/$archive" "$base/$archive"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
expected=$(awk -v f="$archive" '{ n = $2; sub(/^\*/, "", n); if (n == f) { print $1; exit } }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$archive is not listed in SHA256SUMS"
actual=$(sha256sum "$tmp/$archive" | awk '{ print $1 }')
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive"
mkdir "$tmp/x"
tar -xzf "$tmp/$archive" -C "$tmp/x"
[ -f "$tmp/x/skylatch-hub" ] || die 'archive does not contain skylatch-hub'

install -d -m 0755 /usr/local/bin
install -m 0755 "$tmp/x/skylatch-hub" /usr/local/bin/skylatch-hub
install -d -m 0755 /etc/skylatch
id skylatch-hub >/dev/null 2>&1 ||
    useradd --system --user-group --no-create-home --home-dir /var/lib/skylatch --shell /usr/sbin/nologin skylatch-hub
install -d -m 0700 -o skylatch-hub -g skylatch-hub /var/lib/skylatch

if [ ! -e /etc/skylatch/hub.env ]; then
    if [ -z "$user" ]; then
        user='admin'
        if [ -r /dev/tty ] && [ -w /dev/tty ]; then
            printf 'Initial username [admin]: ' >/dev/tty
            read -r answer </dev/tty || answer=
            [ -z "$answer" ] || user=$answer
        fi
    fi
    if [ -z "$password" ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then
        printf 'Initial password (empty = generate): ' >/dev/tty
        stty -echo </dev/tty 2>/dev/null || true
        read -r password </dev/tty || password=
        stty echo </dev/tty 2>/dev/null || true
        printf '\n' >/dev/tty
    fi
    if [ -z "$password" ]; then
        password=$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 16)
        generated=1
    fi
    case $user$password in
        *[,\"\'\\\ ]*|*[![:print:]]*) die 'username/password must not contain commas, quotes, backslashes or spaces' ;;
    esac
    case $user in *=*|'') die 'invalid username' ;; esac
    [ "${#password}" -ge 8 ] || die 'password must be at least 8 characters'
    umask 077
    printf 'SKYLATCH_USERS=%s=%s\n' "$user" "$password" >/etc/skylatch/hub.env
    chmod 600 /etc/skylatch/hub.env
else
    echo 'Keeping existing /etc/skylatch/hub.env'
fi

cat >/etc/systemd/system/skylatch-hub.service <<UNIT
[Unit]
Description=Skylatch Hub
Wants=network-online.target
After=network-online.target

[Service]
EnvironmentFile=/etc/skylatch/hub.env
ExecStart=/usr/local/bin/skylatch-hub --listen 127.0.0.1:$port --state-file /var/lib/skylatch/state.json --mode $mode
Restart=on-failure
RestartSec=5s
User=skylatch-hub
Group=skylatch-hub

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now skylatch-hub.service
systemctl restart skylatch-hub.service

ok=
for _ in 1 2 3 4 5 6 7 8 9 10; do
    if curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then ok=1; break; fi
    sleep 1
done
[ -n "$ok" ] || die "hub did not become healthy; see: journalctl -u skylatch-hub"
echo "Hub is healthy on 127.0.0.1:$port"

cat <<NOTES

Put a TLS reverse proxy in front (WebSocket upgrades must pass through):
  Caddy  (/etc/caddy/Caddyfile):
    hub.example.com {
        reverse_proxy 127.0.0.1:$port
    }
  Nginx  (inside a server { listen 443 ssl; ... } block):
    location / {
        proxy_pass http://127.0.0.1:$port;
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade; proxy_set_header Connection "upgrade"; proxy_set_header Host \$host;
    }

Sign in at https://hub.example.com/ (your own domain) to add devices.
NOTES
if [ -n "$generated" ]; then
    echo "Generated login (shown once): user=$user password=$password"
fi
