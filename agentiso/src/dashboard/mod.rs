pub mod data;
pub mod ui;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;

pub struct App {
    pub data: data::DashboardData,
    pub selected: usize,
    pub log_scroll: usize,
    pub should_quit: bool,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub refresh_interval: Duration,
    pub last_refresh: Instant,
    pub tick_count: u64,
    /// Child process if we spawned the server from the dashboard.
    pub server_child: Option<Child>,
    /// Transient status message shown in the header (auto-clears after 3s).
    pub status_message: Option<(String, Instant)>,
}

impl App {
    /// Returns true if the server child we spawned is still alive.
    fn is_our_server_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.server_child {
            match child.try_wait() {
                Ok(None) => true,  // still running
                Ok(Some(_)) => {
                    self.server_child = None;
                    false
                }
                Err(_) => {
                    self.server_child = None;
                    false
                }
            }
        } else {
            false
        }
    }

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
}

pub fn run(config: Config, config_path: Option<PathBuf>, refresh_secs: u64) -> anyhow::Result<()> {
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
        config_path,
        refresh_interval,
        last_refresh: Instant::now(),
        tick_count: 0,
        server_child: None,
        status_message: None,
    };

    // Event loop
    let result = run_loop(&mut terminal, &mut app);

    // Stop server child if we spawned it
    if let Some(ref mut child) = app.server_child {
        // Send SIGTERM for graceful shutdown
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        // Give it a moment, then force kill
        std::thread::sleep(Duration::from_millis(500));
        let _ = child.kill();
        let _ = child.wait();
    }

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
            // Reap zombie child if server exited
            app.is_our_server_alive();
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
                app.log_scroll = 0;
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
            app.log_scroll = usize::MAX;
        }
        KeyCode::Char('g') => {
            app.log_scroll = 0;
        }
        KeyCode::Char('r') => {
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
        KeyCode::Char('S') | KeyCode::Char('s') => {
            start_server(app);
        }
        KeyCode::Char('X') | KeyCode::Char('x') => {
            stop_server(app);
        }
        _ => {}
    }
}

/// Start the MCP server as a child process.
fn start_server(app: &mut App) {
    // Check if already running
    if app.data.system.server_running {
        if app.is_our_server_alive() {
            app.set_message("Server already running (started by this dashboard)");
        } else {
            app.set_message("Server already running (external process)");
        }
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            app.set_message(format!("Failed to find executable: {}", e));
            return;
        }
    };

    let mut cmd = Command::new(exe);
    cmd.arg("serve");
    if let Some(ref config_path) = app.config_path {
        cmd.arg("--config").arg(config_path);
    }

    // Keep stdin pipe open so the MCP server blocks on read (no client connected).
    // Redirect stdout to null (MCP protocol writes go nowhere without a client).
    // Redirect stderr to a log file for debugging.
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    let stderr_path = app
        .config
        .server
        .state_file
        .parent()
        .map(|p| p.join("dashboard-server.log"))
        .unwrap_or_else(|| std::path::PathBuf::from("/var/lib/agentiso/dashboard-server.log"));
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_path);
    match stderr_file {
        Ok(f) => cmd.stderr(f),
        Err(_) => cmd.stderr(Stdio::null()),
    };

    match cmd.spawn() {
        Ok(child) => {
            app.set_message(format!("Server started (PID {})", child.id()));
            app.server_child = Some(child);
            // Next periodic refresh will pick up the new server status
        }
        Err(e) => {
            app.set_message(format!("Failed to start server: {}", e));
        }
    }
}

/// Stop the MCP server.
fn stop_server(app: &mut App) {
    if !app.data.system.server_running {
        app.set_message("Server is not running");
        return;
    }

    if let Some(ref mut child) = app.server_child {
        let pid = child.id();
        // Graceful SIGTERM
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        // Give it a moment to shut down
        std::thread::sleep(Duration::from_millis(300));
        match child.try_wait() {
            Ok(Some(_)) => {
                app.set_message(format!("Server stopped (PID {})", pid));
            }
            _ => {
                // Force kill if it didn't exit
                let _ = child.kill();
                let _ = child.wait();
                app.set_message(format!("Server killed (PID {})", pid));
            }
        }
        app.server_child = None;
    } else {
        app.set_message("Cannot stop external server (use kill or systemctl)");
    }
}
