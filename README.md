# music

A Rust wrapper around [mpv](https://mpv.io/) for playing a music library that
lives behind an [rclone](https://rclone.org/) mount.

It does three things:

- mounts the library (`rclone mount`, read-only) when asked,
- keeps one detached, audio-only mpv process running with an IPC socket,
- forwards commands (`play`, `next`, `pause`, `seek`, …) to that process from
  any terminal, with a multi-pane ratatui control panel.

Because mpv stays alive in the background, playback continues after the
terminal that started it is closed, and the next command works from anywhere.
If [`mpv-mpris`](https://github.com/hoyon/mpv-mpris) is installed, desktop media
keys and MPRIS panels work too.

## Requirements

- `mpv`, `rclone`
- `fzf` for `music browse`
- `parec` (pulseaudio-utils) for the spectrum visualizer
- `fusermount3` (util-linux) for `music unmount`
- `tail` (coreutils) for `music logs -f`
- optional: `mpv-mpris` for hardware media keys

Linux only: the player uses `mountpoint`, `fusermount3`, `setsid`, and
`XDG_RUNTIME_DIR`.

## Build and install

```sh
cargo build --release
./target/release/music --help

# Install `music` into ~/.cargo/bin
./scripts/install.sh

# Format, lint, and test
./scripts/check.sh

# Property/fuzz tests (bigger case count)
./scripts/fuzz.sh
```

`scripts/fuzz.sh` exercises random CLI parses and executions, TOML config
parsing, path resolution, playlist building over random directory trees, mpv
IPC replies and status parsing, and ratatui rendering at random terminal sizes.
Use `FUZZ_CASES=50000 ./scripts/fuzz.sh` for a longer run.

## Usage

`music mount` mounts the library in the foreground (`Ctrl-C` unmounts). `music`
or `music help` shows the command list.

```
Library
  music mount                    Mount in the foreground
  music unmount                  Quit mpv, then unmount the library
  music status                   Mount state + now playing

Playback
  music play [PATH] [--shuffle]  Replace the queue. No PATH = whole library shuffled
  music add PATH...              Append albums/artists/tracks
  music browse                   Pick artists and albums with fzf
  music toggle | pause | resume
  music next | prev | stop
  music volume N|+N|-N
  music seek N|-N|N%
  music shuffle [on|off|toggle]
  music repeat [off|all|one]
  music controls                 Full-screen control panel
  music quit                     Stop and exit mpv
  music restart                  Quit mpv and unmount the library
  music logs [-f]                Tail rclone/mpv logs
```

`PATH` may be absolute, relative to the current directory, or relative to the
music mount — so `music play "Brian Eno"` and
`music play "Brian Eno/Ambient 1"` both work. Matching falls back to a
case-insensitive search under the mount.

`browse` returns as soon as you make a selection: it hands the queue to the
detached mpv daemon and exits, so the picker window closes immediately.

### Control panel

`music controls` is a ratatui UI with three panes:

- **Now Playing** — artist/album, title, progress, volume/shuffle/repeat.
- **Spectrum** — live bars from the system output monitor (`v` toggles it).
- **Queue** — always visible, with the current track marked.

`Tab` switches focus between Now Playing and Queue; the focused pane's border
is cyan and the other is dim. If the queue is empty when the panel opens, it
automatically runs the add picker first.

**Common keys**

| Key | Action |
| --- | --- |
| `space` | pause/resume |
| `n` / `p` | next / previous track |
| `h` / `l` | seek −10s / +10s |
| `-` / `+` | volume −5 / +5 |
| `s` / `r` / `m` | shuffle / repeat / mute |
| `o` (or `b`) | browse library → play now (replace queue) |
| `a` | browse library → add to queue |
| `v` | visualizer on/off |
| `Tab` | switch focus (Now Playing ↔ Queue) |
| `/` | focus the queue and filter it |
| `?` | full help overlay |
| `q` / `Q` | detach / quit mpv |

**While Now Playing has focus**

| Key | Action |
| --- | --- |
| `j` / `k` | next / previous track |
| `g` / `gg` / `G` | first track / first track / last track |
| `←` / `→` | seek −10s / +10s |
| `↑` / `↓` | volume +5 / −5 |

**While Queue has focus**

| Key | Action |
| --- | --- |
| `j` / `k` | move selection |
| `g` / `gg` / `G` | first / first / last |
| `Ctrl-d` / `Ctrl-u` | half-page down / up |
| `Ctrl-f` / `Ctrl-b` | full-page down / up |
| `↑` / `↓` | move selection |
| `Enter` | switch to the selected track |
| `Esc` | clear the filter, else focus Now Playing |

- The `/` filter is case-insensitive; `Enter` keeps it, `Esc` clears it.
- `gg` uses a 500ms pending-`g` state; a single `g` also goes first after the timeout.
- The picker (fzf) temporarily takes over the terminal, then returns to the panel.

### Visualizer

The spectrum pane captures the default output monitor with `parec` and renders
log-spaced FFT bars at ~30 FPS. It shows **system output**, not just mpv — bars
keep moving if another app is playing. `v` toggles it; toggling off/on reconnects
to the current default sink. It is hidden automatically below 20 terminal rows.

The UI polls mpv over one persistent socket, reconnects if the daemon restarts,
and restores the terminal on exit.

## Configuration

Defaults:

```toml
# ~/.config/music/config.toml (optional)
music_dir = "~/Music"
remote = "sam:/Volumes/NASH_SYNC/music/library"
mpv_extra_args = []
```

`MUSIC_DIR` and `MUSIC_REMOTE` environment variables override the file.
`mpv_extra_args` is appended to the daemon's mpv command line (useful for
testing with `--ao=null`).

### Gapless playback

Tracks are streamed over the rclone mount, so the gap between tracks depends on
how fast the next file can be opened. mpv is started with:

- `--gapless-audio=yes` — keep the audio device open across files
- `--prefetch-playlist=yes` — open the next playlist entry before the current one ends
- `--audio-buffer=0.5` — buffer enough audio to cover the network open
- `--demuxer-max-bytes=256MiB --demuxer-readahead-secs=30` — keep the next file ready early
- `--autoload-files=no` — never auto-open sidecars (cover images, subtitles, external audio)

These are intentionally mpv-only: no rclone disk cache, so playback starts
immediately and nothing is written to a cache directory. If a particular album
still has gaps (for example, tracks with different sample rates), the next step
would be a local rclone VFS cache rather than larger demux buffers.

## How it works

- `rclone mount <remote> <music_dir> --read-only --dir-cache-time=10m`
- one detached mpv process:
  `mpv --idle --no-video --no-terminal --input-ipc-server=…/mpv.sock`
- the Rust binary talks to mpv over JSON on the Unix socket; all commands and
  properties are handled in-process (no Python or jq)
- explicit paths/albums are expanded by the wrapper into an m3u using an
  audio-extension allowlist (hidden files and AppleDouble `._*` sidecars are
  skipped); whole-library shuffle lets mpv expand the directory asynchronously
- state lives outside the repo:
  - socket: `${XDG_RUNTIME_DIR:-/tmp}/music-mount-$UID/mpv.sock`
  - logs and pid files: `~/.cache/music/`

## Desktop integration

Example [niri](https://github.com/YaLTeR/niri) binding for a floating controls
window:

```kdl
Mod+M { spawn-sh "~/.config/niri/focus-or-spawn.sh com.mitchellh.ghostty.music ghostty --class=com.mitchellh.ghostty.music -e $HOME/.cargo/bin/music controls"; }
```

```kdl
window-rule {
    match app-id=r#"^com\.mitchellh\.ghostty\.music$"#
    open-floating true
    default-column-width { proportion 0.5; }
    default-window-height { proportion 0.55; }
}
```
