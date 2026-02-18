pub mod data;
pub mod ui;

use std::time::{Duration, Instant};

use crate::config::Config;

pub struct App {
    pub data: data::DashboardData,
    pub selected: usize,
    pub log_scroll: usize,
    pub should_quit: bool,
    pub config: Config,
    pub refresh_interval: Duration,
    pub last_refresh: Instant,
    pub tick_count: u64,
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
    };

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
        terminal.draw(|frame| ui::draw(frame, app))?;

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
            app.data = data::DashboardData::load(&app.config);
            // Clamp selection
            if !app.data.workspaces.is_empty() {
                app.selected = app.selected.min(app.data.workspaces.len() - 1);
            }
            app.last_refresh = Instant::now();
        }

        app.tick_count += 1;
    }
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.data.workspaces.is_empty() {
                app.selected = (app.selected + 1) % app.data.workspaces.len();
                app.log_scroll = 0; // reset scroll on selection change
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !app.data.workspaces.is_empty() {
                app.selected = app
                    .selected
                    .checked_sub(1)
                    .unwrap_or(app.data.workspaces.len() - 1);
                app.log_scroll = 0;
            }
        }
        KeyCode::Char('G') => {
            // Jump to bottom of log
            app.log_scroll = usize::MAX;
        }
        KeyCode::Char('g') => {
            // Jump to top of log
            app.log_scroll = 0;
        }
        KeyCode::Char('r') => {
            // Force refresh
            app.data = data::DashboardData::load(&app.config);
            if !app.data.workspaces.is_empty() {
                app.selected = app.selected.min(app.data.workspaces.len() - 1);
            }
            app.last_refresh = Instant::now();
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
