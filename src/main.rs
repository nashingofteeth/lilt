mod browse;
mod config;
mod logs;
mod mount;
mod mpv;
mod player;
mod queue;
mod tui;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use std::process::exit;

use config::Config;

#[derive(Parser, Debug)]
#[command(name = "music", version, about = "mpv music player wrapper")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Mount the music library in the foreground
    Mount,
    /// Quit mpv and unmount the library
    Unmount,
    /// Show mount, mpv, and now-playing state
    Status,
    /// Replace the queue with PATH (whole library if omitted)
    Play {
        path: Option<String>,
        #[arg(long, short = 's')]
        shuffle: bool,
    },
    /// Append artists/albums/tracks to the queue
    Add {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<String>,
    },
    /// Pick artists and albums with fzf
    Browse,
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
    /// Single-key control panel
    Controls,
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
        eprintln!("music: {error:#}");
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
        Command::Mount => mount::mount_foreground(&config),
        Command::Unmount => mount::unmount(&config),
        Command::Status => player::status(&config),
        Command::Play { path, shuffle } => player::play(&config, path.as_deref(), shuffle),
        Command::Add { paths } => player::add(&config, &paths),
        Command::Browse => browse::browse(&config),
        Command::Toggle => player::toggle(&config),
        Command::Pause => player::pause(&config),
        Command::Resume => player::resume(&config),
        Command::Next => player::next(&config),
        Command::Prev => player::prev(&config),
        Command::Stop => player::stop(&config),
        Command::Volume { value } => player::volume(&config, &value),
        Command::Seek { value } => player::seek(&config, &value),
        Command::Shuffle { mode } => player::shuffle(&config, mode.as_deref()),
        Command::Repeat { mode } => player::repeat(&config, mode.as_deref()),
        Command::Quit => player::quit(&config),
        Command::Controls => {
            mpv::ensure_daemon(&config)?;
            tui::run(&config)
        }
        Command::Logs { follow } => logs::show(&config, follow),
        Command::Restart => player::restart(&config),
    }
}
