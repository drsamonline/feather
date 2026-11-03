#!/usr/bin/env bash
# Register Feather with the desktop: a launcher, a MIME association, and the
# right to be chosen as the default viewer for the formats it can actually open.
#
# Deliberately does not seize any default. It registers as a candidate, which is
# what §7 of the plan says an application's job is; the user picks the default.
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BINARY=""
DRY_RUN=0
UNINSTALL=0

usage() {
    cat <<'USAGE'
usage: install.sh [options]

  --prefix DIR    install root (default: ~/.local)
  --binary PATH   the feather-viewer executable to point the launcher at
  --dry-run       do the work but skip system commands (safe in CI)
  --uninstall     remove the launcher and associations
  -h, --help      this message
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="$2"; shift 2 ;;
        --binary) BINARY="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ENTRY="$APP_DIR/feather.desktop"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

run() {
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "would run: $*"
    else
        "$@" || true
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    rm -f "$ENTRY"
    echo "removed $ENTRY"
    run update-desktop-database "$APP_DIR"
    exit 0
fi

if [ -z "$BINARY" ]; then
    for candidate in "$BIN_DIR/feather-viewer" "$(command -v feather-viewer || true)" "$HERE/../../target/release/feather-viewer"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            BINARY="$candidate"
            break
        fi
    done
fi

if [ -z "$BINARY" ]; then
    echo "feather: could not find feather-viewer; pass --binary PATH" >&2
    exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR"

if [ ! -e "$BIN_DIR/feather-viewer" ] && [ "$BINARY" != "$BIN_DIR/feather-viewer" ]; then
    cp "$BINARY" "$BIN_DIR/feather-viewer"
    chmod +x "$BIN_DIR/feather-viewer"
    echo "installed $BIN_DIR/feather-viewer"
fi

TARGET="$BIN_DIR/feather-viewer"
sed "s|__FEATHER_BIN__|$TARGET|g" "$HERE/feather.desktop" > "$ENTRY"
echo "wrote $ENTRY"

run update-desktop-database "$APP_DIR"

echo
echo "Feather is registered. To make it the default for PDFs:"
echo "  xdg-mime default feather.desktop application/pdf"
echo "(the installer will not do that for you — §7 of the plan)"
