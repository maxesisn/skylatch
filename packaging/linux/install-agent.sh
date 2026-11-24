#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    echo 'Usage: sudo install-agent.sh [BIN_DIR]'
    echo 'With no BIN_DIR, use the binaries already installed by the Skylatch package.'
    exit 0
fi
if [[ $EUID -ne 0 ]]; then
    echo 'Run this script as root with sudo.' >&2
    exit 1
fi
if [[ ! -d /run/systemd/system ]]; then
    echo 'This installer requires a running systemd system manager.' >&2
    exit 1
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
src=${1:-/usr/bin}
dest=/usr/bin
if [[ $# -eq 1 ]]; then
    dest=/usr/local/bin
fi
for bin in skylatch-agent skylatch-host-engine; do
    if [[ ! -f "$src/$bin" || ! -x "$src/$bin" ]]; then
        echo "Missing executable: $src/$bin" >&2
        exit 1
    fi
done
if [[ $src != "$dest" ]]; then
    install -d "$dest"
    install -m 0755 "$src/skylatch-agent" "$src/skylatch-host-engine" "$dest/"
fi
if ! id skylatch >/dev/null 2>&1; then
    useradd --system --user-group --home-dir /var/lib/skylatch --shell /usr/sbin/nologin skylatch
fi
install -d -m 0700 -o skylatch -g skylatch /var/lib/skylatch
unit=/etc/systemd/system/skylatch-agent.service
sed "s|/usr/bin/skylatch-|$dest/skylatch-|g" "$here/skylatch-agent.service" | install -m 0644 /dev/stdin "$unit"
systemctl daemon-reload

cat <<INSTRUCTIONS
Enroll the service account (replace the hub URL and pairing code):
  sudo -u skylatch env XDG_CONFIG_HOME=/var/lib/skylatch $dest/skylatch-agent join <hub> --code <6-digit>

Then start the agent now and at boot:
  sudo systemctl enable --now skylatch-agent.service

Enrollment is saved under /var/lib/skylatch. The agent can run headless;
streaming a desktop still requires a display session accessible to the service user.
INSTRUCTIONS
