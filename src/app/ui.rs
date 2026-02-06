//! Terminal UI rendering — layout, status bar, agent windows, and activity panel.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::rice::RiceStatus;

use super::App;
use super::daemon::AgentWindowStatus;

impl App {
    /// Render the full TUI frame: header bar, activity log, agent windows, and input prompt.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(frame.area());

        // ── Status bar ───────────────────────────────────────────────
        self.draw_status_bar(frame, rows[0]);

        // ── Main content area (log + optional agent panel) ───────────
        if self.show_side_panel {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(rows[1]);

            self.draw_activity_log(frame, cols[0]);
            self.draw_agent_panel(frame, cols[1]);
        } else {
            self.draw_activity_log(frame, rows[1]);
        }

        // ── Input prompt ─────────────────────────────────────────────
        let prompt_label = if let Some(fid) = self.focused_window {
            if self
                .agent_windows
                .iter()
                .any(|w| w.id == fid && w.status == AgentWindowStatus::WaitingForInput)
            {
                format!(" Reply to Agent #{fid} ")
            } else {
                format!(" Command (focused: #{fid}) ")
            }
        } else {
            " Command ".to_string()
        };

        let input_panel = Paragraph::new(self.input.as_str())
            .block(Block::default().borders(Borders::ALL).title(prompt_label));
        frame.render_widget(input_panel, rows[2]);

        let input_width = rows[2].width.saturating_sub(2) as usize;
        let cursor = self.cursor.min(input_width);
        frame.set_cursor_position(Position::new(rows[2].x + 1 + cursor as u16, rows[2].y + 1));
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
            Span::styled("Persona: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.active_agent.name.clone(),
                Style::default().fg(Color::Magenta),
            ),
        ];
        if let Some(ws) = &self.rice.shared_run_id {
            spans.push(Span::styled(
                "  Workspace: ",
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(ws.clone(), Style::default().fg(Color::Green)));
        }
        spans.extend([
            Span::styled("  Tools: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.mcp_status_label(),
                Style::default().fg(self.mcp_status_color()),
            ),
            Span::styled("  Rice: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.rice.status_label(),
                Style::default().fg(self.rice_status_color()),
            ),
            Span::styled(
                format!("  Turns: {thread_turns}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        if daemon_count > 0 {
            spans.push(Span::styled(
                format!("  Auto: {daemon_count}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        if window_count > 0 {
            let mut agent_label = format!("  Agents: {window_count}");
            if thinking > 0 {
                agent_label.push_str(&format!(" ({thinking} thinking)"));
            }
            if waiting > 0 {
                agent_label.push_str(&format!(" ({waiting} waiting)"));
            }
            spans.push(Span::styled(agent_label, Style::default().fg(Color::Cyan)));
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
            format!(" Memini [^{}] ", self.scroll_offset)
        } else {
            " Memini ".to_string()
        };

        let panel = log_paragraph
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(scroll_indicator),
            )
            .scroll((top_row, 0));
        frame.render_widget(panel, area);
    }

    // ── Agent panel (stacked agent windows) ──────────────────────────

    fn draw_agent_panel(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.agent_windows.is_empty() && self.daemon_handles.is_empty() {
            // Empty state.
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No agents running.",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  /spawn <prompt>  to start one",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "  /auto start <name>  for autopilot",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "  Ctrl+1..9 to focus a window",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let panel = Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title(" Agents "));
            frame.render_widget(panel, area);
            return;
        }

        // Split the panel area vertically among agent windows + optional daemon summary.
        let window_count = self.agent_windows.len();
        let has_daemons = !self.daemon_handles.is_empty();
        let total_sections = window_count + if has_daemons { 1 } else { 0 };

        if total_sections == 0 {
            return;
        }

        // Each agent window gets an equal share; daemons get a small fixed section.
        let mut constraints: Vec<Constraint> = Vec::new();
        if has_daemons {
            let daemon_height = (self.daemon_handles.len() as u16 + 2).min(6);
            constraints.push(Constraint::Length(daemon_height));
        }
        for _ in 0..window_count {
            constraints.push(Constraint::Min(5));
        }

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut section_idx = 0;

        // ── Daemon summary (compact) ──
        if has_daemons {
            let mut lines: Vec<Line> = Vec::new();
            for handle in &self.daemon_handles {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {} ", handle.def.name),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("every {}s", handle.def.interval_secs),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            let panel = Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title(" Autopilot "));
            frame.render_widget(panel, sections[section_idx]);
            section_idx += 1;
        }

        // ── Agent windows ──
        for window in &self.agent_windows {
            if section_idx >= sections.len() {
                break;
            }
            let win_area = sections[section_idx];
            section_idx += 1;

            let is_focused = self.focused_window == Some(window.id);

            // Status indicator.
            let (status_label, status_color) = match window.status {
                AgentWindowStatus::Thinking => ("thinking...", Color::Yellow),
                AgentWindowStatus::Done => ("done", Color::Green),
                AgentWindowStatus::WaitingForInput => ("NEEDS INPUT", Color::Red),
            };

            let title = format!(" [{}] {} -- {} ", window.id, window.label, status_label);

            let border_style = if is_focused {
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // Build output lines for this window.
            let inner_height = win_area.height.saturating_sub(2) as usize;
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
                        Color::Red
                    } else if s.starts_with("--") {
                        Color::DarkGray
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
                        .border_style(border_style)
                        .title(Span::styled(title, Style::default().fg(status_color))),
                )
                .wrap(Wrap { trim: false });

            frame.render_widget(panel, win_area);
        }
    }

    // ── Status-bar helpers ───────────────────────────────────────────

    fn mcp_status_label(&self) -> String {
        if let Some(connection) = &self.mcp_connection {
            format!("{} (connected)", connection.server.display_name())
        } else if let Some(server) = &self.active_mcp {
            format!("{} (saved)", server.display_name())
        } else {
            "none".to_string()
        }
    }

    fn mcp_status_color(&self) -> Color {
        if self.mcp_connection.is_some() {
            Color::Green
        } else if self.active_mcp.is_some() {
            Color::Yellow
        } else {
            Color::DarkGray
        }
    }

    fn openai_status_label(&self) -> String {
        match &self.openai_key_hint {
            Some(hint) => hint.clone(),
            None => "unset".to_string(),
        }
    }

    fn openai_status_color(&self) -> Color {
        if self.openai_key_hint.is_some() {
            Color::Green
        } else {
            Color::DarkGray
        }
    }

    fn rice_status_color(&self) -> Color {
        match self.rice.status {
            RiceStatus::Connected => Color::Green,
            RiceStatus::Disabled(_) => Color::DarkGray,
        }
    }
}
