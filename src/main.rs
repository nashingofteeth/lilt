mod browse;
mod config;
mod cover;
mod logs;
mod mount;
mod mpv;
mod nts;
mod platform;
mod player;
mod queue;
mod tui;
mod visualizer;

#[cfg(test)]
mod test_support;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use std::process::exit;

use config::Config;

#[derive(Parser, Debug)]
#[command(name = "lilt", version, about = "mpv music player wrapper")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Mount the music library in the background
    Mount,
    /// Unmount the library, quitting mpv if library tracks are in use
    Unmount,
    /// Show mount, mpv, and now-playing state
    Status,
    /// Replace the queue with SOURCE (local path, URL, NTS station; whole library if omitted)
    Play {
        #[arg(value_name = "SOURCE", num_args = 0..)]
        paths: Vec<String>,
        #[arg(long, short = 's')]
        shuffle: bool,
    },
    /// Append local paths, URLs, or NTS stations to the queue
    Add {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<String>,
    },
    /// Pick artists and albums with fzf
    Browse,
    /// List the current queue
    #[command(visible_alias = "list")]
    Queue,
    /// Open the playing URL or reveal the playing file
    Open,
    /// Toggle pause
    Toggle,
    /// Pause playback
    Pause,
    /// Resume playback
    Resume,
    /// Next queue entry
    Next,
    /// Previous queue entry
    Prev,
    /// Stop playback, keep mpv running
    Stop,
    /// Mute, unmute, or toggle mute
    Mute { mode: Option<String> },
    /// Jump to queue number N (1-based)
    Jump { number: usize },
    /// Set or adjust volume (N, +N, -N)
    Volume {
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Seek absolute seconds, relative seconds, or percent
    Seek {
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Shuffle the queue
    Shuffle { mode: Option<String> },
    /// Repeat mode
    Repeat { mode: Option<String> },
    /// Stop and exit the mpv daemon
    Quit,
    /// Full-screen control panel
    #[command(alias = "controls")]
    Panel,
    /// Show rclone/mpv logs
    Logs {
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Quit mpv and unmount the library
    Restart,
}

fn main() {
    // Behave like a normal Unix tool when output is piped to `head`.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    if let Err(error) = run() {
        eprintln!("lilt: {error:#}");
        exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    let config = Config::load()?;
    match command {
        Command::Mount => mount::mount(&config),
        Command::Unmount => mount::unmount(&config),
        Command::Status => player::status(&config),
        Command::Play { paths, shuffle } => player::play(&config, &paths, shuffle),
        Command::Add { paths } => player::add(&config, &paths),
        Command::Browse => browse::browse(&config),
        Command::Queue => player::queue(&config),
        Command::Open => player::open(&config),
        Command::Toggle => player::toggle(&config),
        Command::Pause => player::pause(&config),
        Command::Resume => player::resume(&config),
        Command::Next => player::next(&config),
        Command::Prev => player::prev(&config),
        Command::Stop => player::stop(&config),
        Command::Mute { mode } => player::mute(&config, mode.as_deref()),
        Command::Jump { number } => player::jump(&config, number),
        Command::Volume { value } => player::volume(&config, &value),
        Command::Seek { value } => player::seek(&config, &value),
        Command::Shuffle { mode } => player::shuffle(&config, mode.as_deref()),
        Command::Repeat { mode } => player::repeat(&config, mode.as_deref()),
        Command::Quit => player::quit(&config),
        Command::Panel => {
            mpv::ensure_daemon(&config)?;
            tui::run(&config)
        }
        Command::Logs { follow } => logs::show(&config, follow),
        Command::Restart => player::restart(&config),
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::Cli;
    use crate::test_support::{Rng, cases};
    use clap::Parser;

    #[test]
    fn fuzz_cli_parsing_never_panics() {
        let pool = [
            "play",
            "add",
            "browse",
            "volume",
            "seek",
            "shuffle",
            "repeat",
            "status",
            "panel",
            "controls",
            "mount",
            "unmount",
            "restart",
            "logs",
            "--help",
            "-h",
            "--version",
            "-s",
            "--shuffle",
            "-5",
            "+10",
            "50%",
            "NaN",
            "inf",
            "--",
            "",
            "a b",
            "🎵",
            "-",
            "\\",
            "..",
            "~",
        ];
        let mut rng = Rng::new(0xC0FFEE);

        for _ in 0..cases(2_000) {
            let mut args = vec!["lilt".to_string()];
            for _ in 0..rng.below(7) {
                if rng.boolean() {
                    let word: &str = rng.pick(&pool);
                    args.push(word.to_string());
                } else {
                    args.push(rng.string(30));
                }
            }
            let _ = Cli::try_parse_from(args);
        }
    }
}
