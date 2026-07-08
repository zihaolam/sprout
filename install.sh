#!/bin/sh
# Install the latest release of `sprout`:
#   curl -fsSL https://raw.githubusercontent.com/zihaolam/sprout/main/install.sh | sh
#
# Options (env vars):
#   SPROUT_INSTALL_DIR   install directory (default: /usr/local/bin if
#                        writable, otherwise ~/.local/bin)
#   SPROUT_VERSION       tag to install, e.g. v0.1.0 (default: latest)
#   SPROUT_NO_MODIFY_RC  set to any value to skip editing your shell rc file
#                        (PATH + shell-init are printed instead)
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

# --- wire up PATH + shell integration ----------------------------------------
# Add the PATH export and `eval "$(sprout shell-init)"` to the user's shell rc,
# so `sprout` is on PATH and `switch`/`main` auto-cd with tab completion out of
# the box. Idempotent: we grep for each line first and only append what's
# missing, so re-running (or a line you added by hand) never gets duplicated.

# The rc file for the user's *login* shell ($SHELL), not the `sh` running this.
rc_file() {
  case "${SHELL:-}" in
    */zsh)  echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
    */bash) echo "$HOME/.bash_profile" ;; # macOS Terminal runs bash as a login shell
    *)      echo "" ;;
  esac
}

# Is INSTALL_DIR already on PATH in the shell that invoked us?
on_path() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) return 0 ;;
    *) return 1 ;;
  esac
}

RC="$(rc_file)"

if [ -n "${SPROUT_NO_MODIFY_RC:-}" ] || [ -z "$RC" ]; then
  # Opted out, or an unrecognized shell: fall back to printing instructions.
  if ! on_path; then
    echo
    echo "note: $INSTALL_DIR is not on your PATH. Add to your shell rc:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
  fi
  echo
  echo "for auto-cd on 'sprout switch/main' + tab completion, add to your shell rc:"
  echo "  eval \"\$(sprout shell-init)\""
else
  need_path=0
  need_init=0
  # PATH: skip if already active this session or already referenced in the rc.
  on_path || grep -qF "$INSTALL_DIR" "$RC" 2>/dev/null || need_path=1
  # shell-init: skip if the eval is already there (a prior run, or added by hand).
  grep -qF 'sprout shell-init' "$RC" 2>/dev/null || need_init=1

  if [ "$need_path" -eq 1 ] || [ "$need_init" -eq 1 ]; then
    {
      printf '\n# sprout\n'
      # PATH first, so the shell-init eval below can find `sprout`.
      if [ "$need_path" -eq 1 ]; then
        printf 'export PATH="%s:$PATH"\n' "$INSTALL_DIR"
      fi
      if [ "$need_init" -eq 1 ]; then
        printf 'eval "$(sprout shell-init)"\n'
      fi
    } >> "$RC"
    echo
    echo "updated $RC — restart your shell or run: source $RC"
  else
    echo
    echo "$RC already set up — nothing to change"
  fi
fi
