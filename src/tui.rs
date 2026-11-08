use anyhow::{Context, Result, anyhow};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::mount;
use crate::mpv::{self, Mpv, PlaylistEntry, Repeat, Status};
use crate::player;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    NowPlaying,
    Queue,
}

pub fn run(config: &Config) -> Result<()> {
    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    let _guard = TermGuard;

    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        old_hook(info);
    }));

    let mut app = App::new(config.clone());
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

struct App {
    config: Config,
    mpv: Option<Mpv>,
    status: Status,
    view: View,
    playlist: Vec<PlaylistEntry>,
    items: Vec<String>,
    list_state: ListState,
    mounted: bool,
    last_poll: Instant,
    message: Option<(String, Instant)>,
    quit: bool,
}

impl App {
    fn new(config: Config) -> Self {
        let mpv = Mpv::connect(&config.socket).ok();
        let mut app = Self {
            mounted: mount::is_mounted(&config.music_dir),
            config,
            mpv,
            status: Status::default(),
            view: View::NowPlaying,
            playlist: Vec::new(),
            items: Vec::new(),
            list_state: ListState::default(),
            last_poll: Instant::now() - Duration::from_secs(1),
            message: None,
            quit: false,
        };
        app.refresh();
        app
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            if self.last_poll.elapsed() >= Duration::from_millis(100) {
                self.refresh();
            }

            if self.quit {
                return Ok(());
            }
        }
    }

    fn refresh(&mut self) {
        self.last_poll = Instant::now();
        self.mounted = mount::is_mounted(&self.config.music_dir);

        if self.mpv.is_none() {
            self.mpv = Mpv::connect(&self.config.socket).ok();
        }

        let previous_pos = self.status.playlist_pos;
        let previous_count = self.status.playlist_count;

        if let Some(client) = self.mpv.as_mut() {
            match client.status() {
                Ok(status) => self.status = status,
                Err(_) => {
                    self.mpv = None;
                    self.status = Status::default();
                }
            }
        } else {
            self.status = Status::default();
        }

        if self.view == View::Queue
            && (self.playlist.is_empty()
                || previous_count != self.status.playlist_count
                || previous_pos != self.status.playlist_pos)
        {
            self.reload_playlist();
        }
    }

    fn reload_playlist(&mut self) {
        let entries = match self.client() {
            Some(client) => client.playlist().ok(),
            None => None,
        };
        if let Some(entries) = entries {
            self.playlist = entries;
            self.rebuild_items();
            let default = if self.status.playlist_pos >= 0 {
                self.status.playlist_pos as usize
            } else {
                0
            };
            let selected = self.list_state.selected().unwrap_or(default);
            if !self.items.is_empty() {
                self.list_state
                    .select(Some(selected.min(self.items.len() - 1)));
            }
        }
    }

    fn select_current(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let selected = if self.status.playlist_pos >= 0 {
            self.status.playlist_pos as usize
        } else {
            0
        };
        self.list_state
            .select(Some(selected.min(self.items.len() - 1)));
    }

    fn rebuild_items(&mut self) {
        self.items = self
            .playlist
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let current = index as i64 == self.status.playlist_pos;
                let marker = if current { "●" } else { " " };
                let name = entry
                    .title
                    .clone()
                    .unwrap_or_else(|| display_name(&entry.filename));
                format!("{marker} {:>4}  {name}", index + 1)
            })
            .collect();
    }

    fn client(&mut self) -> Option<&mut Mpv> {
        if self.mpv.is_none() {
            self.mpv = Mpv::connect(&self.config.socket).ok();
        }
        self.mpv.as_mut()
    }

    fn act<F>(&mut self, action: F)
    where
        F: FnOnce(&mut Mpv) -> Result<()>,
    {
        let result = match self.client() {
            Some(client) => action(client),
            None => Err(anyhow!("mpv is not running")),
        };
        if let Err(err) = result {
            self.mpv = None;
            self.show_message(format!("{err}"));
        }
    }

    fn show_message(&mut self, message: String) {
        self.message = Some((message, Instant::now()));
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return;
            }
            KeyCode::Char('Q') => {
                let _ = mpv::quit(&self.config, true);
                self.mpv = None;
                self.status = Status::default();
                self.quit = true;
                return;
            }
            KeyCode::Tab => {
                self.view = match self.view {
                    View::NowPlaying => View::Queue,
                    View::Queue => View::NowPlaying,
                };
                if self.view == View::Queue {
                    self.reload_playlist();
                    self.select_current();
                }
                return;
            }
            KeyCode::Esc if self.view == View::Queue => {
                self.view = View::NowPlaying;
                return;
            }
            _ => {}
        }

        match self.view {
            View::NowPlaying => self.handle_now_playing(key),
            View::Queue => self.handle_queue(key),
        }
    }

    fn handle_now_playing(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(' ') => {
                self.act(|client| client.command(&["cycle", "pause"]));
            }
            KeyCode::Char('n') => {
                self.act(|client| {
                    if client.command(&["playlist-next"]).is_err() {
                        let _ = client.command_values(vec![json!("playlist-play-index"), json!(0)]);
                    }
                    Ok(())
                });
            }
            KeyCode::Char('p') => {
                self.act(|client| {
                    if client.command(&["playlist-prev"]).is_err() {
                        let count = client
                            .get("playlist-count")
                            .ok()
                            .and_then(|value| value.as_i64())
                            .unwrap_or(0);
                        if count > 0 {
                            let _ = client.command_values(vec![
                                json!("playlist-play-index"),
                                json!(count - 1),
                            ]);
                        }
                    }
                    Ok(())
                });
            }
            KeyCode::Left => {
                self.act(|client| {
                    client.command_values(vec![json!("seek"), json!(-10.0), json!("relative")])?;
                    Ok(())
                });
            }
            KeyCode::Right => {
                self.act(|client| {
                    client.command_values(vec![json!("seek"), json!(10.0), json!("relative")])?;
                    Ok(())
                });
            }
            KeyCode::Up => {
                self.act(|client| {
                    client.command_values(vec![json!("add"), json!("volume"), json!(5.0)])?;
                    Ok(())
                });
            }
            KeyCode::Down => {
                self.act(|client| {
                    client.command_values(vec![json!("add"), json!("volume"), json!(-5.0)])?;
                    Ok(())
                });
            }
            KeyCode::Char('s') => {
                self.act(|client| client.command(&["cycle", "shuffle"]));
            }
            KeyCode::Char('r') => {
                self.act(|client| {
                    let current = client.status()?.repeat;
                    let next = match current {
                        Repeat::Off => Repeat::All,
                        Repeat::All => Repeat::One,
                        Repeat::One => Repeat::Off,
                    };
                    player::set_repeat(client, next)
                });
            }
            KeyCode::Char('m') => {
                self.act(|client| client.command(&["cycle", "mute"]));
            }
            _ => {}
        }
    }

    fn handle_queue(&mut self, key: KeyEvent) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let next = selected.saturating_sub(1);
                self.list_state.select(Some(next));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = (selected + 1).min(len - 1);
                self.list_state.select(Some(next));
            }
            KeyCode::PageUp => {
                let next = selected.saturating_sub(10);
                self.list_state.select(Some(next));
            }
            KeyCode::PageDown => {
                let next = (selected + 10).min(len - 1);
                self.list_state.select(Some(next));
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.list_state.select(Some(0));
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.list_state.select(Some(len - 1));
            }
            KeyCode::Enter => {
                let index = selected;
                self.act(move |client| {
                    client
                        .command_values(vec![json!("playlist-play-index"), json!(index as i64)])?;
                    Ok(())
                });
                self.view = View::NowPlaying;
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 40 || area.height < 12 {
            let paragraph = Paragraph::new("terminal too small (need 40x12)")
                .alignment(Alignment::Center)
                .block(Block::bordered());
            frame.render_widget(paragraph, area);
            return;
        }

        let block = Block::bordered()
            .title(" music ")
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        match self.view {
            View::NowPlaying => self.draw_now_playing(frame, inner),
            View::Queue => self.draw_queue(frame, inner),
        }
    }

    fn draw_now_playing(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);

        let context = if self.status.artist.is_empty() && self.status.album.is_empty() {
            "mpv".to_string()
        } else if self.status.album.is_empty() {
            self.status.artist.clone()
        } else if self.status.artist.is_empty() {
            self.status.album.clone()
        } else {
            format!("{} — {}", self.status.artist, self.status.album)
        };

        let title_text = if self.status.media_title.is_empty() {
            "Nothing playing"
        } else {
            self.status.media_title.as_str()
        };
        let title = if self.status.paused {
            format!("|| {title_text}")
        } else {
            title_text.to_string()
        };

        let header = Paragraph::new(vec![
            Line::from(Span::styled(
                context,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ]);
        frame.render_widget(header, chunks[0]);

        let ratio = match (self.status.time_pos, self.status.duration) {
            (Some(position), Some(duration)) if duration > 0.0 => {
                (position / duration).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        let ratio = if ratio.is_finite() { ratio } else { 0.0 };
        let label = if self.status.idle {
            "idle".to_string()
        } else {
            format!(
                "{} / {}",
                player::format_time(self.status.time_pos),
                player::format_time(self.status.duration)
            )
        };
        let gauge = Gauge::default()
            .block(Block::bordered().title("progress"))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(label);
        frame.render_widget(gauge, chunks[1]);

        let queue = if self.status.playlist_count > 0 && self.status.playlist_pos >= 0 {
            format!(
                "  ({}/{})",
                self.status.playlist_pos + 1,
                self.status.playlist_count
            )
        } else {
            String::new()
        };
        let buffering = if self.status.paused_for_cache {
            "  buffering…"
        } else {
            ""
        };
        let muted = if self.status.mute { "   muted" } else { "" };
        let state = format!(
            "vol {:.0}%   shuffle {}   repeat {}{}{}{}",
            self.status.volume.floor(),
            if self.status.shuffle { "on" } else { "off" },
            player::repeat_label(self.status.repeat),
            queue,
            buffering,
            muted
        );
        frame.render_widget(
            Paragraph::new(state).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );

        if !self.mounted {
            let warning = Paragraph::new("library not mounted — run `music mount`")
                .style(Style::default().fg(Color::Yellow))
                .alignment(Alignment::Center);
            frame.render_widget(warning, chunks[3]);
        } else if self.status.idle && self.status.media_title.is_empty() {
            let hint = Paragraph::new("queue empty — `music play` or Tab for the queue")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(hint, chunks[3]);
        }

        self.draw_help(frame, chunks[4]);
        self.draw_message(frame, chunks[5]);
    }

    fn draw_queue(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);

        let header = format!("Queue — {} track(s)", self.items.len());
        frame.render_widget(
            Paragraph::new(header).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            chunks[0],
        );

        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|item| ListItem::new(item.as_str()))
            .collect();
        let list = List::new(items)
            .block(Block::bordered())
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, chunks[1], &mut self.list_state);

        let help = vec![
            Line::from("↑/↓ or j/k move   PgUp/PgDn   Enter play   Tab/Esc back"),
            Line::from("q detach   Q quit mpv"),
        ];
        frame.render_widget(Paragraph::new(help), chunks[2]);
        self.draw_message(frame, chunks[3]);
    }

    fn draw_help(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let help = if area.width >= 70 {
            vec![
                Line::from("space pause   n/p next/prev   ←/→ seek 10s   ↑/↓ volume 5"),
                Line::from("s shuffle   r repeat   m mute   Tab queue   q detach   Q quit"),
            ]
        } else {
            vec![
                Line::from("space n/p ←/→ ↑/↓ s r m"),
                Line::from("Tab queue   q detach   Q quit"),
            ]
        };
        frame.render_widget(
            Paragraph::new(help)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn draw_message(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        if let Some((message, at)) = &self.message {
            if at.elapsed() < Duration::from_secs(3) {
                frame.render_widget(
                    Paragraph::new(message.as_str()).style(Style::default().fg(Color::Yellow)),
                    area,
                );
                return;
            }
            self.message = None;
        }
        frame.render_widget(Paragraph::new(""), area);
    }
}

fn display_name(filename: &str) -> String {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(filename);
    let tail = stem.rsplit(" - ").next().unwrap_or(stem);
    let cleaned = tail.trim_start_matches(|character: char| {
        character.is_ascii_digit() || matches!(character, ' ' | '.' | '_' | '-')
    });
    if cleaned.is_empty() {
        stem.to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;
    use crate::test_support::{Rng, cases};

    #[test]
    fn fuzz_display_name_never_panics() {
        let mut rng = Rng::new(7);
        for _ in 0..cases(5_000) {
            let input = rng.string(100);
            let _ = display_name(&input);
        }
    }
}

#[cfg(test)]
mod layout_fuzz_tests {
    use super::*;
    use crate::test_support::{Rng, cases, test_config, unique_base};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn fuzz_tui_rendering_never_panics() {
        let mut rng = Rng::new(0xBEEF);
        let iterations = cases(500).min(2_000);
        let base = unique_base("tui-layout");
        let config = test_config(base.join("Music"), &base);

        for _ in 0..iterations {
            let width = (rng.below(120) + 1) as u16;
            let height = (rng.below(60) + 1) as u16;
            let count = rng.below(30);
            let items: Vec<String> = (0..count)
                .map(|index| format!("● {:>4}  {}", index + 1, rng.string(60)))
                .collect();
            let playlist: Vec<PlaylistEntry> = (0..count)
                .map(|_| PlaylistEntry {
                    filename: rng.string(30),
                    title: Some(rng.string(30)),
                })
                .collect();
            let selected = if items.is_empty() {
                None
            } else {
                Some(rng.below(items.len()))
            };

            let mut app = App {
                config: config.clone(),
                mpv: None,
                status: Status {
                    idle: rng.boolean(),
                    media_title: rng.string(80),
                    artist: rng.string(40),
                    album: rng.string(40),
                    time_pos: if rng.boolean() {
                        Some(rng.f64_bits())
                    } else {
                        None
                    },
                    duration: if rng.boolean() {
                        Some(rng.f64_bits())
                    } else {
                        None
                    },
                    paused: rng.boolean(),
                    volume: rng.f64_bits(),
                    shuffle: rng.boolean(),
                    repeat: match rng.below(3) {
                        0 => Repeat::Off,
                        1 => Repeat::All,
                        _ => Repeat::One,
                    },
                    playlist_pos: rng.below(35) as i64 - 1,
                    playlist_count: rng.below(35) as i64,
                    paused_for_cache: rng.boolean(),
                    mute: rng.boolean(),
                },
                view: if rng.boolean() {
                    View::NowPlaying
                } else {
                    View::Queue
                },
                playlist,
                items,
                list_state: ListState::default().with_selected(selected),
                mounted: rng.boolean(),
                last_poll: Instant::now(),
                message: if rng.boolean() {
                    Some((rng.string(50), Instant::now()))
                } else {
                    None
                },
                quit: false,
            };

            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }

        let _ = std::fs::remove_dir_all(&base);
    }
}
