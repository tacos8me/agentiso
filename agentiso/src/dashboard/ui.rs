use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Row, Table, Wrap},
    Frame,
};

use super::data::WorkspaceEntry;

// ── Color Theme ─────────────────────────────────────────────────────────────

const ACCENT: Color = Color::Cyan;
const TEXT: Color = Color::White;
const TEXT_DIM: Color = Color::Rgb(128, 128, 128);
const TEXT_MUTED: Color = Color::DarkGray;

const STATE_RUNNING: Color = Color::Rgb(0, 215, 135); // soft green
const STATE_STOPPED: Color = Color::Rgb(108, 108, 108); // gray
const STATE_SUSPENDED: Color = Color::Yellow;
const STATE_DEAD: Color = Color::Red;

const SELECTED_BG: Color = Color::Rgb(30, 40, 55); // subtle dark blue
const HEADER_FG: Color = Color::Cyan;
const BORDER: Color = Color::Rgb(60, 60, 60);
const BORDER_ACTIVE: Color = Color::Cyan;

// ── Public Draw Entry Point ─────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, app: &mut super::App) {
    let area = frame.area();

    // Guard: terminal too small
    if area.width < 40 || area.height < 15 {
        let msg = Paragraph::new("Terminal too small. Resize to at least 40x15.")
            .style(Style::default().fg(TEXT_MUTED))
            .alignment(Alignment::Center);
        frame.render_widget(msg, area);
        return;
    }

    // Main vertical layout: header, middle (table + detail), footer
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),       // header
            Constraint::Percentage(100), // middle (fills remaining)
            Constraint::Length(1),       // footer
        ])
        .split(area);

    draw_header(frame, app, outer[0]);
    draw_middle(frame, app, outer[1]);
    draw_footer(frame, outer[2]);
}

// ── Header ──────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, app: &super::App, area: Rect) {
    let sys = &app.data.system;

    // Title spans
    let left_title = Line::from(vec![Span::styled(
        " agentiso ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )]);

    let right_title = Line::from(vec![Span::styled(
        format!(
            " {} workspaces \u{2500}\u{2500} {} running ",
            sys.total, sys.running
        ),
        Style::default().fg(TEXT_DIM),
    )]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(left_title)
        .title_alignment(Alignment::Left)
        .title(right_title)
        .title_alignment(Alignment::Right)
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // System status indicators
    let mut indicators: Vec<Span> = Vec::new();

    // ZFS indicator
    let zfs_ok = sys.zfs_free != "N/A";
    indicators.push(Span::styled(
        "\u{25cf} ",
        Style::default().fg(if zfs_ok { STATE_RUNNING } else { STATE_DEAD }),
    ));
    indicators.push(Span::styled(
        format!("ZFS {} free", sys.zfs_free),
        Style::default().fg(TEXT),
    ));
    indicators.push(Span::styled("    ", Style::default()));

    // Bridge indicator
    indicators.push(Span::styled(
        "\u{25cf} ",
        Style::default().fg(if sys.bridge_up {
            STATE_RUNNING
        } else {
            STATE_DEAD
        }),
    ));
    indicators.push(Span::styled(
        format!(
            "{} {}",
            sys.bridge_name,
            if sys.bridge_up { "up" } else { "down" }
        ),
        Style::default().fg(TEXT),
    ));
    indicators.push(Span::styled("    ", Style::default()));

    // KVM indicator
    let kvm_ok = std::path::Path::new("/dev/kvm").exists();
    indicators.push(Span::styled(
        "\u{25cf} ",
        Style::default().fg(if kvm_ok { STATE_RUNNING } else { STATE_DEAD }),
    ));
    indicators.push(Span::styled(
        "KVM /dev/kvm",
        Style::default().fg(TEXT),
    ));
    indicators.push(Span::styled("    ", Style::default()));

    // Server indicator
    indicators.push(Span::styled(
        "\u{25cf} ",
        Style::default().fg(if sys.server_running {
            STATE_RUNNING
        } else {
            STATE_STOPPED
        }),
    ));
    indicators.push(Span::styled(
        if sys.server_running { "MCP server" } else { "MCP stopped" },
        Style::default().fg(if sys.server_running { TEXT } else { TEXT_DIM }),
    ));

    let status_line = Line::from(indicators);

    // Build content lines
    let mut lines = vec![status_line];

    // Error line (if any)
    if let Some(ref err) = app.data.error {
        lines.push(Line::from(Span::styled(
            format!("  Error: {}", err),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )));
    }

    // Transient status message
    if let Some((ref msg, _)) = app.status_message {
        lines.push(Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(ACCENT),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ── Middle Area (Table + Detail) ────────────────────────────────────────────

fn draw_middle(frame: &mut Frame, app: &mut super::App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40), // workspace table
            Constraint::Percentage(60), // detail + log
        ])
        .split(area);

    draw_workspace_table(frame, app, chunks[0]);
    draw_detail_area(frame, app, chunks[1]);
}

// ── Workspace Table ─────────────────────────────────────────────────────────

fn draw_workspace_table(frame: &mut Frame, app: &super::App, area: Rect) {
    if app.data.workspaces.is_empty() {
        let msg = Paragraph::new("No workspaces. Start the MCP server and create a workspace.")
            .style(Style::default().fg(TEXT_MUTED))
            .alignment(Alignment::Center);
        // Center vertically by rendering in the middle of the area
        let centered = centered_rect_vertical(area, 1);
        frame.render_widget(msg, centered);
        return;
    }

    // Column widths
    let widths = [
        Constraint::Min(16),      // WORKSPACE (flexible)
        Constraint::Length(14),    // STATE
        Constraint::Length(16),    // IP
        Constraint::Length(9),     // MEM
        Constraint::Length(9),     // DISK
        Constraint::Length(10),    // AGE
    ];

    // Header row
    let header_cells = [
        "  WORKSPACE",
        "STATE",
        "IP",
        "     MEM",
        "    DISK",
        "       AGE",
    ];
    let header = Row::new(
        header_cells
            .iter()
            .map(|h| {
                Span::styled(
                    *h,
                    Style::default()
                        .fg(HEADER_FG)
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    )
    .height(1)
    .bottom_margin(0);

    // Data rows
    let rows: Vec<Row> = app
        .data
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, ws)| build_workspace_row(ws, i == app.selected))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(BORDER))
                .border_type(BorderType::Plain)
                .padding(Padding::horizontal(1)),
        );

    frame.render_widget(table, area);
}

fn build_workspace_row(ws: &WorkspaceEntry, selected: bool) -> Row<'static> {
    let base_style = if selected {
        Style::default().bg(SELECTED_BG).fg(TEXT)
    } else {
        Style::default().fg(TEXT)
    };

    // Selection marker + workspace name
    let marker = if selected {
        Span::styled("\u{25b8} ", Style::default().fg(ACCENT).bg(if selected { SELECTED_BG } else { Color::Reset }))
    } else {
        Span::styled("  ", base_style)
    };
    let name_span = Span::styled(ws.name.clone(), base_style);
    let name_cell = Line::from(vec![marker, name_span]);

    // State cell with colored indicator
    let state_cell = build_state_cell(ws, selected);

    // IP cell
    let ip_style = if selected {
        base_style
    } else if ws.ip == "\u{2014}" || ws.state == "Stopped" {
        Style::default().fg(TEXT_DIM)
    } else {
        Style::default().fg(TEXT)
    };
    let ip_cell = Span::styled(ws.ip.clone(), ip_style);

    // MEM cell (right-aligned by padding)
    let mem_cell = Span::styled(
        format!("{:>8}", ws.memory),
        base_style,
    );

    // DISK cell (right-aligned by padding)
    let disk_cell = Span::styled(
        format!("{:>8}", ws.disk),
        base_style,
    );

    // AGE cell (right-aligned by padding)
    let age_style = if selected {
        base_style
    } else {
        Style::default().fg(TEXT_DIM)
    };
    let age_cell = Span::styled(
        format!("{:>10}", ws.age),
        age_style,
    );

    Row::new(vec![
        Line::from(name_cell),
        Line::from(state_cell),
        Line::from(vec![ip_cell]),
        Line::from(vec![mem_cell]),
        Line::from(vec![disk_cell]),
        Line::from(vec![age_cell]),
    ])
    .style(base_style)
    .height(1)
}

fn build_state_cell(ws: &WorkspaceEntry, selected: bool) -> Line<'static> {
    let bg = if selected { SELECTED_BG } else { Color::Reset };

    match ws.state.as_str() {
        "Running" if ws.state_alive => Line::from(vec![
            Span::styled(
                "\u{25cf} ",
                Style::default().fg(STATE_RUNNING).bg(bg),
            ),
            Span::styled(
                "Running",
                Style::default().fg(TEXT).bg(bg),
            ),
        ]),
        "Running" => Line::from(vec![
            Span::styled(
                "\u{25cf} ",
                Style::default().fg(STATE_DEAD).bg(bg),
            ),
            Span::styled(
                "Dead",
                Style::default().fg(STATE_DEAD).bg(bg),
            ),
        ]),
        "Stopped" => Line::from(vec![
            Span::styled(
                "\u{25cb} ",
                Style::default().fg(STATE_STOPPED).bg(bg),
            ),
            Span::styled(
                "Stopped",
                Style::default().fg(TEXT_DIM).bg(bg),
            ),
        ]),
        "Suspended" => Line::from(vec![
            Span::styled(
                "\u{25d0} ",
                Style::default().fg(STATE_SUSPENDED).bg(bg),
            ),
            Span::styled(
                "Suspended",
                Style::default().fg(TEXT).bg(bg),
            ),
        ]),
        other => Line::from(vec![Span::styled(
            other.to_string(),
            Style::default().fg(TEXT_DIM).bg(bg),
        )]),
    }
}

// ── Detail Area (Detail Pane + Log Viewer) ──────────────────────────────────

fn draw_detail_area(frame: &mut Frame, app: &mut super::App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35), // detail pane
            Constraint::Percentage(65), // log viewer
        ])
        .split(area);

    draw_detail_pane(frame, app, chunks[0]);
    draw_log_viewer(frame, app, chunks[1]);
}

// ── Detail Pane ─────────────────────────────────────────────────────────────

fn draw_detail_pane(frame: &mut Frame, app: &super::App, area: Rect) {
    let selected_ws = if app.data.workspaces.is_empty() {
        None
    } else {
        app.data.workspaces.get(app.selected)
    };

    let title = match selected_ws {
        Some(ws) => format!(" {} ", ws.name),
        None => " No workspace selected ".to_string(),
    };

    let border_color = if selected_ws.is_some() {
        BORDER_ACTIVE
    } else {
        BORDER
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default()
                .fg(if selected_ws.is_some() { ACCENT } else { TEXT_MUTED })
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::new(2, 1, 1, 0));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    match selected_ws {
        Some(ws) => {
            let lines = build_detail_lines(ws);
            let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
        }
        None => {
            let msg = Paragraph::new("(no workspace)")
                .style(Style::default().fg(TEXT_MUTED))
                .alignment(Alignment::Center);
            frame.render_widget(msg, inner);
        }
    }
}

fn build_detail_lines(ws: &WorkspaceEntry) -> Vec<Line<'static>> {
    let label_style = Style::default().fg(TEXT_DIM);
    let value_style = Style::default().fg(TEXT);

    let mut lines = vec![
        detail_kv("ID", &ws.id, label_style, value_style),
        detail_kv("Image", &ws.base_image, label_style, value_style),
        detail_kv("CID", &ws.vsock_cid.to_string(), label_style, value_style),
        detail_kv("TAP", &ws.tap_device, label_style, value_style),
        detail_kv("vCPUs", &ws.vcpus.to_string(), label_style, value_style),
        Line::from(""),
    ];

    // Snapshots section
    lines.push(Line::from(Span::styled(
        "Snapshots",
        Style::default()
            .fg(TEXT)
            .add_modifier(Modifier::BOLD),
    )));

    if ws.snapshot_names.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none)",
            Style::default().fg(TEXT_MUTED),
        )));
    } else {
        let total = ws.snapshot_names.len();
        for (i, name) in ws.snapshot_names.iter().enumerate() {
            let is_last = i == total - 1;
            let prefix = if is_last { " \u{2514}\u{2500} " } else { " \u{251c}\u{2500} " };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(TEXT_DIM)),
                Span::styled(name.clone(), value_style),
            ]));
        }
    }

    lines
}

fn detail_kv(label: &str, value: &str, label_style: Style, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<8}", label), label_style),
        Span::styled(value.to_string(), value_style),
    ])
}

// ── Log Viewer ──────────────────────────────────────────────────────────────

fn draw_log_viewer(frame: &mut Frame, app: &mut super::App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(Span::styled(
            " console ",
            Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.data.logs.is_empty() {
        let msg = Paragraph::new("(no logs)")
            .style(Style::default().fg(TEXT_MUTED))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    let visible_height = inner.height as usize;
    let total_lines = app.data.logs.len();

    // Clamp log_scroll
    let max_scroll = total_lines.saturating_sub(visible_height);
    app.log_scroll = app.log_scroll.min(max_scroll);

    let visible_logs: Vec<Line> = app
        .data
        .logs
        .iter()
        .skip(app.log_scroll)
        .take(visible_height)
        .map(|line| Line::from(Span::styled(line.clone(), Style::default().fg(TEXT_DIM))))
        .collect();

    let paragraph = Paragraph::new(visible_logs);
    frame.render_widget(paragraph, inner);
}

// ── Footer ──────────────────────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, area: Rect) {
    let help_text = " q quit \u{00b7} j/k navigate \u{00b7} r refresh \u{00b7} g/G top/bottom \u{00b7} pgup/pgdn scroll ";

    let paragraph = Paragraph::new(Line::from(Span::styled(
        help_text,
        Style::default().fg(TEXT_MUTED),
    )))
    .alignment(Alignment::Center);

    frame.render_widget(paragraph, area);
}

// ── Utility ─────────────────────────────────────────────────────────────────

/// Return a vertically-centered sub-rect with the given height.
fn centered_rect_vertical(area: Rect, height: u16) -> Rect {
    let top = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x: area.x,
        y: top,
        width: area.width,
        height: height.min(area.height),
    }
}
