//! Terminal UI rendering — dashboard grid, agent sessions, and status bar.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::rice::RiceStatus;

use super::App;
use super::RiceSetupStep;
use super::ViewMode;
use super::daemon::AgentWindowStatus;

/// Animated spinner frames for the thinking indicator.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Gradient accent colors for grid cards.
const ACCENT_COLORS: &[Color] = &[
    Color::Rgb(0, 255, 136),   // green
    Color::Rgb(0, 210, 255),   // cyan
    Color::Rgb(138, 43, 226),  // purple
    Color::Rgb(255, 165, 0),   // orange
    Color::Rgb(255, 105, 180), // pink
    Color::Rgb(64, 224, 208),  // turquoise
    Color::Rgb(255, 215, 0),   // gold
    Color::Rgb(100, 149, 237), // cornflower
    Color::Rgb(220, 20, 60),   // crimson
];

/// Fun idle messages for empty grid cells.
const EMPTY_HINTS: &[&str] = &[
    "awaiting orders…",
    "ready for action",
    "idle & caffeinated",
    "standing by ☕",
    "spawn me!",
    "nothing to see here",
    "free real estate",
    "room for one more",
    "agent vacancy",
];

impl App {
    /// Get the current spinner frame based on tick count.
    fn spinner_frame(&self) -> &'static str {
        SPINNER[(self.tick_count / 3) as usize % SPINNER.len()]
    }

    /// Get a gradient accent color for a given index.
    fn accent_color(&self, idx: usize) -> Color {
        ACCENT_COLORS[idx % ACCENT_COLORS.len()]
    }
    /// Render the full TUI frame, dispatching to the active view mode.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        match self.view_mode.clone() {
            ViewMode::Dashboard => self.draw_dashboard(frame),
            ViewMode::AgentSession(window_id) => self.draw_agent_session(frame, window_id),
        }
    }

    // ── Dashboard view ───────────────────────────────────────────────

    /// Home screen: status bar, activity log (compact) + 3×3 agent grid, input prompt, footer.
    fn draw_dashboard(&mut self, frame: &mut Frame<'_>) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // status bar
                Constraint::Min(1),    // main area
                Constraint::Length(3), // input prompt
                Constraint::Length(1), // footer bar
            ])
            .split(frame.area());

        // ── Status bar ───────────────────────────────────────────────
        self.draw_status_bar(frame, rows[0]);

        // ── Main area: activity log (left) + agent grid (right) ──────
        {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(rows[1]);
            self.draw_activity_log(frame, cols[0]);
            self.draw_agent_grid(frame, cols[1]);
        }

        // ── Input prompt ─────────────────────────────────────────────
        let (prompt_label, prompt_style) = if let Some(ref step) = self.rice_setup_step {
            let label = match step {
                RiceSetupStep::StateUrl => " 🔧 Rice State URL ",
                RiceSetupStep::StateToken => " 🔑 Rice State Token ",
                RiceSetupStep::StorageUrl => " 📦 Rice Storage URL ",
                RiceSetupStep::StorageToken => " 🔑 Rice Storage Token ",
            };
            (label, Style::default().fg(Color::Rgb(0, 210, 255)))
        } else if self.chat_busy {
            let spinner = self.spinner_frame();
            // Can't interpolate a dynamic spinner into a static str, so we use a fixed label.
            let _ = spinner;
            (" ⟳ Thinking… ", Style::default().fg(Color::Yellow))
        } else {
            (" ❯ memini ", Style::default().fg(Color::Rgb(0, 255, 136)))
        };

        let input_panel = Paragraph::new(self.input.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(prompt_label)
                .border_style(prompt_style),
        );
        frame.render_widget(input_panel, rows[2]);

        let input_width = rows[2].width.saturating_sub(2) as usize;
        let cursor = self.cursor.min(input_width);
        frame.set_cursor_position(Position::new(rows[2].x + 1 + cursor as u16, rows[2].y + 1));

        // ── Footer bar ───────────────────────────────────────────────
        self.draw_footer(frame, rows[3]);
    }

    // ── Agent grid (3×3) ─────────────────────────────────────────────

    /// Render the 3×3 grid of agent cards in the dashboard.
    fn draw_agent_grid(&self, frame: &mut Frame<'_>, area: Rect) {
        let grid_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(area);

        for row in 0..3u16 {
            let grid_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                ])
                .split(grid_rows[row as usize]);

            for col in 0..3u16 {
                let cell_idx = (row * 3 + col) as usize;
                let cell_area = grid_cols[col as usize];
                let is_selected = cell_idx == self.grid_selected;

                if let Some(window) = self.agent_windows.get(cell_idx) {
                    self.draw_agent_card(frame, cell_area, window, is_selected);
                } else {
                    self.draw_empty_card(frame, cell_area, cell_idx, is_selected);
                }
            }
        }
    }

    /// Render one agent card in the grid.
    fn draw_agent_card(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        window: &super::daemon::AgentWindow,
        is_selected: bool,
    ) {
        let accent = self.accent_color(window.id);

        let (status_icon, status_color) = match window.status {
            AgentWindowStatus::Thinking => (self.spinner_frame(), Color::Yellow),
            AgentWindowStatus::Done => ("✓", Color::Rgb(0, 255, 136)),
            AgentWindowStatus::WaitingForInput => ("◈", Color::Rgb(255, 105, 180)),
        };

        let title = format!(" #{} {} {} ", window.id, status_icon, window.label);

        let border_style = if is_selected {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(status_color)
        };

        // Build card content: prompt preview + last few output lines.
        let inner_height = area.height.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();

        // Prompt preview (first line).
        let preview: String = window.prompt.chars().take(40).collect();
        lines.push(Line::from(Span::styled(
            format!(" {preview}…"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));

        // Status line.
        let status_text = match window.status {
            AgentWindowStatus::Thinking => format!("{} working…", self.spinner_frame()),
            AgentWindowStatus::Done => "✓ done".to_string(),
            AgentWindowStatus::WaitingForInput => "◈ needs input".to_string(),
        };
        lines.push(Line::from(Span::styled(
            format!(" {status_text}"),
            Style::default().fg(status_color),
        )));

        // Tail of output.
        let remaining = inner_height.saturating_sub(lines.len());
        if remaining > 0 {
            let tail: Vec<&String> = window
                .output_lines
                .iter()
                .rev()
                .take(remaining)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            for s in tail {
                let truncated: String = s.chars().take(50).collect();
                lines.push(Line::from(Span::styled(
                    format!(" {truncated}"),
                    Style::default().fg(Color::White),
                )));
            }
        }

        let card = Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(title, Style::default().fg(status_color))),
        );
        frame.render_widget(card, area);
    }

    /// Render an empty grid cell placeholder.
    fn draw_empty_card(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        cell_idx: usize,
        is_selected: bool,
    ) {
        let accent = self.accent_color(cell_idx);
        let hint_text = EMPTY_HINTS[cell_idx % EMPTY_HINTS.len()];

        let border_style = if is_selected {
            Style::default().fg(accent)
        } else {
            Style::default().fg(Color::Rgb(50, 50, 50))
        };

        let hint = if is_selected {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  /spawn <prompt>",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("  {hint_text}"),
                    Style::default().fg(Color::Rgb(80, 80, 80)),
                )),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  {hint_text}"),
                    Style::default().fg(Color::Rgb(50, 50, 50)),
                )),
            ]
        };

        let card = Paragraph::new(Text::from(hint)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style),
        );
        frame.render_widget(card, area);
    }

    // ── Agent session view ───────────────────────────────────────────

    /// Full-screen view for a single agent session.
    fn draw_agent_session(&mut self, frame: &mut Frame<'_>, window_id: usize) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // status bar
                Constraint::Min(1),    // agent output
                Constraint::Length(3), // input prompt
                Constraint::Length(1), // footer
            ])
            .split(frame.area());

        // ── Status bar ───────────────────────────────────────────────
        self.draw_status_bar(frame, rows[0]);

        // ── Agent output (full width) ────────────────────────────────
        if let Some(window) = self.agent_windows.iter().find(|w| w.id == window_id) {
            let accent = self.accent_color(window.id);

            let (status_label, status_color) = match window.status {
                AgentWindowStatus::Thinking => {
                    (format!("{} thinking…", self.spinner_frame()), Color::Yellow)
                }
                AgentWindowStatus::Done => ("✓ done".to_string(), Color::Rgb(0, 255, 136)),
                AgentWindowStatus::WaitingForInput => {
                    ("◈ needs input".to_string(), Color::Rgb(255, 105, 180))
                }
            };

            let title = format!(
                " #{} {} — {} [Esc: back] ",
                window.id, window.label, status_label
            );

            let inner_height = rows[1].height.saturating_sub(2) as usize;
            let display_lines: Vec<Line> = window
                .output_lines
                .iter()
                .rev()
                .take(inner_height.max(1))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|s| {
                    let color = if s.starts_with(">>") {
                        Color::Rgb(255, 105, 180)
                    } else if s.starts_with("--") {
                        Color::Rgb(80, 80, 80)
                    } else if s.starts_with("Thinking")
                        || s.starts_with("Recalling")
                        || s.starts_with("Saving")
                        || s.starts_with("Found")
                    {
                        Color::Yellow
                    } else {
                        Color::White
                    };
                    Line::from(Span::styled(format!(" {s}"), Style::default().fg(color)))
                })
                .collect();

            let panel = Paragraph::new(Text::from(display_lines))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(accent))
                        .title(Span::styled(
                            title,
                            Style::default()
                                .fg(status_color)
                                .add_modifier(Modifier::BOLD),
                        )),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(panel, rows[1]);
        } else {
            // Window no longer exists — show message.
            let msg = Paragraph::new("Agent window not found. Press Esc to return.")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(msg, rows[1]);
        }

        // ── Input prompt ─────────────────────────────────────────────
        let prompt_label = if self
            .agent_windows
            .iter()
            .any(|w| w.id == window_id && w.status == AgentWindowStatus::WaitingForInput)
        {
            format!(" ◈ Reply to Agent #{window_id} ")
        } else {
            format!(" ❯ Agent #{window_id} ")
        };

        let input_panel = Paragraph::new(self.input.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(prompt_label)
                .border_style(Style::default().fg(self.accent_color(window_id))),
        );
        frame.render_widget(input_panel, rows[2]);

        let input_width = rows[2].width.saturating_sub(2) as usize;
        let cursor = self.cursor.min(input_width);
        frame.set_cursor_position(Position::new(rows[2].x + 1 + cursor as u16, rows[2].y + 1));

        // ── Footer ───────────────────────────────────────────────────
        self.draw_footer(frame, rows[3]);
    }

    // ── Status bar ───────────────────────────────────────────────────

    fn draw_status_bar(&self, frame: &mut Frame<'_>, area: Rect) {
        let thread_turns = self.conversation_thread.len() / 2;
        let daemon_count = self.daemon_handles.len();
        let window_count = self.agent_windows.len();
        let thinking = self
            .agent_windows
            .iter()
            .filter(|w| w.status == AgentWindowStatus::Thinking)
            .count();
        let waiting = self
            .agent_windows
            .iter()
            .filter(|w| w.status == AgentWindowStatus::WaitingForInput)
            .count();

        let mut spans = vec![
            Span::styled(
                " ◆ ",
                Style::default()
                    .fg(Color::Rgb(0, 255, 136))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                self.active_agent.name.clone(),
                Style::default()
                    .fg(Color::Rgb(138, 43, 226))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(ws) = &self.rice.shared_run_id {
            spans.push(Span::styled(
                "  ⊞ ",
                Style::default().fg(Color::Rgb(80, 80, 80)),
            ));
            spans.push(Span::styled(
                ws.clone(),
                Style::default().fg(Color::Rgb(0, 255, 136)),
            ));
        }
        spans.extend([
            Span::styled("  ⚡ ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                self.mcp_status_label(),
                Style::default().fg(self.mcp_status_color()),
            ),
            Span::styled("  ⬡ ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                self.rice.status_label(),
                Style::default().fg(self.rice_status_color()),
            ),
            Span::styled(
                format!("  ↩ {thread_turns}"),
                Style::default().fg(Color::Rgb(100, 100, 100)),
            ),
        ]);
        if daemon_count > 0 {
            spans.push(Span::styled(
                format!("  ⚙ {daemon_count}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        if window_count > 0 {
            let mut agent_label = format!("  ▣ {window_count}");
            if thinking > 0 {
                agent_label.push_str(&format!(" ({thinking}{}", self.spinner_frame()));
                agent_label.push(')');
            }
            if waiting > 0 {
                agent_label.push_str(&format!(" ({waiting}◈)"));
            }
            spans.push(Span::styled(
                agent_label,
                Style::default().fg(Color::Rgb(0, 210, 255)),
            ));
        }
        if self.chat_busy {
            spans.push(Span::styled(
                format!("  {} Thinking…", self.spinner_frame()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    // ── Activity log ─────────────────────────────────────────────────

    fn draw_activity_log(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let inner_width = area.width.saturating_sub(2);
        let inner_height = area.height.saturating_sub(2) as usize;

        let log_lines: Vec<Line> = self.logs.iter().flat_map(|l| l.render()).collect();
        let log_paragraph = Paragraph::new(Text::from(log_lines)).wrap(Wrap { trim: false });

        let total_visual = log_paragraph.line_count(inner_width);
        let max_scroll = total_visual.saturating_sub(inner_height);

        if (self.scroll_offset as usize) > max_scroll {
            self.scroll_offset = max_scroll as u16;
        }
        let top_row = max_scroll.saturating_sub(self.scroll_offset as usize) as u16;

        let scroll_indicator = if self.scroll_offset > 0 {
            format!(" ◆ memini [↑{}] ", self.scroll_offset)
        } else {
            " ◆ memini ".to_string()
        };

        let panel = log_paragraph
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                    .title(Span::styled(
                        scroll_indicator,
                        Style::default()
                            .fg(Color::Rgb(0, 255, 136))
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .scroll((top_row, 0));
        frame.render_widget(panel, area);
    }

    // ── Status-bar helpers ───────────────────────────────────────────

    fn mcp_status_label(&self) -> String {
        let connected = self.mcp_connections.len();
        if connected > 0 {
            if connected == 1 {
                let name = self
                    .mcp_connections
                    .values()
                    .next()
                    .map(|conn| conn.server.display_name())
                    .unwrap_or_else(|| "1 connected".to_string());
                return format!("{name} (connected)");
            }

            if let Some(active) = self
                .active_mcp
                .as_ref()
                .and_then(|server| self.mcp_connections.get(&server.id))
            {
                return format!("{} (+{})", active.server.display_name(), connected - 1);
            }

            return format!("{connected} connected");
        }

        if let Some(server) = &self.active_mcp {
            format!("{} (saved)", server.display_name())
        } else {
            "none".to_string()
        }
    }

    fn mcp_status_color(&self) -> Color {
        if !self.mcp_connections.is_empty() {
            Color::Rgb(0, 255, 136)
        } else if self.active_mcp.is_some() {
            Color::Yellow
        } else {
            Color::Rgb(80, 80, 80)
        }
    }

    #[allow(dead_code)]
    fn openai_status_label(&self) -> String {
        match &self.openai_key_hint {
            Some(hint) => hint.clone(),
            None => "unset".to_string(),
        }
    }

    #[allow(dead_code)]
    fn openai_status_color(&self) -> Color {
        if self.openai_key_hint.is_some() {
            Color::Rgb(0, 255, 136)
        } else {
            Color::Rgb(80, 80, 80)
        }
    }

    fn rice_status_color(&self) -> Color {
        match self.rice.status {
            RiceStatus::Connected => Color::Rgb(0, 255, 136),
            RiceStatus::Disabled(_) => Color::Rgb(80, 80, 80),
        }
    }

    // ── Footer bar ───────────────────────────────────────────────────

    fn draw_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let keys = vec![
            Span::styled(
                " /help",
                Style::default()
                    .fg(Color::Rgb(0, 210, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" commands  ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                "Tab",
                Style::default()
                    .fg(Color::Rgb(0, 210, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cycle  ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Rgb(0, 210, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" open  ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Rgb(0, 210, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" back  ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                "Ctrl+C",
                Style::default()
                    .fg(Color::Rgb(0, 210, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" quit  ", Style::default().fg(Color::Rgb(80, 80, 80))),
            Span::styled(
                "/rice setup",
                Style::default()
                    .fg(Color::Rgb(0, 210, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" configure", Style::default().fg(Color::Rgb(80, 80, 80))),
        ];
        frame.render_widget(Paragraph::new(Line::from(keys)), area);
    }
}
