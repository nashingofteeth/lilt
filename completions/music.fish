# Fish completions for music(1) (lilt alias).
complete -c music -f

set -l __music_commands \
    mount unmount status \
    play add browse nts queue list open toggle pause resume next prev stop mute jump \
    volume seek shuffle repeat quit restart controls logs help

complete -c music -n '__fish_use_subcommand' -a mount -d 'Mount the music library'
complete -c music -n '__fish_use_subcommand' -a unmount -d 'Quit mpv and unmount'
complete -c music -n '__fish_use_subcommand' -a status -d 'Show mount and playback state'
complete -c music -n '__fish_use_subcommand' -a play -d 'Replace the queue with paths or URLs'
complete -c music -n '__fish_use_subcommand' -a add -d 'Append local paths or URLs'
complete -c music -n '__fish_use_subcommand' -a browse -d 'Pick artists/albums with fzf'
complete -c music -n '__fish_use_subcommand' -a nts -d 'Play NTS live or Infinite Mixtapes'
complete -c music -n '__fish_use_subcommand' -a queue -d 'List the current queue'
complete -c music -n '__fish_use_subcommand' -a open -d 'Open the playing URL or reveal the file'
complete -c music -n '__fish_use_subcommand' -a list -d 'List the current queue (alias)'
complete -c music -n '__fish_use_subcommand' -a toggle -d 'Toggle pause'
complete -c music -n '__fish_use_subcommand' -a pause -d 'Pause playback'
complete -c music -n '__fish_use_subcommand' -a resume -d 'Resume playback'
complete -c music -n '__fish_use_subcommand' -a next -d 'Next queue entry'
complete -c music -n '__fish_use_subcommand' -a prev -d 'Previous queue entry'
complete -c music -n '__fish_use_subcommand' -a stop -d 'Stop playback'
complete -c music -n '__fish_use_subcommand' -a mute -d 'Mute, unmute, or toggle mute'
complete -c music -n '__fish_use_subcommand' -a jump -d 'Jump to a queue number'
complete -c music -n '__fish_use_subcommand' -a volume -d 'Set or adjust volume'
complete -c music -n '__fish_use_subcommand' -a seek -d 'Seek in the current track'
complete -c music -n '__fish_use_subcommand' -a shuffle -d 'Shuffle the queue'
complete -c music -n '__fish_use_subcommand' -a repeat -d 'Set repeat mode'
complete -c music -n '__fish_use_subcommand' -a quit -d 'Exit the mpv daemon'
complete -c music -n '__fish_use_subcommand' -a restart -d 'Quit mpv and unmount'
complete -c music -n '__fish_use_subcommand' -a controls -d 'Single-key control panel'
complete -c music -n '__fish_use_subcommand' -a logs -d 'Show rclone/mpv logs'
complete -c music -n '__fish_use_subcommand' -a help -d 'Show usage'

complete -c music -n "__fish_seen_subcommand_from $__music_commands" -l help -d 'Show usage'

complete -c music -n '__fish_seen_subcommand_from play add' -a '(__fish_complete_path)' -d 'Album, artist, or track'
complete -c music -n '__fish_seen_subcommand_from play' -l shuffle -d 'Shuffle after loading'

complete -c music -n '__fish_seen_subcommand_from volume' -a '+5\tIncrease by 5'
complete -c music -n '__fish_seen_subcommand_from volume' -a '-5\tDecrease by 5'

complete -c music -n '__fish_seen_subcommand_from seek' -a '+10\tForward 10s'
complete -c music -n '__fish_seen_subcommand_from seek' -a '-10\tBack 10s'

complete -c music -n '__fish_seen_subcommand_from shuffle' -a 'on\tEnable shuffle'
complete -c music -n '__fish_seen_subcommand_from shuffle' -a 'off\tDisable shuffle'
complete -c music -n '__fish_seen_subcommand_from shuffle' -a 'toggle\tToggle shuffle'

complete -c music -n '__fish_seen_subcommand_from mute' -a 'on\tMute'
complete -c music -n '__fish_seen_subcommand_from mute' -a 'off\tUnmute'
complete -c music -n '__fish_seen_subcommand_from mute' -a 'toggle\tToggle mute'

complete -c music -n '__fish_seen_subcommand_from repeat' -a 'off\tNo repeat'
complete -c music -n '__fish_seen_subcommand_from repeat' -a 'all\tRepeat the queue'
complete -c music -n '__fish_seen_subcommand_from repeat' -a 'one\tRepeat the current track'

complete -c music -n '__fish_seen_subcommand_from logs' -a '-f\tFollow the logs'
