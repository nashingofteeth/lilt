# music

A small wrapper around [mpv](https://mpv.io/) for playing a music library that
lives behind an [rclone](https://rclone.org/) mount.

It does three things:

- mounts the library (`rclone mount`, read-only) when asked,
- keeps one detached, audio-only mpv process running with an IPC socket,
- forwards simple commands (`play`, `next`, `pause`, `seek`, …) to that process
  from any terminal.

Because mpv stays alive in the background, playback continues after the
terminal that started it is closed, and the next command works from anywhere.
If [`mpv-mpris`](https://github.com/hoyon/mpv-mpris) is installed, desktop media
keys and MPRIS panels work too.

## Requirements

- `bash`, `mpv`, `rclone`, `python3`, `jq`
- `fzf` for `music browse`
- `fusermount3` (util-linux) for `music unmount`
- optional: `mpv-mpris` for hardware media keys

Linux only: the script uses `mountpoint`, `fusermount3`, and
`XDG_RUNTIME_DIR`.

## Install

Clone the repo to `~/music` and put the executable on `PATH`:

```sh
git clone <repo-url> ~/music
ln -s ../../music/bin/music ~/.local/bin/music
ln -s ../../../music/completions/music.fish ~/.config/fish/completions/music.fish
```

(The symlinks are relative, so a different `$HOME` works as long as the repo
is cloned to `~/music`.)

## Usage

Run `music` with no arguments to mount the library in the foreground
(`Ctrl-C` unmounts). Everything else:

```
Library
  music mount                    Mount in the foreground
  music unmount                  Stop playback, then unmount
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
  music controls                 Single-key control panel
  music quit                     Stop and exit mpv
  music logs [-f]                Tail rclone/mpv logs
```

`PATH` may be absolute, relative to the current directory, or relative to the
music mount — so `music play "Brian Eno"` and
`music play "Brian Eno/Ambient 1"` both work. Matching falls back to a
case-insensitive search under the mount.

### Control panel

`music controls` draws a status line and reads single keys:

| Key | Action |
| --- | --- |
| `space` | pause/resume |
| `n` / `p` | next / previous |
| `←` / `→` | seek −10s / +10s |
| `↑` / `↓` | volume +5 / −5 |
| `s` | toggle shuffle |
| `r` | cycle repeat (off → all → one) |
| `q` | detach (playback continues) |
| `Q` | quit mpv |

The panel uses the terminal's alternate screen, redraws only changed lines, and
truncates to the window width so long titles never wrap. On exit it restores
the previous screen.

## Configuration

Defaults:

```sh
MUSIC_DIR="$HOME/Music"
MUSIC_REMOTE="sam:/Volumes/NASH_SYNC/music/library"
```

Override them with environment variables or `~/.config/music/config` (a shell
fragment sourced by the script):

```sh
MUSIC_REMOTE="nas:/music"
MUSIC_DIR="$HOME/Music"
```

## How it works

- `rclone mount <remote> <MUSIC_DIR> --read-only`
- one mpv process:
  `mpv --idle --no-video --no-terminal --input-ipc-server=…/mpv.sock`
- commands are JSON messages sent to the socket by an embedded Python helper;
  `status` requests player properties in one batch and formats them with `jq`
- state lives outside the repo:
  - socket: `${XDG_RUNTIME_DIR:-/tmp}/music-mount-$UID/mpv.sock`
  - logs and pid files: `~/.cache/music/`

mpv runs with `--directory-mode=recursive --directory-filter-types=audio`, so
passing an artist or album directory queues its audio files and ignores cover
art.

## Desktop integration

Example [niri](https://github.com/YaLTeR/niri) binding for a floating fzf
picker:

```kdl
Mod+M { spawn-sh "ghostty --class=music-browse -e $HOME/.local/bin/music browse"; }
```

```kdl
window-rule {
    match app-id=r#"^music-browse$"#
    open-floating true
    default-column-width { proportion 0.6; }
    default-window-height { proportion 0.7; }
}
```
