use crate::{
    discovery::AgentKey,
    tmux::{Client, sanitize},
    tracker::{self, Activity, NavResult, Navigation, Preview, Row, Snapshot, Tracker},
};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use std::{io, process::Command, sync::Arc, time::Duration};

struct TerminalGuard {
    active: bool,
}
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        let mut guard = Self { active: false };
        guard.resume()?;
        Ok(guard)
    }
    fn resume(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        self.active = true;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(())
    }
    fn restore(&mut self) -> io::Result<()> {
        if self.active {
            self.active = false;
            let raw = terminal::disable_raw_mode();
            let screen = execute!(io::stdout(), LeaveAlternateScreen, Show);
            raw.and(screen)?;
        }
        Ok(())
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Details,
    Preview,
}
impl Focus {
    fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::List, true) | (Self::Preview, false) => Self::Details,
            (Self::Details, true) | (Self::List, false) => Self::Preview,
            _ => Self::List,
        }
    }
}
struct App {
    snapshot: Arc<Snapshot>,
    visible: Vec<usize>,
    selected: Option<AgentKey>,
    index: usize,
    list_state: ListState,
    focus: Focus,
    filter: String,
    editing: Option<String>,
    help: bool,
    error: Option<String>,
    scroll: usize,
    follow: bool,
    preview_height: usize,
    list_height: usize,
    navigation: Option<Navigation>,
    chooser: Option<(Vec<Client>, usize)>,
}
impl App {
    fn new() -> Self {
        Self {
            snapshot: Arc::new(Snapshot::default()),
            visible: Vec::new(),
            selected: None,
            index: 0,
            list_state: ListState::default(),
            focus: Focus::List,
            filter: String::new(),
            editing: None,
            help: false,
            error: None,
            scroll: 0,
            follow: true,
            preview_height: 1,
            list_height: 1,
            navigation: None,
            chooser: None,
        }
    }
    fn row(&self) -> Option<&Row> {
        self.visible
            .get(self.index)
            .and_then(|i| self.snapshot.rows.get(*i))
    }
    fn reconcile(&mut self) {
        let filter = self.filter.to_lowercase();
        self.visible.clear();
        self.visible.extend(
            self.snapshot
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    filter.is_empty()
                        || format!(
                            "{} {} {}",
                            row.key.product,
                            row.cwd
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                            row.pane
                                .as_ref()
                                .map(|p| p.session_name.as_str())
                                .unwrap_or("")
                        )
                        .to_lowercase()
                        .contains(&filter)
                })
                .map(|(i, _)| i),
        );
        let prior = self.selected;
        self.index = self
            .selected
            .and_then(|key| {
                self.visible
                    .iter()
                    .position(|i| self.snapshot.rows[*i].key == key)
            })
            .unwrap_or(self.index)
            .min(self.visible.len().saturating_sub(1));
        self.selected = self.row().map(|r| r.key);
        self.list_state.select(self.selected.map(|_| self.index));
        if prior != self.selected {
            self.reset_preview();
        }
    }
    fn reset_preview(&mut self) {
        self.scroll = 0;
        self.follow = true;
    }
    fn move_selection(&mut self, index: usize) {
        let previous = self.selected;
        self.index = index.min(self.visible.len().saturating_sub(1));
        self.selected = self.row().map(|r| r.key);
        self.list_state.select(self.selected.map(|_| self.index));
        if previous != self.selected {
            self.reset_preview();
        }
    }
    fn preview_text(&self) -> &str {
        match &self.snapshot.preview {
            Some((key, preview)) if Some(*key) == self.selected => match preview {
                Preview::Text(text) => text.trim_end_matches('\n'),
                Preview::NoPane => "No tmux pane available",
                Preview::Error(error) => error,
            },
            _ if self.row().and_then(|r| r.pane.as_ref()).is_some() => "Loading pane output",
            _ => "No tmux pane available",
        }
    }
    fn max_scroll(&self) -> usize {
        self.preview_text()
            .lines()
            .count()
            .saturating_sub(self.preview_height)
    }
    fn input(&mut self, key: KeyEvent, tracker: &Tracker) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.help {
            match key.code {
                KeyCode::Char('q') => return true,
                KeyCode::Esc | KeyCode::Char('?') => self.help = false,
                _ => {}
            }
            return false;
        }
        if self.editing.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.editing = None;
                }
                KeyCode::Esc => {
                    self.filter = self.editing.take().unwrap();
                    self.reconcile();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.reconcile();
                }
                KeyCode::Char(c) if !control => {
                    self.filter.push(c);
                    self.reconcile();
                }
                _ => {}
            }
            return false;
        }
        if let Some((clients, index)) = &mut self.chooser {
            match key.code {
                KeyCode::Esc => {
                    self.chooser = None;
                    self.navigation = None;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('j') | KeyCode::Down => {
                    *index = (*index + 1).min(clients.len().saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                KeyCode::Enter => {
                    if let Some(mut request) = self.navigation.clone() {
                        request.client = Some(clients[*index].name.clone());
                        tracker.navigate(request);
                        self.chooser = None;
                        self.error = Some("Validating selected client…".into());
                    }
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => self.editing = Some(self.filter.clone()),
            KeyCode::Char('r') => {
                tracker.refresh();
                self.error = None;
            }
            KeyCode::Tab | KeyCode::Char('l') => self.focus = self.focus.cycle(true),
            KeyCode::BackTab | KeyCode::Char('h') => self.focus = self.focus.cycle(false),
            KeyCode::Enter => {
                if self.navigation.is_none() {
                    if let Some(row) = self.row() {
                        let request = Navigation {
                            key: row.key,
                            pane: row.pane.clone(),
                            client: None,
                        };
                        tracker.navigate(request.clone());
                        self.navigation = Some(request);
                        self.error = Some("Validating live agent and terminal…".into());
                    } else {
                        self.error = Some("No agent selected".into());
                    }
                }
            }
            code => {
                let page = if self.focus == Focus::Preview {
                    self.preview_height
                } else {
                    self.list_height / 2 // Each agent occupies two grid rows.
                } / 2;
                let delta = match code {
                    KeyCode::Char('j') | KeyCode::Down => Some(1isize),
                    KeyCode::Char('k') | KeyCode::Up => Some(-1),
                    KeyCode::Char('d') if control => Some(page.max(1) as isize),
                    KeyCode::Char('u') if control => Some(-(page.max(1) as isize)),
                    _ => None,
                };
                if self.focus == Focus::List {
                    match code {
                        KeyCode::Char('g') => self.move_selection(0),
                        KeyCode::Char('G') => {
                            self.move_selection(self.visible.len().saturating_sub(1))
                        }
                        _ => {
                            if let Some(delta) = delta {
                                self.move_selection(self.index.saturating_add_signed(delta));
                            }
                        }
                    }
                } else if self.focus == Focus::Preview {
                    let max = self.max_scroll();
                    if self.follow {
                        self.scroll = max;
                    }
                    match code {
                        KeyCode::Char('g') => self.scroll = 0,
                        KeyCode::Char('G') => self.scroll = max,
                        _ => {
                            if let Some(delta) = delta {
                                self.scroll = self.scroll.saturating_add_signed(delta).min(max);
                            }
                        }
                    }
                    self.follow = self.scroll == max;
                }
            }
        }
        false
    }
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 60 || area.height < 16 {
            frame.render_widget(
                Paragraph::new(
                    "Resize terminal to at least 60 columns × 16 rows. q: quit  ?: help",
                ),
                area,
            );
            if self.help {
                self.draw_help(frame, area);
            }
            return;
        }
        let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let content = parts[0];
        let (list, details, preview) = if area.width >= 100 {
            let halves =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .split(content);
            let right =
                Layout::vertical([Constraint::Length(8), Constraint::Min(1)]).split(halves[1]);
            (halves[0], right[0], right[1])
        } else {
            let stack = Layout::vertical([
                Constraint::Percentage(35),
                Constraint::Length(8),
                Constraint::Min(1),
            ])
            .split(content);
            (stack[0], stack[1], stack[2])
        };
        self.list_height = list.height.saturating_sub(2) as usize;
        self.preview_height = preview.height.saturating_sub(2).max(1) as usize;
        let list_block = self.block("Agents", Focus::List);
        if self.visible.is_empty() {
            let empty = if self.filter.is_empty() {
                "No supported agents running"
            } else {
                "No matching agents"
            };
            frame.render_widget(Paragraph::new(empty).block(list_block), list);
        } else {
            let items: Vec<_> = self
                .visible
                .iter()
                .map(|i| {
                    let row = &self.snapshot.rows[*i];
                    let color = match row.activity {
                        Activity::OutputActive => Color::Green,
                        Activity::Quiet => Color::Yellow,
                        Activity::Running => Color::Reset,
                    };
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::raw(format!("{} #{}  ", row.key.product, row.key.root_pid)),
                            Span::styled(
                                row.attention.unwrap_or_else(|| row.activity.label()),
                                Style::default().fg(if row.attention.is_some() {
                                    Color::Magenta
                                } else {
                                    color
                                }),
                            ),
                        ]),
                        Line::raw(
                            sanitize(&format!("{} · {}", row.project(), row.location()))
                                .replace('\n', "�"),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(list_block)
                    .highlight_symbol("› ")
                    .highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                    ),
                list,
                &mut self.list_state,
            );
        }
        let detail = if let Some(row) = self.row() {
            let age = tracker::now().saturating_sub(row.key.start_time);
            let cwd = row
                .cwd
                .as_ref()
                .map(|p| sanitize(&p.display().to_string()).replace('\n', "�"))
                .unwrap_or_else(|| "unavailable".into());
            let explanation = if let Some(reason) = row.attention {
                reason
            } else if row.pane.as_ref().is_some_and(|p| p.last_output.is_some()) {
                "Shared-pane output; not agent intent"
            } else {
                "Output activity unavailable; process is alive"
            };
            let pane = row
                .pane
                .as_ref()
                .map(|p| {
                    sanitize(&format!(
                        "{} / {}  {}",
                        p.session_name, p.window_name, p.pane_id
                    ))
                    .replace('\n', "�")
                })
                .unwrap_or_else(|| "outside tmux".into());
            format!(
                "{}  PID {}  age {}s\nOS state: {}\ncwd: {}\n{} — {}\n{}\n{}",
                row.key.product,
                row.key.root_pid,
                age,
                row.state,
                cwd,
                row.activity.label(),
                explanation,
                pane,
                row.pane.as_ref().map(|p| p.target()).unwrap_or_default()
            )
        } else {
            "Select an agent".into()
        };
        frame.render_widget(
            Paragraph::new(sanitize(&detail)).block(self.block("Details", Focus::Details)),
            details,
        );
        let max = self.max_scroll();
        if self.follow {
            self.scroll = max;
        } else {
            self.scroll = self.scroll.min(max);
        }
        frame.render_widget(
            Paragraph::new(self.preview_text())
                .scroll((self.scroll.min(u16::MAX as usize) as u16, 0))
                .block(self.block("Pane output", Focus::Preview)),
            preview,
        );
        let footer = if self.editing.is_some() {
            format!("/{}  Enter accept · Esc cancel", self.filter)
        } else {
            let error = self
                .error
                .as_deref()
                .or(self.snapshot.error.as_deref())
                .or(self.snapshot.notification_error.as_deref());
            match error {
                Some(error) => format!("q quit · ? help | {}", sanitize(error).replace('\n', " ")),
                None => format!(
                    "j/k move · h/l focus · / filter · Enter jump · r refresh · ? help · q quit{}",
                    if self.filter.is_empty() {
                        String::new()
                    } else {
                        format!(" | /{}", self.filter)
                    }
                ),
            }
        };
        frame.render_widget(Paragraph::new(footer), parts[1]);
        if self.help {
            self.draw_help(frame, content);
        }
        if let Some((clients, index)) = &self.chooser {
            let popup = popup(content, 80, (clients.len() + 4).min(20) as u16);
            frame.render_widget(Clear, popup);
            let mut state = ListState::default().with_selected(Some(*index));
            let items = clients
                .iter()
                .map(|c| ListItem::new(sanitize(&format!("{}  {}", c.name, c.session_id))))
                .collect::<Vec<_>>();
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Choose tracker client · j/k Enter Esc"),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::REVERSED),
                    ),
                popup,
                &mut state,
            );
        }
    }
    fn block<'a>(&self, title: &'a str, focus: Focus) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(if self.focus == focus {
                Color::Cyan
            } else {
                Color::Reset
            }))
    }
    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let popup = popup(area, 96, 21);
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new("j/k or arrows: move list / scroll preview\ng/G: first/last or preview top/bottom\nCtrl-D/U: half page · h/l or Tab/Shift-Tab: focus\n/: product/cwd/session substring filter\nEnter accepts filter; Escape restores prior filter\nEnter: jump to selected live agent, from any focus\nr: scan now and refresh selected output\n?: help · Escape/?: close help · q/Ctrl-C: quit\n\nrunning: alive, output activity unavailable\noutput active: pane traffic within the last 3 seconds\nquiet: older/no recorded pane traffic\nActivity belongs to the shared pane, not agent intent.\nIt does not prove approval, completion, or success.\nOS stopped/sleep/run state is shown separately.\n\ntmux shares active window/pane state within sessions.\nTargeting one client does not isolate shared selections.").block(Block::default().title("Help").borders(Borders::ALL)),popup);
    }
}
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
pub(crate) fn run() -> io::Result<()> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        previous(info);
    }));
    let mut guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let tracker = Tracker::new();
    let mut app = App::new();
    let mut dirty = true;
    loop {
        let (snapshot, navigation) = tracker.take();
        if let Some(snapshot) = snapshot {
            if *snapshot != *app.snapshot {
                app.snapshot = snapshot;
                app.reconcile();
                tracker.select(app.selected);
                dirty = true;
            }
        }
        if let Some(result) = navigation {
            match result {
                NavResult::Error(error) => {
                    app.error = Some(error);
                    app.navigation = None;
                }
                NavResult::Focused => {
                    guard.restore()?;
                    return Ok(());
                }
                NavResult::Choose(clients) => {
                    app.chooser = Some((clients, 0));
                    app.error = None;
                }
                NavResult::Ready { target, client } => {
                    guard.restore()?;
                    let mut command = Command::new("tmux");
                    command.arg("-N");
                    let result = if let Some(client) = client {
                        command.args(["switch-client", "-c", &client, "-Z", "-t", &target]);
                        command
                            .output()
                            .map(|o| (o.status, String::from_utf8_lossy(&o.stderr).into_owned()))
                    } else {
                        command.args(["attach-session", "-t", &target]);
                        command
                            .stdin(std::process::Stdio::inherit())
                            .stdout(std::process::Stdio::inherit())
                            .stderr(std::process::Stdio::piped());
                        command
                            .output()
                            .map(|o| (o.status, String::from_utf8_lossy(&o.stderr).into_owned()))
                    };
                    match result {
                        Ok((status, _)) if status.success() => return Ok(()),
                        other => {
                            guard.resume()?;
                            terminal.clear()?;
                            app.error = Some(match other {
                                Ok((status, error)) => {
                                    if error.trim().is_empty() {
                                        format!("tmux exited with {status}")
                                    } else {
                                        error
                                    }
                                }
                                Err(e) => e.to_string(),
                            });
                            app.navigation = None;
                            tracker.refresh();
                        }
                    }
                }
            }
            dirty = true;
        }
        if dirty {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.input(key, &tracker) {
                        break;
                    }
                    tracker.select(app.selected);
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
    }
    guard.restore()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Product;
    fn row(pid: u32) -> Row {
        Row {
            key: AgentKey {
                product: Product::Codex,
                root_pid: pid,
                start_time: 10,
            },
            cwd: Some(format!("/work/{pid}").into()),
            state: "Run".into(),
            pane: None,
            activity: Activity::Running,
            attention: None,
        }
    }
    #[test]
    fn recent_output_is_not_hidden_by_empty_grid_rows() {
        let mut app = App::new();
        let key = row(1).key;
        app.selected = Some(key);
        app.snapshot = Arc::new(Snapshot {
            preview: Some((
                key,
                Preview::Text(Arc::new(format!("first\n\nLAT_SMOKE\n{}", "\n".repeat(30)))),
            )),
            ..Snapshot::default()
        });
        app.preview_height = 2;
        assert_eq!(app.preview_text(), "first\n\nLAT_SMOKE");
        assert_eq!(app.max_scroll(), 1);
    }
    #[test]
    fn selection_survives_reorder_and_replaces_removed_row_safely() {
        let mut app = App::new();
        app.snapshot = Arc::new(Snapshot {
            rows: vec![row(1), row(2), row(3)],
            ..Snapshot::default()
        });
        app.reconcile();
        app.move_selection(1);
        app.snapshot = Arc::new(Snapshot {
            rows: vec![row(3), row(1), row(2)],
            ..Snapshot::default()
        });
        app.reconcile();
        assert_eq!(app.selected, Some(row(2).key));
        assert_eq!(app.index, 2);
        app.snapshot = Arc::new(Snapshot {
            rows: vec![row(3), row(1)],
            ..Snapshot::default()
        });
        app.reconcile();
        assert_eq!(app.selected, Some(row(1).key));
        app.filter = "/work/3".into();
        app.reconcile();
        assert_eq!(app.selected, Some(row(3).key));
        app.filter = "absent".into();
        app.reconcile();
        assert_eq!(app.selected, None);
    }
}
