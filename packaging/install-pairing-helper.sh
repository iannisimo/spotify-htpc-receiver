#!/bin/sh
# Installs the pairing helper, so htpc-nowplaying can open Soloist's pairing port in the
# firewall without a terminal or a password prompt. Run it once, from your own account:
#
#   sudo packaging/install-pairing-helper.sh
#   sudo packaging/install-pairing-helper.sh --uninstall
#
# It installs packaging/pairing-port as /usr/local/libexec/htpc-nowplaying/pairing-port
# (root-owned) and adds /etc/sudoers.d/htpc-nowplaying, which lets you (the user who ran sudo)
# run exactly `pairing-port open`, `close` and `status` as root without a password. Nothing
# else. The sudoers file is checked with `visudo -c` before it is put in place.
#
# --uninstall closes the port, then removes both files.

set -eu

HELPER_DIR=/usr/local/libexec/htpc-nowplaying
HELPER=$HELPER_DIR/pairing-port
SUDOERS=/etc/sudoers.d/htpc-nowplaying
SRC_DIR=$(cd "$(dirname "$0")" && pwd)

die() {
    echo "install-pairing-helper: $*" >&2
    exit 1
}

[ "$(id -u)" = 0 ] || die "run it with sudo: sudo $0 ${1:-}"

case ${1:-} in
    --uninstall)
        if [ -x "$HELPER" ]; then
            "$HELPER" close >/dev/null || echo "install-pairing-helper: could not close the pairing port; check your firewall's rules" >&2
        fi
        rm -f "$SUDOERS" "$HELPER"
        rmdir "$HELPER_DIR" 2>/dev/null || true
        rm -rf /run/htpc-nowplaying
        echo "Removed the pairing helper."
        exit 0
        ;;
    '') ;;
    *) die "usage: sudo $0 [--uninstall]" ;;
esac

USER_NAME=${SUDO_USER:-}
if [ -z "$USER_NAME" ] || [ "$USER_NAME" = root ]; then
    die "run it with sudo from the account that runs Soloist, not as root"
fi
# The name goes into a sudoers file: allow only plain account names.
case $USER_NAME in
    -* | *[!A-Za-z0-9._-]*) die "unusual user name, refusing to write it into sudoers: $USER_NAME" ;;
esac
id -u "$USER_NAME" >/dev/null 2>&1 || die "no such user: $USER_NAME"
[ -f "$SRC_DIR/pairing-port" ] || die "pairing-port not found next to this script ($SRC_DIR)"
command -v visudo >/dev/null 2>&1 || die "visudo not found (is sudo installed?)"

# Write and check the sudoers rule before installing anything. The temp name contains a dot,
# so sudo ignores it while it exists.
tmp=$(mktemp /etc/sudoers.d/.htpc-nowplaying.XXXXXX)
trap 'rm -f "$tmp"' EXIT
cat >"$tmp" <<EOF
# Installed by htpc-nowplaying's install-pairing-helper.sh. Lets $USER_NAME open and close
# Soloist's pairing port without a password. Remove with:
#   sudo install-pairing-helper.sh --uninstall
$USER_NAME ALL=(root) NOPASSWD: $HELPER open, $HELPER close, $HELPER status
EOF
chmod 0440 "$tmp"
visudo -cf "$tmp" >/dev/null || die "the sudoers rule did not pass visudo -c; nothing was changed"

install -d -o root -g root -m 0755 "$HELPER_DIR"
install -o root -g root -m 0755 "$SRC_DIR/pairing-port" "$HELPER"
mv -f "$tmp" "$SUDOERS"
trap - EXIT

if ! grep -Eq '^[#@]includedir[[:space:]]+/etc/sudoers\.d/?[[:space:]]*$' /etc/sudoers; then
    echo "install-pairing-helper: /etc/sudoers does not read /etc/sudoers.d, so the rule has no" >&2
    echo "effect. Add this line with 'sudo visudo':  @includedir /etc/sudoers.d" >&2
fi

echo "Installed $HELPER and $SUDOERS for $USER_NAME."
if status=$(su -s /bin/sh -c "sudo -n $HELPER status" "$USER_NAME" 2>&1); then
    echo "Check as $USER_NAME: $status"
else
    echo "install-pairing-helper: the check as $USER_NAME failed: $status" >&2
    exit 1
fi
