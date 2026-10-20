#!/bin/sh
set -eu

# Orca Daemon Uninstaller for Linux
# Usage: curl -fsSL https://orca-desktop.com/uninstall-daemon.sh | sudo sh

echo "=== Uninstalling Orca Daemon ==="

if [ "$(id -u)" -ne 0 ]; then
    echo "Error: This script must be run as root (use sudo)"
    exit 1
fi

if dpkg-query -W -f='${Status}' orca-daemon 2>/dev/null | grep -q "ok installed"; then
    # Installed by install-daemon.sh: let apt remove the package's files
    # (its prerm stops and disables the service)
    apt-get remove -y orca-daemon
else
    # Standalone binary installed by hand
    systemctl stop orca-daemon 2>/dev/null || true
    systemctl disable orca-daemon 2>/dev/null || true
    rm -f /etc/systemd/system/orca-daemon.service
    rm -f /usr/local/bin/orca-daemon /usr/local/bin/orca
fi
systemctl daemon-reload

# Drop the Cloudsmith apt repository added by install-daemon.sh
rm -f /etc/apt/sources.list.d/edvin-orca.list \
    /usr/share/keyrings/edvin-orca-archive-keyring.gpg \
    /etc/apt/trusted.gpg.d/edvin-orca.gpg

echo ""
echo "Orca daemon removed."
echo "Config preserved at /etc/orca/ — delete manually if not needed."
