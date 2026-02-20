pub mod data;
pub mod ui;

use std::time::{Duration, Instant};

use crate::config::Config;

#[derive(Clone, Copy, PartialEq)]
pub enum SortField {
    Name,
    State,
    Age,
    Memory,
}

impl SortField {
    fn next(self) -> SortField {
        match self {
            SortField::Name => SortField::State,
            SortField::State => SortField::Age,
            SortField::Age => SortField::Memory,
            SortField::Memory => SortField::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortField::Name => "Name",
            SortField::State => "State",
            SortField::Age => "Age",
            SortField::Memory => "Memory",
        }
    }
}

pub struct App {
    pub data: data::DashboardData,
    pub selected: usize,
    pub log_scroll: usize,
    pub should_quit: bool,
    pub config: Config,
    pub refresh_interval: Duration,
    pub last_refresh: Instant,
    pub tick_count: u64,
    /// Transient status message shown in the header (auto-clears after 3s).
    pub status_message: Option<(String, Instant)>,
    /// Whether the help overlay is currently shown.
    pub show_help: bool,
    /// Current sort field for workspace list.
    pub sort_by: SortField,
    /// Human-readable selection position, e.g. "[2/5]".
    pub selection_display: String,
    /// Toggle between workspace and team views.
    pub team_mode: bool,
    /// Team data (loaded when team_mode=true).
    pub team_data: data::TeamDashboardData,
    /// Selected team index.
    pub team_selected: usize,
}

impl App {
    fn set_message(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    /// Clear expired status messages (older than 4s).
    fn clear_stale_messages(&mut self) {
        if let Some((_, when)) = &self.status_message {
            if when.elapsed() > Duration::from_secs(4) {
                self.status_message = None;
            }
        }
    }

    /// Update the selection_display string based on current state.
    fn update_selection_display(&mut self) {
        let total = self.data.workspaces.len();
        if total == 0 {
            self.selection_display = "[0/0]".to_string();
        } else {
            self.selection_display = format!("[{}/{}]", self.selected + 1, total);
        }
    }

    /// Sort workspaces by the current sort field.
    fn sort_workspaces(&mut self) {
        match self.sort_by {
            SortField::Name => {
                self.data.workspaces.sort_by(|a, b| a.name.cmp(&b.name));
            }
            SortField::State => {
                self.data.workspaces.sort_by(|a, b| {
                    fn state_order(s: &str) -> u8 {
                        match s {
                            "Running" => 0,
                            "Stopped" => 1,
                            _ => 2,
                        }
                    }
                    state_order(&a.state).cmp(&state_order(&b.state))
                });
            }
            SortField::Age => {
                // Newest first: age strings are tricky, but the data is loaded
                // in created_at order from data.rs. "Newest first" means we
                // reverse the default (oldest first) ordering. We sort by age
                // string length and content as a heuristic — shorter age = newer.
                // A proper approach would store created_at, but we work with what
                // WorkspaceEntry exposes. We reverse the list since data.rs loads
                // oldest-first.
                self.data.workspaces.reverse();
            }
            SortField::Memory => {
                // Highest first. Parse memory strings like "2 GB", "512 MB".
                self.data.workspaces.sort_by(|a, b| {
                    fn parse_mem_mb(s: &str) -> u64 {
                        let parts: Vec<&str> = s.trim().split_whitespace().collect();
                        if parts.len() != 2 {
                            return 0;
                        }
                        let val: f64 = parts[0].parse().unwrap_or(0.0);
                        match parts[1] {
                            "GB" => (val * 1024.0) as u64,
                            "MB" => val as u64,
                            _ => 0,
                        }
                    }
                    parse_mem_mb(&b.memory).cmp(&parse_mem_mb(&a.memory))
                });
            }
        }
    }

    /// Load console logs for the currently selected workspace.
    fn load_selected_logs(&mut self) {
        if let Some(ws) = self.data.workspaces.get(self.selected) {
            self.data.logs =
                data::DashboardData::load_workspace_logs(&self.config, &ws.id, 500);
        } else {
            self.data.logs = Vec::new();
        }
    }
}

pub fn run(config: Config, refresh_secs: u64) -> anyhow::Result<()> {
    // Setup terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // Create app
    let refresh_interval = Duration::from_secs(refresh_secs);
    let initial_data = data::DashboardData::load(&config);
    let mut app = App {
        data: initial_data,
        selected: 0,
        log_scroll: 0,
        should_quit: false,
        config,
        refresh_interval,
        last_refresh: Instant::now(),
        tick_count: 0,
        status_message: None,
        show_help: false,
        sort_by: SortField::Name,
        selection_display: String::new(),
        team_mode: false,
        team_data: data::TeamDashboardData::default(),
        team_selected: 0,
    };

    // Initial sort, log load, and selection display
    app.sort_workspaces();
    app.load_selected_logs();
    app.update_selection_display();

    // Event loop
    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        app.clear_stale_messages();

        terminal.draw(|frame| {
            ui::draw(frame, app);
            if app.show_help {
                ui::render_help_overlay(frame);
            }
        })?;

        // Poll for events with 100ms timeout (smooth key response)
        if crossterm::event::poll(Duration::from_millis(100))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    handle_key(app, key);
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }

        // Periodic data refresh
        if app.last_refresh.elapsed() >= app.refresh_interval {
            if app.team_mode {
                app.team_data = data::TeamDashboardData::load(&app.config);
                if !app.team_data.teams.is_empty() {
                    app.team_selected = app.team_selected.min(app.team_data.teams.len() - 1);
                }
            } else {
                app.data = data::DashboardData::load(&app.config);
                app.sort_workspaces();
                if !app.data.workspaces.is_empty() {
                    app.selected = app.selected.min(app.data.workspaces.len() - 1);
                }
                app.load_selected_logs();
                app.update_selection_display();
            }
            app.last_refresh = Instant::now();
        }

        app.tick_count += 1;
    }
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    // If help overlay is shown, any key dismisses it
    if app.show_help {
        app.show_help = false;
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('?') => {
            app.show_help = true;
        }
        KeyCode::Char('t') => {
            app.team_mode = !app.team_mode;
            if app.team_mode {
                app.team_data = data::TeamDashboardData::load(&app.config);
                app.team_selected = 0;
                app.set_message("Team view");
            } else {
                app.set_message("Workspace view");
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.team_mode {
                if !app.team_data.teams.is_empty() {
                    app.team_selected = (app.team_selected + 1) % app.team_data.teams.len();
                }
            } else if !app.data.workspaces.is_empty() {
                app.selected = (app.selected + 1) % app.data.workspaces.len();
                app.log_scroll = 0;
                app.load_selected_logs();
                app.update_selection_display();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.team_mode {
                if !app.team_data.teams.is_empty() {
                    app.team_selected = app
                        .team_selected
                        .checked_sub(1)
                        .unwrap_or(app.team_data.teams.len() - 1);
                }
            } else if !app.data.workspaces.is_empty() {
                app.selected = app
                    .selected
                    .checked_sub(1)
                    .unwrap_or(app.data.workspaces.len() - 1);
                app.log_scroll = 0;
                app.load_selected_logs();
                app.update_selection_display();
            }
        }
        KeyCode::Char('G') => {
            app.log_scroll = usize::MAX;
        }
        KeyCode::Char('g') => {
            app.log_scroll = 0;
        }
        KeyCode::Char('r') => {
            if app.team_mode {
                app.team_data = data::TeamDashboardData::load(&app.config);
                if !app.team_data.teams.is_empty() {
                    app.team_selected = app.team_selected.min(app.team_data.teams.len() - 1);
                }
            } else {
                app.data = data::DashboardData::load(&app.config);
                app.sort_workspaces();
                if !app.data.workspaces.is_empty() {
                    app.selected = app.selected.min(app.data.workspaces.len() - 1);
                }
                app.load_selected_logs();
                app.update_selection_display();
            }
            app.last_refresh = Instant::now();
            app.set_message("Refreshed".to_string());
        }
        KeyCode::Char('s') => {
            if !app.team_mode {
                app.sort_by = app.sort_by.next();
                app.sort_workspaces();
                if !app.data.workspaces.is_empty() {
                    app.selected = app.selected.min(app.data.workspaces.len() - 1);
                }
                app.load_selected_logs();
                app.update_selection_display();
                app.set_message(format!("Sort: {}", app.sort_by.label()));
            }
        }
        KeyCode::PageDown | KeyCode::Char('f') => {
            app.log_scroll = app.log_scroll.saturating_add(20);
        }
        KeyCode::PageUp | KeyCode::Char('b') => {
            app.log_scroll = app.log_scroll.saturating_sub(20);
        }
        _ => {}
    }
}
