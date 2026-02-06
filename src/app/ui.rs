//! Terminal UI rendering — layout, status bar, and activity panel.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::rice::RiceStatus;

use super::App;

impl App {
    /// Render the full TUI frame: header bar, activity log, and input prompt.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(frame.area());

        // ── Status bar ───────────────────────────────────────────────
        let thread_turns = self.conversation_thread.len() / 2;
        let daemon_count = self.daemon_handles.len();
        let mut header_spans = vec![
            Span::styled("Agent: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.active_agent.name.clone(),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled("  MCP: ", Style::default().fg(Color::DarkGray)),
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
                format!("  Thread: {thread_turns}"),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        if daemon_count > 0 {
            header_spans.push(Span::styled(
                format!("  ⚡{daemon_count}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        let header_line = Line::from(header_spans);
        frame.render_widget(Paragraph::new(header_line), chunks[0]);

        // ── Activity log ─────────────────────────────────────────────
        let inner_width = chunks[1].width.saturating_sub(2);
        let inner_height = chunks[1].height.saturating_sub(2) as usize;

        // Build the log paragraph with wrapping so we can query its
        // rendered line count (ratatui 0.30 native API).
        let log_lines: Vec<Line> = self.logs.iter().map(|l| l.render()).collect();
        let log_paragraph = Paragraph::new(Text::from(log_lines))
            .wrap(Wrap { trim: true });

        let total_visual = log_paragraph.line_count(inner_width);
        let max_scroll = total_visual.saturating_sub(inner_height);

        // Clamp scroll_offset (lines from the bottom) to valid range.
        if (self.scroll_offset as usize) > max_scroll {
            self.scroll_offset = max_scroll as u16;
        }
        let top_row = max_scroll.saturating_sub(self.scroll_offset as usize) as u16;

        let scroll_indicator = if self.scroll_offset > 0 {
            format!(" Activity [↑{}] ", self.scroll_offset)
        } else {
            " Activity ".to_string()
        };

        let log_panel = log_paragraph
            .block(Block::default().borders(Borders::ALL).title(scroll_indicator))
            .scroll((top_row, 0));
        frame.render_widget(log_panel, chunks[1]);

        // ── Input prompt ─────────────────────────────────────────────
        let input_panel = Paragraph::new(self.input.as_str())
            .block(Block::default().borders(Borders::ALL).title("Command"));
        frame.render_widget(input_panel, chunks[2]);

        let input_width = chunks[2].width.saturating_sub(2) as usize;
        let cursor = self.cursor.min(input_width);
        frame.set_cursor_position(Position::new(
            chunks[2].x + 1 + cursor as u16,
            chunks[2].y + 1,
        ));
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
