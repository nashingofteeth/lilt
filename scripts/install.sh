#!/usr/bin/env bash
# Build and install the `lilt` binary (same pattern as nile).
set -euo pipefail
cd "$(dirname "$0")/.."

# `cargo install` defaults to ~/.cargo, which is not always on PATH (notably on
# a fresh macOS setup). Prefer ~/.local/bin in that case.
if [ -n "${CARGO_INSTALL_ROOT:-}" ]; then
    root="$CARGO_INSTALL_ROOT"
else
    root="$HOME/.cargo"
    case ":$PATH:" in
        *":$root/bin:"*) ;;
        *) root="$HOME/.local" ;;
    esac
fi

cargo install --locked --force --path . --root "$root"
ln -sfn lilt "$root/bin/music"
echo "installed $root/bin/lilt (temporary music alias)"
