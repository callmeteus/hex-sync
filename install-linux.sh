#!/bin/sh
# hex-sync online installer for Linux.
# Usage: curl -fsSL https://raw.githubusercontent.com/callmeteus/hex-sync/master/install-linux.sh | sh
set -eu

REPO="callmeteus/hex-sync"
ASSET="hex-sync-linux-x86_64"
INSTALL_DIR="${HEX_SYNC_INSTALL_DIR:-$HOME/.local/bin}"

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64) ;;
    *)
        echo "hex-sync: unsupported architecture '$ARCH' (only x86_64 is published)" >&2
        exit 1
        ;;
esac

OS="$(uname -s)"
if [ "$OS" != "Linux" ]; then
    echo "hex-sync: this installer is for Linux (detected '$OS')" >&2
    exit 1
fi

if [ -n "${HEX_SYNC_VERSION:-}" ]; then
    URL="https://github.com/$REPO/releases/download/$HEX_SYNC_VERSION/$ASSET"
else
    URL="https://github.com/$REPO/releases/latest/download/$ASSET"
fi

TMP="$(mktemp "${TMPDIR:-/tmp}/hex-sync.XXXXXX")"
trap 'rm -f "$TMP"' EXIT HUP INT TERM

echo "Downloading hex-sync from $URL"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP"
elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$TMP" "$URL"
else
    echo "hex-sync: install curl or wget first" >&2
    exit 1
fi

if [ ! -s "$TMP" ]; then
    echo "hex-sync: download produced an empty file" >&2
    exit 1
fi

chmod +x "$TMP"
mkdir -p "$INSTALL_DIR"
mv "$TMP" "$INSTALL_DIR/hex-sync"
trap - EXIT HUP INT TERM

echo "Installed: $INSTALL_DIR/hex-sync"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Add $INSTALL_DIR to your PATH, then run: hex-sync --help" ;;
esac
