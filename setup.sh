#!/usr/bin/env bash
# vevor-lcd — one-time system setup.
#
# Installs the udev rule that creates /dev/vevor_lcd and grants the plugdev
# group permission to open it, then reloads udev. Nothing else is modified;
# no packages are installed.
#
# Usage: sudo bash setup.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RULE_SRC="$SCRIPT_DIR/99-vevor-lcd.rules"
RULE_DST="/etc/udev/rules.d/99-vevor-lcd.rules"

if [[ $EUID -ne 0 ]]; then
    echo "This script needs root to install the udev rule."
    echo "Re-run as: sudo bash setup.sh"
    exit 1
fi

echo "==> Installing udev rule → $RULE_DST"
install -m 0644 "$RULE_SRC" "$RULE_DST"

echo "==> Reloading udev"
udevadm control --reload
udevadm trigger --subsystem-match=usb --subsystem-match=hidraw --action=add

# Add the invoking user (the one who ran sudo) to plugdev, if not already.
TARGET_USER="${SUDO_USER:-}"
if [[ -n "$TARGET_USER" ]] && ! id -nG "$TARGET_USER" | tr ' ' '\n' | grep -qx plugdev; then
    echo "==> Adding $TARGET_USER to the 'plugdev' group"
    usermod -aG plugdev "$TARGET_USER"
    echo "    (log out and back in for this to take effect)"
fi

cat <<EOF

==> Done.

If the LCD is already plugged in, either unplug + replug it, or reboot, so
the new udev rule binds to the live device. Then run:

    bash verify.sh
EOF
