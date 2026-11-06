#!/bin/sh
# Downloads Spotify Soloist from Spotify's own servers into ~/.local/opt/soloist. Soloist may
# not be redistributed, so it is never bundled with htpc-nowplaying.
#
# Soloist builds expire after 90 days (it then exits with code 10). To update:
#   packaging/install-soloist.sh --update
# which downloads again, does nothing if the build is the same, and otherwise replaces the
# files and restarts soloist.service if it is running or failed. The Soloist unit and the
# weekly soloist-update.timer run it for you (see the README).
#
# --if-missing does nothing when Soloist is installed, and otherwise installs it like
# --update but never restarts anything: soloist.service runs it before each start, so the
# first start downloads Soloist by itself.
#
# usage: install-soloist.sh [--update | --if-missing] [install-dir]
set -eu

# update=1: no questions, and a same build is left alone. restart=1: restart the service after.
update=0
restart=0
if_missing=0
case "${1:-}" in
    --update)
        update=1
        restart=1
        shift
        ;;
    --if-missing)
        update=1
        if_missing=1
        shift
        ;;
esac

default_dest="$HOME/.local/opt/soloist"
dest="${1:-$default_dest}"
key_file="${XDG_CONFIG_HOME:-$HOME/.config}/soloist/api-key"
hash_file="$dest/.tarball.sha256"

if [ "$if_missing" = 1 ] && [ -x "$dest/soloist" ]; then
    exit 0
fi

case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=arm64 ;;
    armv7l | armv7* | armhf) arch=arm32 ;;
    *)
        echo "install-soloist: no Soloist build for $(uname -m)" >&2
        exit 1
        ;;
esac
url="https://soloist-builds.spotifycdn.com/soloist_release_${arch}.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url"
if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error --output "$tmp/soloist.tar.gz" "$url" || {
        echo "install-soloist: the download failed; nothing was changed" >&2
        exit 1
    }
elif command -v wget >/dev/null 2>&1; then
    wget --quiet --output-document="$tmp/soloist.tar.gz" "$url" || {
        echo "install-soloist: the download failed; nothing was changed" >&2
        exit 1
    }
else
    echo "install-soloist: needs curl or wget" >&2
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    new_hash="$(sha256sum "$tmp/soloist.tar.gz" | cut -d ' ' -f 1)"
else
    new_hash="$(shasum -a 256 "$tmp/soloist.tar.gz" | cut -d ' ' -f 1)"
fi

if [ "$update" = 1 ] && [ -f "$hash_file" ] && [ "$(cat "$hash_file")" = "$new_hash" ]; then
    echo "Soloist is up to date"
    exit 0
fi

mkdir -p "$tmp/extract"
tar -xzf "$tmp/soloist.tar.gz" -C "$tmp/extract"
if [ ! -x "$tmp/extract/soloist" ]; then
    echo "install-soloist: the download did not contain a soloist executable" >&2
    exit 1
fi

mkdir -p "$dest"
# Replace the files, keeping the folder (and anything else in it, such as logs).
for f in "$tmp"/extract/*; do
    cp -f "$f" "$dest/"
done
printf '%s\n' "$new_hash" >"$hash_file"
# A copy of this script next to Soloist gives the update units a path that does not depend on
# where the repository is. (Skipped when it is already running from there.)
if [ ! "$0" -ef "$dest/install-soloist.sh" ]; then
    cp -f "$0" "$dest/install-soloist.sh"
fi

version="$("$dest/soloist" --version 2>/dev/null || true)"

if [ "$if_missing" = 1 ]; then
    echo "Installed Soloist ${version:-} to $dest"
    exit 0
fi

if [ "$update" = 1 ]; then
    # The new build only runs once the service restarts. A stopped service stays stopped; an
    # expired one has failed, and starting it again is the point.
    if [ "$restart" = 1 ] && command -v systemctl >/dev/null 2>&1 &&
        { systemctl --user is-active --quiet soloist.service ||
            systemctl --user is-failed --quiet soloist.service; }; then
        systemctl --user restart soloist.service
    fi
    echo "Updated Soloist to ${version:-the new build}"
    exit 0
fi

echo "Installed to $dest"
[ -n "$version" ] && echo "$version"
if [ "$dest" != "$default_dest" ]; then
    echo
    echo "This is not the default folder. Tell htpc-nowplaying where Soloist is, in config.toml:"
    echo "  [daemon]"
    echo "  soloist_path = \"$dest/soloist\""
fi

if [ ! -s "$key_file" ]; then
    echo
    echo "No API key yet. Create one (needs Spotify Premium) at"
    echo "  https://developer.spotify.com/dashboard/soloist"
    if [ -t 0 ]; then
        printf "Paste it here (or press Enter to skip): "
        stty -echo 2>/dev/null || true
        read -r key || key=""
        stty echo 2>/dev/null || true
        echo
        if [ -n "$key" ]; then
            mkdir -p "$(dirname "$key_file")"
            (umask 077 && printf '%s' "$key" >"$key_file")
            echo "Saved to $key_file (readable only by you)"
        fi
    fi
    if [ ! -s "$key_file" ]; then
        echo "Then save it with:"
        echo "  mkdir -p $(dirname "$key_file") && (umask 077 && printf '%s' 'YOUR-KEY' > $key_file)"
    fi
fi

echo
echo "Next: install packaging/systemd/soloist.service (see the README) and pair from your phone."
