#!/bin/sh
# Install the latest release of `sprout`:
#   curl -fsSL https://raw.githubusercontent.com/zihaolam/sprout/main/install.sh | sh
#
# Options (env vars):
#   SPROUT_INSTALL_DIR  install directory (default: /usr/local/bin if
#                       writable, otherwise ~/.local/bin)
#   SPROUT_VERSION      tag to install, e.g. v0.1.0 (default: latest)
set -eu

REPO="zihaolam/sprout"
ASSET="sprout-universal-apple-darwin.tar.gz"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "error: sprout is macOS-only (APFS clonefile)" >&2
  exit 1
fi

if [ -n "${SPROUT_VERSION:-}" ]; then
  URL="https://github.com/$REPO/releases/download/$SPROUT_VERSION/$ASSET"
  SUMS_URL="https://github.com/$REPO/releases/download/$SPROUT_VERSION/checksums.txt"
else
  URL="https://github.com/$REPO/releases/latest/download/$ASSET"
  SUMS_URL="https://github.com/$REPO/releases/latest/download/checksums.txt"
fi

if [ -n "${SPROUT_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$SPROUT_INSTALL_DIR"
elif [ -w /usr/local/bin ]; then
  INSTALL_DIR=/usr/local/bin
else
  INSTALL_DIR="$HOME/.local/bin"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "downloading $URL"
curl -fsSL -o "$TMP/$ASSET" "$URL"

echo "verifying checksum"
curl -fsSL -o "$TMP/checksums.txt" "$SUMS_URL"
(cd "$TMP" && shasum -a 256 -c checksums.txt >/dev/null)

tar -xzf "$TMP/$ASSET" -C "$TMP"
mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/sprout" "$INSTALL_DIR/sprout"

echo "installed $("$INSTALL_DIR/sprout" --version) to $INSTALL_DIR/sprout"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo
    echo "note: $INSTALL_DIR is not on your PATH. Add to ~/.zshrc:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

echo
echo "optional — auto-cd on 'sprout switch': add to ~/.zshrc:"
echo "  eval \"\$(sprout shell-init)\""
