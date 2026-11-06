#!/bin/sh
# Downloads Spotify Soloist from Spotify's own servers into ~/.local/opt/soloist. Soloist may
# not be redistributed, so it is never bundled with htpc-nowplaying.
#
# Also the way to update: Soloist builds expire after 90 days (it then exits with code 10).
# Run this again, then: systemctl --user restart soloist.service
#
# usage: packaging/install-soloist.sh [install-dir]
set -eu

dest="${1:-$HOME/.local/opt/soloist}"
key_file="${XDG_CONFIG_HOME:-$HOME/.config}/soloist/api-key"

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
    curl --fail --location --silent --show-error --output "$tmp/soloist.tar.gz" "$url"
elif command -v wget >/dev/null 2>&1; then
    wget --quiet --output-document="$tmp/soloist.tar.gz" "$url"
else
    echo "install-soloist: needs curl or wget" >&2
    exit 1
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
echo "Installed to $dest"
"$dest/soloist" --version 2>/dev/null || true

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
