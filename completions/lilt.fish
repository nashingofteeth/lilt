# Fish completions for lilt(1).
complete -c lilt -f

set -l __lilt_commands \
    mount unmount status \
    play add browse queue list open toggle pause resume next prev stop mute jump \
    volume seek shuffle repeat quit restart controls logs help

complete -c lilt -n '__fish_use_subcommand' -a mount -d 'Mount the music library'
complete -c lilt -n '__fish_use_subcommand' -a unmount -d 'Quit mpv and unmount'
complete -c lilt -n '__fish_use_subcommand' -a status -d 'Show mount and playback state'
complete -c lilt -n '__fish_use_subcommand' -a play -d 'Replace the queue with paths, URLs, or NTS stations'
complete -c lilt -n '__fish_use_subcommand' -a add -d 'Append local paths, URLs, or NTS stations'
complete -c lilt -n '__fish_use_subcommand' -a browse -d 'Pick artists, albums, or NTS with fzf'
complete -c lilt -n '__fish_use_subcommand' -a queue -d 'List the current queue'
complete -c lilt -n '__fish_use_subcommand' -a open -d 'Open the playing URL or reveal the file'
complete -c lilt -n '__fish_use_subcommand' -a list -d 'List the current queue (alias)'
complete -c lilt -n '__fish_use_subcommand' -a toggle -d 'Toggle pause'
complete -c lilt -n '__fish_use_subcommand' -a pause -d 'Pause playback'
complete -c lilt -n '__fish_use_subcommand' -a resume -d 'Resume playback'
complete -c lilt -n '__fish_use_subcommand' -a next -d 'Next queue entry'
complete -c lilt -n '__fish_use_subcommand' -a prev -d 'Previous queue entry'
complete -c lilt -n '__fish_use_subcommand' -a stop -d 'Stop playback'
complete -c lilt -n '__fish_use_subcommand' -a mute -d 'Mute, unmute, or toggle mute'
complete -c lilt -n '__fish_use_subcommand' -a jump -d 'Jump to a queue number'
complete -c lilt -n '__fish_use_subcommand' -a volume -d 'Set or adjust volume'
complete -c lilt -n '__fish_use_subcommand' -a seek -d 'Seek in the current track'
complete -c lilt -n '__fish_use_subcommand' -a shuffle -d 'Shuffle the queue'
complete -c lilt -n '__fish_use_subcommand' -a repeat -d 'Set repeat mode'
complete -c lilt -n '__fish_use_subcommand' -a quit -d 'Exit the mpv daemon'
complete -c lilt -n '__fish_use_subcommand' -a restart -d 'Quit mpv and unmount'
complete -c lilt -n '__fish_use_subcommand' -a controls -d 'Single-key control panel'
complete -c lilt -n '__fish_use_subcommand' -a logs -d 'Show rclone/mpv logs'
complete -c lilt -n '__fish_use_subcommand' -a help -d 'Show usage'

complete -c lilt -n "__fish_seen_subcommand_from $__lilt_commands" -l help -d 'Show usage'

complete -c lilt -n '__fish_seen_subcommand_from play add' -a '(__fish_complete_path)' -d 'Album, artist, or track'
complete -c lilt -n '__fish_seen_subcommand_from play' -l shuffle -d 'Shuffle after loading'

complete -c lilt -n '__fish_seen_subcommand_from volume' -a '+5\tIncrease by 5'
complete -c lilt -n '__fish_seen_subcommand_from volume' -a '-5\tDecrease by 5'

complete -c lilt -n '__fish_seen_subcommand_from seek' -a '+10\tForward 10s'
complete -c lilt -n '__fish_seen_subcommand_from seek' -a '-10\tBack 10s'

complete -c lilt -n '__fish_seen_subcommand_from shuffle' -a 'on\tEnable shuffle'
complete -c lilt -n '__fish_seen_subcommand_from shuffle' -a 'off\tDisable shuffle'
complete -c lilt -n '__fish_seen_subcommand_from shuffle' -a 'toggle\tToggle shuffle'

complete -c lilt -n '__fish_seen_subcommand_from mute' -a 'on\tMute'
complete -c lilt -n '__fish_seen_subcommand_from mute' -a 'off\tUnmute'
complete -c lilt -n '__fish_seen_subcommand_from mute' -a 'toggle\tToggle mute'

complete -c lilt -n '__fish_seen_subcommand_from repeat' -a 'off\tNo repeat'
complete -c lilt -n '__fish_seen_subcommand_from repeat' -a 'all\tRepeat the queue'
complete -c lilt -n '__fish_seen_subcommand_from repeat' -a 'one\tRepeat the current track'

complete -c lilt -n '__fish_seen_subcommand_from logs' -a '-f\tFollow the logs'
