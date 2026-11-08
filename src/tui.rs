use anyhow::{Context, Result, anyhow};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::browse;
use crate::config::Config;
use crate::mount;
use crate::mpv::{self, Mpv, PlaylistEntry, Repeat, Status};
use crate::player;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    NowPlaying,
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Play,
    Add,
}

pub fn run(config: &Config) -> Result<()> {
    let mut app = App::new(config.clone());
    // Opening the panel with nothing queued should go straight to the add
    // picker, so the first thing you see is a library to choose from.
    if let Err(error) = auto_add_if_empty(config) {
        app.show_message(format!("{error}"));
    }
    app.refresh();

    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    let _guard = TermGuard;

    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        old_hook(info);
    }));

    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

fn auto_add_if_empty(config: &Config) -> Result<()> {
    let Ok(mut client) = Mpv::connect(&config.socket) else {
        return Ok(());
    };
    let Ok(status) = client.status() else {
        return Ok(());
    };
    if status.playlist_count > 0 {
        return Ok(());
    }
    if let Some(paths) = browse::pick(config, false)? {
        player::append_paths(config, &paths)?;
    }
    Ok(())
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
    pending: Option<Pending>,
    filter_query: String,
    filter_editing: bool,
    filtered: Vec<usize>,
    help_visible: bool,
    pending_g: Option<Instant>,
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
            pending: None,
            filter_query: String::new(),
            filter_editing: false,
            filtered: Vec::new(),
            help_visible: false,
            pending_g: None,
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

            if let Some(action) = self.pending.take() {
                self.run_pending(terminal, action)?;
            }

            if let Some(started) = self.pending_g
                && started.elapsed() >= Duration::from_millis(500)
            {
                self.pending_g = None;
                self.goto_first();
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
            self.apply_filter();
        }
    }

    fn apply_filter(&mut self) {
        let previous_actual = self
            .list_state
            .selected()
            .and_then(|index| self.filtered.get(index).copied());
        let query = self.filter_query.to_lowercase();

        self.filtered = if query.is_empty() {
            (0..self.items.len()).collect()
        } else {
            self.items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.to_lowercase().contains(&query))
                .map(|(index, _)| index)
                .collect()
        };

        if self.filtered.is_empty() {
            self.list_state.select(None);
            return;
        }

        let target = previous_actual
            .filter(|actual| self.filtered.contains(actual))
            .or_else(|| {
                if self.status.playlist_pos >= 0 {
                    let position = self.status.playlist_pos as usize;
                    self.filtered.iter().position(|&index| index == position)
                } else {
                    None
                }
            })
            .unwrap_or(0);
        self.list_state.select(Some(target));
    }

    fn select_current(&mut self) {
        if self.filtered.is_empty() {
            self.list_state.select(None);
            return;
        }
        let position = if self.status.playlist_pos >= 0 {
            let current = self.status.playlist_pos as usize;
            self.filtered.iter().position(|&index| index == current)
        } else {
            None
        };
        self.list_state.select(Some(position.unwrap_or(0)));
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
            if self.filter_editing {
                self.clear_filter();
            } else if self.help_visible {
                self.help_visible = false;
            } else {
                self.quit = true;
            }
            return;
        }

        if self.filter_editing {
            self.handle_filter_key(key);
            return;
        }

        if self.help_visible {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.help_visible = false,
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('Q') => {
                    self.quit_mpv();
                    self.quit = true;
                }
                _ => {}
            }
            return;
        }

        if self.pending_g.is_some() {
            self.pending_g = None;
            if key.code == KeyCode::Char('g') {
                self.goto_first();
                return;
            }
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') if !ctrl => {
                self.quit = true;
                return;
            }
            KeyCode::Char('Q') if !ctrl => {
                self.quit_mpv();
                self.quit = true;
                return;
            }
            KeyCode::Tab => {
                self.toggle_view();
                return;
            }
            KeyCode::Char('?') => {
                self.help_visible = true;
                return;
            }
            KeyCode::Char('/') => {
                self.open_filter();
                return;
            }
            KeyCode::Char('o') | KeyCode::Char('b') if !ctrl => {
                self.pending = Some(Pending::Play);
                return;
            }
            KeyCode::Char('a') if !ctrl => {
                self.pending = Some(Pending::Add);
                return;
            }
            KeyCode::Char('g') if !ctrl => {
                self.pending_g = Some(Instant::now());
                return;
            }
            KeyCode::Char('G') if !ctrl => {
                self.goto_last();
                return;
            }
            _ => {}
        }

        match self.view {
            View::NowPlaying => self.handle_now_playing(key),
            View::Queue => self.handle_queue(key),
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.clear_filter(),
            KeyCode::Enter => self.filter_editing = false,
            KeyCode::Backspace => {
                self.filter_query.pop();
                self.apply_filter();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_query.clear();
                self.apply_filter();
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.filter_query.push(character);
                self.apply_filter();
            }
            _ => {}
        }
    }

    fn clear_filter(&mut self) {
        self.filter_query.clear();
        self.filter_editing = false;
        self.apply_filter();
    }

    fn open_filter(&mut self) {
        if self.view == View::NowPlaying {
            self.view = View::Queue;
            self.reload_playlist();
        }
        self.filter_query.clear();
        self.filter_editing = true;
        self.apply_filter();
    }

    fn toggle_view(&mut self) {
        self.view = match self.view {
            View::NowPlaying => View::Queue,
            View::Queue => View::NowPlaying,
        };
        self.filter_editing = false;
        if self.view == View::Queue {
            self.reload_playlist();
            self.select_current();
        } else {
            self.filter_query.clear();
            self.apply_filter();
        }
    }

    fn quit_mpv(&mut self) {
        let _ = mpv::quit(&self.config, true);
        self.mpv = None;
        self.status = Status::default();
    }

    fn play_index(&mut self, index: i64) {
        self.act(move |client| {
            client.command_values(vec![json!("playlist-play-index"), json!(index)])?;
            Ok(())
        });
    }

    fn goto_first(&mut self) {
        match self.view {
            View::Queue => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            View::NowPlaying => self.play_index(0),
        }
    }

    fn goto_last(&mut self) {
        match self.view {
            View::Queue => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(self.filtered.len() - 1));
                }
            }
            View::NowPlaying => {
                self.act(|client| {
                    let count = client
                        .get("playlist-count")
                        .ok()
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0);
                    if count > 0 {
                        client
                            .command_values(vec![json!("playlist-play-index"), json!(count - 1)])?;
                    }
                    Ok(())
                });
            }
        }
    }

    fn handle_now_playing(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(' ') => {
                self.act(|client| client.command(&["cycle", "pause"]));
            }
            KeyCode::Char('n') | KeyCode::Char('j') => {
                self.act(|client| {
                    if client.command(&["playlist-next"]).is_err() {
                        let _ = client.command_values(vec![json!("playlist-play-index"), json!(0)]);
                    }
                    Ok(())
                });
            }
            KeyCode::Char('p') | KeyCode::Char('k') => {
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
            KeyCode::Left | KeyCode::Char('h') => {
                self.act(|client| {
                    client.command_values(vec![json!("seek"), json!(-10.0), json!("relative")])?;
                    Ok(())
                });
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.act(|client| {
                    client.command_values(vec![json!("seek"), json!(10.0), json!("relative")])?;
                    Ok(())
                });
            }
            KeyCode::Up | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.act(|client| {
                    client.command_values(vec![json!("add"), json!("volume"), json!(5.0)])?;
                    Ok(())
                });
            }
            KeyCode::Down | KeyCode::Char('-') | KeyCode::Char('_') => {
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
        if key.code == KeyCode::Esc {
            if self.filter_query.is_empty() {
                self.view = View::NowPlaying;
            } else {
                self.clear_filter();
            }
            return;
        }

        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        let half_page = 10usize;
        let page = 20usize;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.list_state.select(Some(selected.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.list_state.select(Some((selected + 1).min(len - 1)));
            }
            KeyCode::PageUp => {
                self.list_state.select(Some(selected.saturating_sub(page)));
            }
            KeyCode::PageDown => {
                self.list_state.select(Some((selected + page).min(len - 1)));
            }
            KeyCode::Char('u') if ctrl => {
                self.list_state
                    .select(Some(selected.saturating_sub(half_page)));
            }
            KeyCode::Char('d') if ctrl => {
                self.list_state
                    .select(Some((selected + half_page).min(len - 1)));
            }
            KeyCode::Char('b') if ctrl => {
                self.list_state.select(Some(selected.saturating_sub(page)));
            }
            KeyCode::Char('f') if ctrl => {
                self.list_state.select(Some((selected + page).min(len - 1)));
            }
            KeyCode::Home => {
                self.list_state.select(Some(0));
            }
            KeyCode::End => {
                self.list_state.select(Some(len - 1));
            }
            KeyCode::Enter => {
                let actual = self.filtered[selected] as i64;
                self.play_index(actual);
                self.view = View::NowPlaying;
                self.clear_filter();
            }
            _ => {}
        }
    }

    /// Suspend the TUI, run the fzf picker, then rebuild the terminal.
    fn run_pending(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        action: Pending,
    ) -> Result<()> {
        ratatui::restore();

        let result = (|| -> Result<Option<&'static str>> {
            match action {
                Pending::Play => {
                    if let Some(paths) = browse::pick(&self.config, true)? {
                        browse::play_paths(&self.config, &paths)?;
                        Ok(Some("playing new selection"))
                    } else {
                        Ok(None)
                    }
                }
                Pending::Add => {
                    if let Some(paths) = browse::pick(&self.config, false)? {
                        player::append_paths(&self.config, &paths)?;
                        Ok(Some("added to queue"))
                    } else {
                        Ok(None)
                    }
                }
            }
        })();

        *terminal = ratatui::try_init().context("reinitializing terminal")?;
        terminal.clear()?;

        match result {
            Ok(Some(message)) => {
                self.show_message(message.to_string());
                self.refresh();
            }
            Ok(None) => {}
            Err(error) => self.show_message(format!("{error}")),
        }
        Ok(())
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

        if self.help_visible {
            self.draw_help_overlay(frame, area);
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
        let label_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let gauge = Gauge::default()
            .block(Block::bordered().title("progress"))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(Span::styled(label, label_style));
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
            Constraint::Length(1), // header
            Constraint::Length(1), // filter prompt
            Constraint::Min(0),    // list
            Constraint::Length(2), // help
            Constraint::Length(1), // message
        ])
        .split(area);

        let header = if self.filter_query.is_empty() {
            format!("Queue — {} track(s)", self.items.len())
        } else {
            format!(
                "Queue — {}/{} track(s)",
                self.filtered.len(),
                self.items.len()
            )
        };
        frame.render_widget(
            Paragraph::new(header).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            chunks[0],
        );

        let filter_line = if self.filter_editing {
            Paragraph::new(format!("/{}█", self.filter_query))
                .style(Style::default().fg(Color::Yellow))
        } else if self.filter_query.is_empty() {
            Paragraph::new("")
        } else {
            Paragraph::new(format!(
                "filter: {}   ({} match(es))   / edit   Esc clear",
                self.filter_query,
                self.filtered.len()
            ))
            .style(Style::default().fg(Color::DarkGray))
        };
        frame.render_widget(filter_line, chunks[1]);

        if self.filtered.is_empty() {
            let message = if self.items.is_empty() {
                "queue empty"
            } else {
                "no matches"
            };
            frame.render_widget(
                Paragraph::new(message)
                    .alignment(Alignment::Center)
                    .block(Block::bordered()),
                chunks[2],
            );
        } else {
            let items: Vec<ListItem> = self
                .filtered
                .iter()
                .filter_map(|&index| self.items.get(index))
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
            frame.render_stateful_widget(list, chunks[2], &mut self.list_state);
        }

        let help = vec![
            Line::from("j/k move   Ctrl-d/u half   Ctrl-f/b page   Enter play"),
            Line::from("/ filter   o play   a add   Tab back   Esc clear/back   ? help"),
        ];
        frame.render_widget(Paragraph::new(help), chunks[3]);
        self.draw_message(frame, chunks[4]);
    }

    fn draw_help_overlay(&self, frame: &mut Frame, area: Rect) {
        let popup = centered_rect(78, 82, area);
        frame.render_widget(Clear, popup);

        let lines = vec![
            Line::from(Span::styled(
                "Now Playing",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  space pause        j/k next/prev      h/l seek 10s"),
            Line::from("  -/+ volume         g/gg/G first/last  s shuffle"),
            Line::from("  r repeat           m mute             o play picker"),
            Line::from("  a add picker       Tab queue          / filter queue"),
            Line::from("  q detach           Q quit mpv         ? close help"),
            Line::from(""),
            Line::from(Span::styled(
                "Queue",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k move           Ctrl-d/u half page Ctrl-f/b page"),
            Line::from("  Enter play         / filter           Esc clear/back"),
            Line::from("  o play picker      a add picker       Tab back"),
            Line::from("  q detach           Q quit mpv         ? close help"),
        ];

        let paragraph = Paragraph::new(lines)
            .block(Block::bordered().title(" keys "))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, popup);
    }

    fn draw_help(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let help = if area.width >= 70 {
            vec![
                Line::from("space pause   j/k next/prev   h/l seek 10s   -/+ volume"),
                Line::from("g/G first/last   s/r/m   o play   a add   Tab queue   ? help"),
            ]
        } else {
            vec![
                Line::from("space j/k h/l -/+ g/G"),
                Line::from("s r m o a Tab ? q/Q"),
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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
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
            let filtered: Vec<usize> = (0..count).filter(|_| rng.boolean()).collect();
            let selected = if filtered.is_empty() {
                None
            } else {
                Some(rng.below(filtered.len()))
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
                pending: None,
                filter_query: rng.string(20),
                filter_editing: rng.boolean(),
                filtered,
                help_visible: rng.boolean(),
                pending_g: if rng.boolean() {
                    Some(Instant::now())
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

#[cfg(test)]
mod key_tests {
    use super::*;
    use crate::test_support::{test_config, unique_base};
    use std::path::PathBuf;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app_with_items(items: &[&str]) -> (App, PathBuf) {
        let base = unique_base("tui-keys");
        let config = test_config(base.join("Music"), &base);
        let mut app = App::new(config);
        app.items = items.iter().map(|item| item.to_string()).collect();
        app.playlist = items
            .iter()
            .map(|item| PlaylistEntry {
                filename: item.to_string(),
                title: Some(item.to_string()),
            })
            .collect();
        app.filtered = (0..items.len()).collect();
        app.view = View::Queue;
        app.list_state.select(Some(0));
        (app, base)
    }

    #[test]
    fn filter_edits_clears_and_maps_selection() {
        let (mut app, base) = app_with_items(&["alpha", "beta", "gamma"]);

        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.filter_editing);

        app.handle_key(key(KeyCode::Char('e')));
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.filter_query, "et");
        assert_eq!(app.filtered, vec![1]);

        app.handle_key(key(KeyCode::Enter));
        assert!(!app.filter_editing);
        assert_eq!(app.filter_query, "et");

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.filter_query, "");
        assert_eq!(app.filtered, vec![0, 1, 2]);
        assert_eq!(app.view, View::Queue);

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::NowPlaying);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn help_overlay_toggles_and_swallows_keys() {
        let (mut app, base) = app_with_items(&["one", "two"]);

        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.help_visible);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(0));

        app.handle_key(key(KeyCode::Esc));
        assert!(!app.help_visible);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn gg_and_g_move_to_first_and_last() {
        let (mut app, base) = app_with_items(&["a", "b", "c"]);

        app.list_state.select(Some(2));
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.list_state.selected(), Some(2));
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.list_state.selected(), Some(0));

        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.list_state.selected(), Some(2));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn picker_keys_set_pending_action() {
        let (mut app, base) = app_with_items(&["a"]);

        app.handle_key(key(KeyCode::Char('o')));
        assert_eq!(app.pending, Some(Pending::Play));
        app.pending = None;

        app.handle_key(key(KeyCode::Char('a')));
        assert_eq!(app.pending, Some(Pending::Add));

        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod page_key_tests {
    use super::*;
    use crate::test_support::{test_config, unique_base};
    use std::path::PathBuf;

    fn app_with_items(items: &[String]) -> (App, PathBuf) {
        let base = unique_base("tui-pages");
        let config = test_config(base.join("Music"), &base);
        let mut app = App::new(config);
        app.items = items.to_vec();
        app.playlist = items
            .iter()
            .map(|item| PlaylistEntry {
                filename: item.clone(),
                title: Some(item.clone()),
            })
            .collect();
        app.filtered = (0..items.len()).collect();
        app.view = View::Queue;
        app.list_state.select(Some(0));
        (app, base)
    }

    fn ctrl(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_page_keys_move_selection() {
        let items: Vec<String> = (0..30).map(|index| format!("track {index}")).collect();
        let (mut app, base) = app_with_items(&items);

        app.handle_key(ctrl('f'));
        assert_eq!(app.list_state.selected(), Some(20));
        app.handle_key(ctrl('b'));
        assert_eq!(app.list_state.selected(), Some(0));
        app.handle_key(ctrl('d'));
        assert_eq!(app.list_state.selected(), Some(10));
        app.handle_key(ctrl('u'));
        assert_eq!(app.list_state.selected(), Some(0));

        let _ = std::fs::remove_dir_all(&base);
    }
}
