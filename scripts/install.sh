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

# Stop a pre-rename `music` daemon so it does not keep playing alongside lilt.
legacy_pid_file="$HOME/.cache/music/mpv.pid"
legacy_runtime="${XDG_RUNTIME_DIR:-/tmp}/music-mount-$(id -u)"
legacy_pid=""
if [ -f "$legacy_pid_file" ]; then
    legacy_pid="$(cat "$legacy_pid_file" 2>/dev/null || true)"
fi
case "$legacy_pid" in
    '' | *[!0-9]*) legacy_pid="" ;;
esac
if [ -n "$legacy_pid" ] &&
    ps -p "$legacy_pid" -o command= 2>/dev/null |
        grep -Fq -- "input-ipc-server=${legacy_runtime}/mpv.sock"; then
    echo "stopping legacy music daemon (pid $legacy_pid)"
    kill -TERM "$legacy_pid" 2>/dev/null || true
    for _ in 1 2 3 4 5; do
        kill -0 "$legacy_pid" 2>/dev/null || break
        sleep 0.2
    done
    kill -KILL "$legacy_pid" 2>/dev/null || true
fi
if [ -z "$legacy_pid" ] || ! kill -0 "$legacy_pid" 2>/dev/null; then
    rm -f "$legacy_pid_file" "${legacy_runtime}/mpv.sock"
fi

cargo install --locked --force --path . --root "$root"
ln -sfn lilt "$root/bin/music"

other_music="$(command -v music 2>/dev/null || true)"
if [ -n "$other_music" ] && [ "$other_music" != "$root/bin/music" ]; then
    echo "warning: $other_music shadows $root/bin/music; remove it or adjust PATH" >&2
fi

echo "installed $root/bin/lilt (temporary music alias)"
