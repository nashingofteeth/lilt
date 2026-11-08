use anyhow::{Context, Result, anyhow};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::SetTitle;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Bar, BarChart, BarGroup, Block, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph,
    Wrap,
};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::browse;
use crate::config::Config;
use crate::mount;
use crate::mpv::{self, Mpv, PlaylistEntry, Repeat, Status};
use crate::player;
use crate::visualizer::Visualizer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
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
    let _ = execute!(std::io::stdout(), SetTitle("music controls"));
    let _guard = TermGuard;

    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        old_hook(info);
    }));

    app.start_visualizer();
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
    focus: Focus,
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
    visualizer: Option<Visualizer>,
    visualizer_enabled: bool,
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
            focus: Focus::NowPlaying,
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
            visualizer: None,
            visualizer_enabled: true,
            quit: false,
        };
        app.refresh();
        app
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            let timeout = if self.visualizer_enabled && self.visualizer.is_some() {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(100)
            };
            if event::poll(timeout)? {
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

        if self.playlist.is_empty()
            || previous_count != self.status.playlist_count
            || previous_pos != self.status.playlist_pos
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
            if self.focus == Focus::NowPlaying {
                self.select_current();
            }
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
                self.toggle_focus();
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
            KeyCode::Char('v') if !ctrl => {
                self.toggle_visualizer();
                return;
            }
            KeyCode::Char(' ') => {
                self.toggle_pause();
                return;
            }
            KeyCode::Char('n') if !ctrl => {
                self.next_track();
                return;
            }
            KeyCode::Char('p') if !ctrl => {
                self.prev_track();
                return;
            }
            KeyCode::Char('h') if !ctrl => {
                self.seek_relative(-10.0);
                return;
            }
            KeyCode::Char('l') if !ctrl => {
                self.seek_relative(10.0);
                return;
            }
            KeyCode::Char('+') | KeyCode::Char('=') if !ctrl => {
                self.adjust_volume(5.0);
                return;
            }
            KeyCode::Char('-') | KeyCode::Char('_') if !ctrl => {
                self.adjust_volume(-5.0);
                return;
            }
            KeyCode::Char('s') if !ctrl => {
                self.cycle_shuffle();
                return;
            }
            KeyCode::Char('r') if !ctrl => {
                self.cycle_repeat();
                return;
            }
            KeyCode::Char('m') if !ctrl => {
                self.cycle_mute();
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::NowPlaying => self.handle_now_playing(key),
            Focus::Queue => self.handle_queue(key),
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
            KeyCode::Tab => {
                self.filter_editing = false;
                self.toggle_focus();
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
        if self.playlist.is_empty() {
            self.reload_playlist();
        }
        self.focus = Focus::Queue;
        self.filter_query.clear();
        self.filter_editing = true;
        self.apply_filter();
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::NowPlaying => Focus::Queue,
            Focus::Queue => Focus::NowPlaying,
        };
        self.filter_editing = false;
        self.pending_g = None;
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
        match self.focus {
            Focus::Queue => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            Focus::NowPlaying => self.play_index(0),
        }
    }

    fn goto_last(&mut self) {
        match self.focus {
            Focus::Queue => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(self.filtered.len() - 1));
                }
            }
            Focus::NowPlaying => {
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
            KeyCode::Char('j') => self.next_track(),
            KeyCode::Char('k') => self.prev_track(),
            KeyCode::Left => self.seek_relative(-10.0),
            KeyCode::Right => self.seek_relative(10.0),
            KeyCode::Up => self.adjust_volume(5.0),
            KeyCode::Down => self.adjust_volume(-5.0),
            _ => {}
        }
    }

    fn toggle_pause(&mut self) {
        self.act(|client| client.command(&["cycle", "pause"]));
    }

    fn next_track(&mut self) {
        self.act(|client| {
            if client.command(&["playlist-next"]).is_err() {
                let _ = client.command_values(vec![json!("playlist-play-index"), json!(0)]);
            }
            Ok(())
        });
    }

    fn prev_track(&mut self) {
        self.act(|client| {
            if client.command(&["playlist-prev"]).is_err() {
                let count = client
                    .get("playlist-count")
                    .ok()
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                if count > 0 {
                    let _ =
                        client.command_values(vec![json!("playlist-play-index"), json!(count - 1)]);
                }
            }
            Ok(())
        });
    }

    fn seek_relative(&mut self, seconds: f64) {
        self.act(move |client| {
            client.command_values(vec![json!("seek"), json!(seconds), json!("relative")])?;
            Ok(())
        });
    }

    fn adjust_volume(&mut self, delta: f64) {
        self.act(move |client| {
            client.command_values(vec![json!("add"), json!("volume"), json!(delta)])?;
            Ok(())
        });
    }

    fn cycle_shuffle(&mut self) {
        self.act(|client| client.command(&["cycle", "shuffle"]));
    }

    fn cycle_repeat(&mut self) {
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

    fn cycle_mute(&mut self) {
        self.act(|client| client.command(&["cycle", "mute"]));
    }

    fn start_visualizer(&mut self) {
        if self.visualizer_enabled && self.visualizer.is_none() {
            self.visualizer = Some(Visualizer::start());
        }
    }

    fn toggle_visualizer(&mut self) {
        self.visualizer_enabled = !self.visualizer_enabled;
        if self.visualizer_enabled {
            self.visualizer = Some(Visualizer::start());
        } else {
            self.visualizer = None;
        }
    }

    fn handle_queue(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            if self.filter_query.is_empty() {
                self.focus = Focus::NowPlaying;
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

        let show_visualizer = self.visualizer_enabled && area.height >= 20;
        let mut constraints = vec![Constraint::Length(6)];
        if show_visualizer {
            constraints.push(Constraint::Length(if area.height >= 30 { 8 } else { 5 }));
        }
        constraints.push(Constraint::Min(3));
        constraints.push(Constraint::Length(2));
        constraints.push(Constraint::Length(1));
        let chunks = Layout::vertical(constraints).split(area);

        let now_index = 0;
        let viz_index = if show_visualizer { Some(1) } else { None };
        let queue_index = if show_visualizer { 2 } else { 1 };
        let help_index = queue_index + 1;
        let message_index = help_index + 1;

        self.draw_now_playing(frame, chunks[now_index]);
        if let Some(index) = viz_index {
            self.draw_spectrum_pane(frame, chunks[index]);
        }
        self.draw_queue(frame, chunks[queue_index]);
        self.draw_help(frame, chunks[help_index]);
        self.draw_message(frame, chunks[message_index]);

        if self.help_visible {
            self.draw_help_overlay(frame, area);
        }
    }

    fn draw_now_playing(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::NowPlaying;
        let block = Block::bordered()
            .title(" now playing ")
            .border_style(border_style(focused))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

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

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                context,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))),
            lines[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            ))),
            lines[1],
        );

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
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(Span::styled(label, label_style));
        frame.render_widget(gauge, lines[2]);

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
            lines[3],
        );
    }

    fn draw_spectrum_pane(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(" spectrum ")
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (bands, available) = match &self.visualizer {
            Some(visualizer) => (visualizer.bands(), visualizer.available()),
            None => (Vec::new(), false),
        };
        render_spectrum(frame, inner, &bands, available);
    }

    fn draw_queue(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Queue;
        let title = if self.filter_query.is_empty() {
            format!(" queue — {} track(s) ", self.items.len())
        } else {
            format!(
                " queue — {}/{} track(s) ",
                self.filtered.len(),
                self.items.len()
            )
        };
        let block = Block::bordered()
            .title(title)
            .border_style(border_style(focused))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let filter_visible = self.filter_editing || !self.filter_query.is_empty();
        let chunks = Layout::vertical([
            Constraint::Length(if filter_visible { 1 } else { 0 }),
            Constraint::Min(0),
        ])
        .split(inner);

        if filter_visible {
            let line = if self.filter_editing {
                Paragraph::new(format!("/{}█", self.filter_query))
                    .style(Style::default().fg(Color::Yellow))
            } else {
                Paragraph::new(format!(
                    "filter: {}   ({} match(es))   / edit   Esc clear",
                    self.filter_query,
                    self.filtered.len()
                ))
                .style(Style::default().fg(Color::DarkGray))
            };
            frame.render_widget(line, chunks[0]);
        }

        if self.filtered.is_empty() {
            let message = if self.items.is_empty() {
                "queue empty — press `a` to add"
            } else {
                "no matches"
            };
            frame.render_widget(
                Paragraph::new(message)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::DarkGray)),
                chunks[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .filtered
                .iter()
                .filter_map(|&index| self.items.get(index))
                .map(|item| ListItem::new(item.as_str()))
                .collect();
            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
        }
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
            Line::from("  a add picker       v visualizer       Tab focus queue"),
            Line::from("  / filter queue     q detach           Q quit mpv"),
            Line::from("  ? close help"),
            Line::from(""),
            Line::from(Span::styled(
                "Queue",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k move           g/gg/G first/last  Ctrl-d/u half page"),
            Line::from("  Ctrl-f/b page      Enter play         / filter"),
            Line::from("  Esc clear/back     o play picker      a add picker"),
            Line::from("  v visualizer       Tab focus now playing"),
            Line::from("  q detach           Q quit mpv         ? close help"),
        ];

        let paragraph = Paragraph::new(lines)
            .block(Block::bordered().title(" keys "))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, popup);
    }

    fn draw_help(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let help = match self.focus {
            Focus::NowPlaying => {
                if area.width >= 70 {
                    vec![
                        Line::from(
                            "space pause   j/k next/prev   h/l seek 10s   -/+ volume   g/G first/last",
                        ),
                        Line::from(
                            "s/r/m   o play   a add   v viz   Tab queue   / filter   ? help",
                        ),
                    ]
                } else {
                    vec![
                        Line::from("space j/k h/l -/+ g/G"),
                        Line::from("s r m o a v Tab ? q/Q"),
                    ]
                }
            }
            Focus::Queue => {
                if area.width >= 70 {
                    vec![
                        Line::from(
                            "j/k move   g/G first/last   Ctrl-d/u half   Ctrl-f/b page   Enter play",
                        ),
                        Line::from(
                            "space pause   / filter   o/a pick   v viz   Tab now-playing   ? help",
                        ),
                    ]
                } else {
                    vec![
                        Line::from("j/k g/G ^d/^u ^f/^b Enter"),
                        Line::from("space / o a v Tab ? q/Q"),
                    ]
                }
            }
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

        if !self.mounted {
            frame.render_widget(
                Paragraph::new("library not mounted — run `music mount`")
                    .style(Style::default().fg(Color::Yellow)),
                area,
            );
            return;
        }

        if self.status.idle && self.status.media_title.is_empty() {
            frame.render_widget(
                Paragraph::new("queue empty — press `a` to add")
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
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

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_spectrum(frame: &mut Frame, area: Rect, bands: &[f32], available: bool) {
    if area.is_empty() {
        return;
    }
    if !available || bands.is_empty() {
        frame.render_widget(
            Paragraph::new("visualizer unavailable (parec)")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let width = area.width as usize;
    if width == 0 {
        return;
    }
    let mut count = width.div_ceil(2).clamp(4, 64).min(bands.len());
    while count > 1 && count * 2 - 1 > width {
        count -= 1;
    }

    let bars: Vec<Bar> = (0..count)
        .map(|index| {
            let start = index * bands.len() / count;
            let end = ((index + 1) * bands.len() / count)
                .max(start + 1)
                .min(bands.len());
            let peak = bands[start..end]
                .iter()
                .copied()
                .filter(|value| value.is_finite())
                .fold(0.0f32, f32::max);
            let value = (peak.clamp(0.0, 1.0) * 100.0).round() as u64;
            Bar::new(value).style(band_style(index, count))
        })
        .collect();

    let chart = BarChart::default()
        .data(BarGroup::default().bars(&bars))
        .bar_width(1)
        .bar_gap(1)
        .max(100);
    frame.render_widget(chart, area);
}

fn band_style(index: usize, count: usize) -> Style {
    let t = if count <= 1 {
        0.0
    } else {
        index as f32 / (count - 1) as f32
    };
    let (from, to, amount) = if t < 0.5 {
        ((0, 200, 255), (0, 255, 120), t * 2.0)
    } else {
        ((0, 255, 120), (255, 0, 200), (t - 0.5) * 2.0)
    };
    let (red, green, blue) = lerp_rgb(from, to, amount);
    Style::default().fg(Color::Rgb(red, green, blue))
}

fn lerp_rgb(from: (u8, u8, u8), to: (u8, u8, u8), amount: f32) -> (u8, u8, u8) {
    let mix = |start: u8, end: u8| {
        (start as f32 + (end as f32 - start as f32) * amount.clamp(0.0, 1.0)) as u8
    };
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
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
                focus: if rng.boolean() {
                    Focus::NowPlaying
                } else {
                    Focus::Queue
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
                visualizer: None,
                visualizer_enabled: rng.boolean(),
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
        app.focus = Focus::Queue;
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
        assert_eq!(app.focus, Focus::Queue);

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::NowPlaying);

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
    fn tab_switches_focus_and_enter_keeps_queue_focus() {
        let (mut app, base) = app_with_items(&["a", "b"]);

        assert_eq!(app.focus, Focus::Queue);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::NowPlaying);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Queue);

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Queue);

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
        app.focus = Focus::Queue;
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

#[cfg(test)]
mod spectrum_fuzz_tests {
    use super::*;
    use crate::test_support::{Rng, cases};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn fuzz_spectrum_rendering_never_panics() {
        let mut rng = Rng::new(0xABC);
        let iterations = cases(500).min(1_000);
        for _ in 0..iterations {
            let width = (rng.below(120) + 1) as u16;
            let height = (rng.below(20) + 1) as u16;
            let bands: Vec<f32> = (0..64).map(|_| rng.f64_bits() as f32).collect();
            let available = rng.boolean();

            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render_spectrum(frame, frame.area(), &bands, available))
                .unwrap();
        }
    }
}
