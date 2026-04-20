#!/usr/bin/env bash
# vevor-lcd — sanity check.
#
# Prints a PASS/FAIL line for each prerequisite and optional runtime bit.
# Read-only; does not write to the device.
#
# Usage: bash verify.sh

set -u

GREEN=$'\033[0;32m'
RED=$'\033[0;31m'
YELLOW=$'\033[0;33m'
NC=$'\033[0m'

pass=0; fail=0; warn=0

check() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "${GREEN}PASS${NC}  $label"
        pass=$((pass+1))
    else
        echo "${RED}FAIL${NC}  $label"
        fail=$((fail+1))
    fi
}

soft() {
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        echo "${GREEN}PASS${NC}  $label"
        pass=$((pass+1))
    else
        echo "${YELLOW}WARN${NC}  $label"
        warn=$((warn+1))
    fi
}

cd "$(dirname "${BASH_SOURCE[0]}")"

echo "== udev"
check "rule installed in /etc/udev/rules.d"   test -f /etc/udev/rules.d/99-vevor-lcd.rules
check "rule file matches repo copy"           diff -q 99-vevor-lcd.rules /etc/udev/rules.d/99-vevor-lcd.rules

echo
echo "== User"
check "user in 'plugdev' group"               bash -c 'id -nG | grep -qw plugdev'

echo
echo "== Device"
if lsusb | grep -q '5131:2007'; then
    echo "${GREEN}PASS${NC}  device 5131:2007 enumerated"
    pass=$((pass+1))
    lsusb | grep '5131:2007' | sed 's/^/       /'
else
    echo "${RED}FAIL${NC}  device 5131:2007 NOT enumerated — is the LCD cable plugged in?"
    fail=$((fail+1))
fi

check "/dev/vevor_lcd symlink exists"         test -L /dev/vevor_lcd
check "/dev/vevor_lcd is writable"            test -w /dev/vevor_lcd

echo
echo "== Driver"
soft  "binary at ~/.local/bin/vevor-lcd"      test -x "$HOME/.local/bin/vevor-lcd"
soft  "systemd user service enabled"          bash -c 'systemctl --user is-enabled vevor-lcd.service | grep -qx enabled'

if systemctl --user is-active vevor-lcd.service >/dev/null 2>&1; then
    echo "${GREEN}PASS${NC}  systemd user service active"
    systemctl --user show vevor-lcd.service \
        -p ActiveState,SubState,MainPID,MemoryCurrent 2>/dev/null | sed 's/^/       /'
    pass=$((pass+1))
else
    echo "${YELLOW}WARN${NC}  systemd user service not active (ok if running manually)"
    warn=$((warn+1))
fi

echo
echo "== Summary: ${pass} pass / ${fail} fail / ${warn} warn"
exit $(( fail > 0 ? 1 : 0 ))
