#!/bin/sh
set -eu

REPO="madLinux7/mad-super-bot"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BINARY="mad-super-bot"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS/$ARCH" in
  Linux/x86_64|Linux/amd64) ASSET="mad-super-bot-linux-amd64" ;;
  Darwin/x86_64|Darwin/amd64) ASSET="mad-super-bot-darwin-amd64" ;;
  Darwin/arm64|Darwin/aarch64) ASSET="mad-super-bot-darwin-arm64" ;;
  *) echo "Error: unsupported platform: $OS/$ARCH" >&2; exit 1 ;;
esac

URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' 0
trap 'exit 1' HUP INT TERM

echo "Downloading the latest mad-super-bot for $OS/$ARCH..."
if ! curl -fSL --progress-bar "$URL" -o "$TMP"; then
  echo "Error: download failed. Check that a release exists for $OS/$ARCH." >&2
  exit 1
fi

chmod 755 "$TMP"
mkdir -p "$INSTALL_DIR"
mv "$TMP" "${INSTALL_DIR}/${BINARY}"
echo "mad-super-bot installed to ${INSTALL_DIR}/${BINARY}"

case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *) echo "Add this folder to your PATH to run mad-super-bot:"
     echo "  export PATH=\"${INSTALL_DIR}:\$PATH\"" ;;
esac
