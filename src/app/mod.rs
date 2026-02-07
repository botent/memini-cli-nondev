//! Application core — state, lifecycle, and event dispatch.
//!
//! The [`App`] struct holds all runtime state and is the single entry point
//! for the rest of the binary.  Heavy concerns are delegated to focused
//! submodules:
//!
//! | Module       | Responsibility                            |
//! |--------------|-------------------------------------------|
//! | `chat`       | AI chat flow & tool loops                 |
//! | `commands`   | Slash-command dispatch & handlers          |
//! | `input`      | Text-input editing (cursor, insert, etc.) |
//! | `logging`    | `LogLevel`, `LogLine`, `mask_key`         |
//! | `store`      | Local on-disk MCP credential cache        |
//! | `ui`         | TUI rendering & status-bar helpers        |

mod agents;
mod chat;
mod commands;
mod daemon;
mod input;
mod logging;
mod store;
mod ui;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::{Context, Result};
use chrono::Local;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::constants::{DEFAULT_MEMORY_LIMIT, MAX_DAEMON_RESULTS, MAX_LOGS};
use crate::mcp::McpConnection;
use crate::mcp::config::{McpConfig, McpServer, McpSource};
use crate::mcp::oauth::PendingOAuth;
use crate::openai::OpenAiClient;
use crate::rice::RiceStore;
use crate::util::env_first;

use self::agents::Agent;
use self::daemon::{AgentEvent, AgentWindow, AgentWindowStatus, ChatLogLevel, DaemonHandle};
use self::logging::{LogContent, LogLevel, LogLine};
use self::store::{LocalMcpStore, load_local_mcp_store};

// ── View modes ───────────────────────────────────────────────────────

/// Which top-level screen the TUI is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    /// Home dashboard — status bar + 3×3 agent grid.
    Dashboard,
    /// Full-screen session for a single agent window (by id).
    AgentSession(usize),
}

// ── Application state ────────────────────────────────────────────────

/// Top-level application state.
///
/// Fields use `pub(crate)` visibility so that the sibling submodules
/// (`commands`, `chat`, `ui`, …) can access them directly while keeping
/// them hidden from the rest of the crate.
pub struct App {
    pub(crate) runtime: Runtime,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) logs: Vec<LogLine>,
    pub(crate) mcp_config: McpConfig,
    pub(crate) mcp_source: McpSource,
    pub(crate) active_mcp: Option<McpServer>,
    pub(crate) mcp_connections: HashMap<String, McpConnection>,
    pub(crate) local_mcp_store: LocalMcpStore,
    pub(crate) rice: RiceStore,
    pub(crate) active_agent: Agent,
    pub(crate) custom_agents: Vec<Agent>,
    pub(crate) conversation_thread: Vec<serde_json::Value>,
    pub(crate) openai_key_hint: Option<String>,
    pub(crate) openai_key: Option<String>,
    pub(crate) openai: OpenAiClient,
    pub(crate) memory_limit: u64,
    pub(crate) pending_oauth: Option<(String, PendingOAuth)>,
    pub(crate) scroll_offset: u16,
    pub(crate) should_quit: bool,
    pub(crate) show_side_panel: bool,
    // Input history (up/down arrow cycling)
    pub(crate) input_history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) history_stash: String,
    // Daemon (autonomous background agents)
    pub(crate) daemon_tx: mpsc::UnboundedSender<AgentEvent>,
    pub(crate) daemon_rx: mpsc::UnboundedReceiver<AgentEvent>,
    pub(crate) daemon_handles: Vec<DaemonHandle>,
    pub(crate) daemon_results: Vec<(String, String, String)>, // (task_name, message, timestamp)
    // Agent windows (live interactive agents in side panel)
    pub(crate) agent_windows: Vec<AgentWindow>,
    pub(crate) next_window_id: Arc<AtomicUsize>,
    pub(crate) focused_window: Option<usize>, // id of the focused window
    // Dashboard grid navigation
    pub(crate) view_mode: ViewMode,
    pub(crate) grid_selected: usize, // index into grid cells (0..8 for 3×3)
    // Chat-in-progress flag (prevents double-sends and shows thinking UI)
    pub(crate) chat_busy: bool,
}

// ── Lifecycle ────────────────────────────────────────────────────────

impl App {
    /// Create and initialise a new application instance.
    pub fn new() -> Result<Self> {
        let runtime = Runtime::new().context("create tokio runtime")?;
        let (mcp_config, mcp_source) = McpConfig::load()?;
        let local_mcp_store = load_local_mcp_store();
        let rice = runtime.block_on(RiceStore::connect());
        let memory_limit = env_first(&["MEMINI_MEMORY_LIMIT"])
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MEMORY_LIMIT);

        let (daemon_tx, daemon_rx) = mpsc::unbounded_channel();

        let mut app = App {
            runtime,
            input: String::new(),
            cursor: 0,
            logs: Vec::new(),
            mcp_config,
            mcp_source,
            active_mcp: None,
            mcp_connections: HashMap::new(),
            local_mcp_store,
            rice,
            active_agent: Agent::default(),
            custom_agents: Vec::new(),
            conversation_thread: Vec::new(),
            openai_key_hint: None,
            openai_key: None,
            openai: OpenAiClient::new(),
            memory_limit,
            pending_oauth: None,
            scroll_offset: 0,
            should_quit: false,
            show_side_panel: false,
            input_history: Vec::new(),
            history_index: None,
            history_stash: String::new(),
            daemon_tx,
            daemon_rx,
            daemon_handles: Vec::new(),
            daemon_results: Vec::new(),
            agent_windows: Vec::new(),
            next_window_id: Arc::new(AtomicUsize::new(1)),
            focused_window: None,
            view_mode: ViewMode::Dashboard,
            grid_selected: 0,
            chat_busy: false,
        };

        app.log(
            LogLevel::Info,
            format!(
                "Found {} tool integration(s).",
                app.mcp_config.servers.len(),
            ),
        );
        app.log(
            LogLevel::Info,
            "Welcome to Memini.  Just type to chat -- I remember everything via Rice.".to_string(),
        );
        app.log(
            LogLevel::Info,
            "Type /help to see what I can do.".to_string(),
        );

        app.bootstrap();
        Ok(app)
    }

    /// Load persisted state from Rice on startup.
    fn bootstrap(&mut self) {
        if let Err(err) = self.load_openai_from_rice() {
            log_src!(
                self,
                LogLevel::Warn,
                format!("OpenAI key load skipped: {err}")
            );
        }
        if let Err(err) = self.load_active_mcp_from_rice() {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Active MCP load skipped: {err}")
            );
        }

        // Auto-connect MCP servers we already have tokens for.
        self.autoconnect_saved_mcps();

        // Restore custom agents.
        match self.runtime.block_on(self.rice.load_custom_agents()) {
            Ok(Some(value)) => {
                if let Ok(agents) = serde_json::from_value::<Vec<Agent>>(value) {
                    self.custom_agents = agents;
                }
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    format!("Custom agents load skipped: {err}")
                );
            }
            _ => {}
        }

        // Restore active agent.
        match self.runtime.block_on(self.rice.load_active_agent_name()) {
            Ok(Some(name)) if name != "memini" => {
                if let Some(agent) = self.custom_agents.iter().find(|a| a.name == name) {
                    self.active_agent = agent.clone();
                    self.log(LogLevel::Info, format!("Persona: {}", agent.name));
                }
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    format!("Active agent load skipped: {err}")
                );
            }
            _ => {}
        }

        // Restore conversation thread.
        match self.runtime.block_on(self.rice.load_thread()) {
            Ok(thread) if !thread.is_empty() => {
                let turns = thread.len() / 2;
                self.conversation_thread = thread;
                self.log(
                    LogLevel::Info,
                    format!("Picked up where you left off ({turns} turn(s) from Rice)."),
                );
            }
            Err(err) => {
                log_src!(self, LogLevel::Warn, format!("Thread load skipped: {err}"));
            }
            _ => {}
        }

        // Restore shared workspace.
        match self.runtime.block_on(self.rice.load_shared_workspace()) {
            Ok(Some(name)) => {
                self.rice.join_workspace(&name);
                self.log(LogLevel::Info, format!("Rejoined shared workspace: {name}"));
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    format!("Shared workspace load skipped: {err}")
                );
            }
            _ => {}
        }
    }

    /// Whether the user has requested to quit.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }
}

// ── Event handling ───────────────────────────────────────────────────

impl App {
    /// Called on every tick of the main loop (before draw) so that
    /// background events are processed even when no user input arrives.
    pub fn tick(&mut self) {
        self.drain_daemon_events();
    }

    /// Route a terminal event to the appropriate handler.
    pub fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key)?,
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
        Ok(())
    }

    /// Dispatch a key press to input editing, commands, or control actions.
    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.should_quit = true,

            KeyEvent {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.logs.clear(),

            // Ctrl+1 through Ctrl+9: jump straight into an agent session.
            KeyEvent {
                code: KeyCode::Char(ch @ '1'..='9'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                let idx = (ch as usize) - ('1' as usize); // 0-based index
                if let Some(window) = self.agent_windows.get(idx) {
                    let wid = window.id;
                    self.view_mode = ViewMode::AgentSession(wid);
                    self.focused_window = Some(wid);
                }
            }

            KeyEvent { code, .. } => {
                // View-mode aware dispatch for arrow keys, Enter, Esc.
                match &self.view_mode {
                    ViewMode::Dashboard => self.handle_dashboard_key(code)?,
                    ViewMode::AgentSession(wid) => {
                        let wid = *wid;
                        self.handle_session_key(code, wid)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Key handling while on the dashboard (grid navigation + input).
    fn handle_dashboard_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            // Enter with empty input and a selected agent → open that session.
            KeyCode::Enter if self.input.is_empty() => {
                if let Some(window) = self.agent_windows.get(self.grid_selected) {
                    let wid = window.id;
                    self.view_mode = ViewMode::AgentSession(wid);
                    self.focused_window = Some(wid);
                }
                // If no agent in that cell, Enter with empty input is a no-op.
            }

            // Enter with text → submit input as usual (command / chat).
            KeyCode::Enter => {
                self.scroll_offset = 0;
                self.submit_input()?;
            }

            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.history_index = None;
                } else {
                    self.should_quit = true;
                }
            }

            // ── Text input ───────────────────────────────────────────
            KeyCode::Char(ch) => {
                self.scroll_offset = 0;
                self.history_index = None;
                self.insert_char(ch);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_cursor_left(),
            KeyCode::Right => self.move_cursor_right(),
            KeyCode::Home => self.move_cursor_home(),
            KeyCode::End => self.move_cursor_end(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::PageUp => self.scroll_up(10),
            KeyCode::PageDown => self.scroll_down(10),
            KeyCode::Tab => {
                // Tab on dashboard cycles grid selection forward.
                if !self.agent_windows.is_empty() {
                    self.grid_selected = (self.grid_selected + 1) % self.agent_windows.len().min(9);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Key handling while in an agent session (full-screen agent view).
    fn handle_session_key(&mut self, code: KeyCode, window_id: usize) -> Result<()> {
        match code {
            // Esc returns to dashboard.
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.history_index = None;
                } else {
                    self.view_mode = ViewMode::Dashboard;
                    self.focused_window = None;
                    // Keep grid_selected pointing at this agent.
                    if let Some(idx) = self.agent_windows.iter().position(|w| w.id == window_id) {
                        self.grid_selected = idx;
                    }
                }
            }

            KeyCode::Enter => {
                self.scroll_offset = 0;
                // In agent session, set focused_window so submit_input can reply.
                self.focused_window = Some(window_id);
                self.submit_input()?;
            }

            // Standard text editing.
            KeyCode::Char(ch) => {
                self.scroll_offset = 0;
                self.history_index = None;
                self.insert_char(ch);
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_cursor_left(),
            KeyCode::Right => self.move_cursor_right(),
            KeyCode::Home => self.move_cursor_home(),
            KeyCode::End => self.move_cursor_end(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::PageUp => self.scroll_up(10),
            KeyCode::PageDown => self.scroll_down(10),
            _ => {}
        }
        Ok(())
    }

    /// Submit the current input line for processing.
    fn submit_input(&mut self) -> Result<()> {
        let line = self.input.trim().to_string();
        self.input.clear();
        self.cursor = 0;
        self.history_index = None;

        if line.is_empty() {
            return Ok(());
        }

        // Push to input history (skip consecutive duplicates).
        if self.input_history.last().map_or(true, |prev| prev != &line) {
            self.input_history.push(line.clone());
        }

        // If an agent window is focused and waiting for input, reply to it.
        if let Some(focused_id) = self.focused_window {
            if self.reply_to_agent_window(focused_id, &line) {
                return Ok(());
            }
        }

        if line.starts_with('/') {
            self.handle_command(&line)?;
        } else {
            // Prevent double-sends while the LLM is working.
            if self.chat_busy {
                self.log(LogLevel::Info, "Still thinking… please wait.".to_string());
                return Ok(());
            }

            // Show the user's message immediately in the activity log
            // so they know the input was received.
            self.log(LogLevel::Info, format!("› {line}"));
            self.chat_busy = true;
            // Launch the chat on a background task — returns immediately.
            // chat_busy is cleared when we receive ChatFinished in drain_daemon_events.
            self.handle_chat_message(&line, false);
        }

        Ok(())
    }
}

// ── Scrolling & mouse ────────────────────────────────────────────────

impl App {
    /// Scroll the activity log up by `n` lines.
    pub(crate) fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Scroll the activity log down by `n` lines (towards the latest).
    pub(crate) fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Handle mouse events (scroll wheel / trackpad).
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(3),
            MouseEventKind::ScrollDown => self.scroll_down(3),
            _ => {}
        }
    }
}

// ── Logging ──────────────────────────────────────────────────────────

/// Log a `Warn`/`Error` message, attaching `[file:line]` in debug-logs builds.
///
/// In release (no `debug-logs` feature) this behaves like `self.log()`.
///
/// ```ignore
/// log_src!(self, LogLevel::Warn, format!("something broke: {err:#}"));
/// ```
macro_rules! log_src {
    ($app:expr, $level:expr, $msg:expr) => {{
        #[cfg(feature = "debug-logs")]
        {
            let loc = format!("{}:{}", file!(), line!());
            $app.log_with_src($level, $msg, &loc);
        }
        #[cfg(not(feature = "debug-logs"))]
        {
            $app.log($level, $msg);
        }
    }};
}
pub(crate) use log_src;

impl App {
    /// Append a plain-text message to the activity log.
    pub(crate) fn log(&mut self, level: LogLevel, message: String) {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        self.logs.push(LogLine {
            timestamp,
            level,
            content: LogContent::Plain(message),
        });
        if self.logs.len() > MAX_LOGS {
            let overflow = self.logs.len() - MAX_LOGS;
            self.logs.drain(0..overflow);
        }
    }

    /// Append markdown content (LLM output) to the activity log.
    pub(crate) fn log_markdown(&mut self, label: String, body: String) {
        let timestamp = Local::now().format("%H:%M:%S").to_string();
        self.logs.push(LogLine {
            timestamp,
            level: LogLevel::Info,
            content: LogContent::Markdown { label, body },
        });
        if self.logs.len() > MAX_LOGS {
            let overflow = self.logs.len() - MAX_LOGS;
            self.logs.drain(0..overflow);
        }
    }

    /// Append a message with a source location suffix (debug-logs builds only).
    #[cfg(feature = "debug-logs")]
    pub(crate) fn log_with_src(&mut self, level: LogLevel, message: String, src: &str) {
        let tagged = match level {
            LogLevel::Warn | LogLevel::Error => format!("{message}  [{src}]"),
            _ => message,
        };
        self.log(level, tagged);
    }
}

// ── Daemon (background agents) ───────────────────────────────────────

impl App {
    /// Drain pending background agent events and route them.
    pub(crate) fn drain_daemon_events(&mut self) {
        while let Ok(event) = self.daemon_rx.try_recv() {
            match event {
                AgentEvent::Started { window_id } => {
                    if let Some(win) = self.agent_windows.iter_mut().find(|w| w.id == window_id) {
                        win.status = AgentWindowStatus::Thinking;
                        win.output_lines.push("-- started --".to_string());
                    }
                }
                AgentEvent::Progress { window_id, line } => {
                    if let Some(win) = self.agent_windows.iter_mut().find(|w| w.id == window_id) {
                        win.output_lines.push(line);
                    }
                }
                AgentEvent::Finished {
                    window_id,
                    message,
                    timestamp,
                } => {
                    if let Some(win) = self.agent_windows.iter_mut().find(|w| w.id == window_id) {
                        win.status = AgentWindowStatus::Done;
                        win.output_lines.push(format!("-- done at {timestamp} --"));
                        win.pending_question = None;
                    }
                    // Also log to main chat.
                    let label = self
                        .agent_windows
                        .iter()
                        .find(|w| w.id == window_id)
                        .map(|w| w.label.clone())
                        .unwrap_or_else(|| format!("agent-{window_id}"));
                    self.log_markdown(label, message);
                }
                AgentEvent::NeedsInput {
                    window_id,
                    question,
                } => {
                    if let Some(win) = self.agent_windows.iter_mut().find(|w| w.id == window_id) {
                        win.status = AgentWindowStatus::WaitingForInput;
                        win.pending_question = Some(question.clone());
                        win.output_lines
                            .push(format!(">> Waiting for your input: {question}"));
                    }
                    // Auto-navigate into the agent session that needs input.
                    self.focused_window = Some(window_id);
                    self.view_mode = ViewMode::AgentSession(window_id);
                }
                AgentEvent::DaemonResult {
                    task_name,
                    message,
                    timestamp,
                } => {
                    let label = format!("{task_name} (background)");
                    self.log_markdown(label, message.clone());
                    self.daemon_results.push((task_name, message, timestamp));
                    if self.daemon_results.len() > MAX_DAEMON_RESULTS {
                        self.daemon_results.remove(0);
                    }
                }

                // ── Main-chat async events ───────────────────────────
                AgentEvent::ChatProgress { line, level } => {
                    let log_level = match level {
                        ChatLogLevel::Info => LogLevel::Info,
                        ChatLogLevel::Warn => LogLevel::Warn,
                        ChatLogLevel::Error => LogLevel::Error,
                    };
                    self.log(log_level, line);
                }
                AgentEvent::ChatMarkdown { label, body } => {
                    self.log_markdown(label, body);
                }
                AgentEvent::ChatFinished {
                    user_message: _,
                    output_text: _,
                    agent_name: _,
                    thread_entries,
                } => {
                    // Update conversation thread with this turn.
                    for entry in thread_entries {
                        self.conversation_thread.push(entry);
                    }
                    // Trim thread if over limit.
                    let max = crate::constants::MAX_THREAD_MESSAGES;
                    while self.conversation_thread.len() > max {
                        self.conversation_thread.drain(0..2);
                    }
                    // Persist thread to Rice (best-effort).
                    let _ = self
                        .runtime
                        .block_on(self.rice.save_thread(&self.conversation_thread));
                    self.chat_busy = false;
                }
                AgentEvent::ChatSpawnAgent {
                    window_id,
                    label,
                    prompt,
                    mcp_snapshots,
                    coordination_key,
                    persona,
                } => {
                    // Create the agent window on the main thread.
                    let window = AgentWindow {
                        id: window_id,
                        label: label.clone(),
                        prompt: prompt.clone(),
                        status: AgentWindowStatus::Thinking,
                        output_lines: Vec::new(),
                        pending_question: None,
                        scroll: 0,
                    };
                    self.agent_windows.push(window);
                    let idx = self.agent_windows.len().saturating_sub(1);
                    self.grid_selected = idx;

                    // Spawn the sub-agent background task.
                    let tx = self.daemon_tx.clone();
                    let openai = self.openai.clone();
                    let key = self.openai_key.clone();
                    let rice_handle = self.runtime.spawn(crate::rice::RiceStore::connect());
                    let has_mcp = !mcp_snapshots.is_empty();

                    if has_mcp {
                        daemon::spawn_agent_window_with_mcp(
                            window_id,
                            coordination_key,
                            persona,
                            prompt,
                            mcp_snapshots,
                            tx,
                            openai,
                            key,
                            rice_handle,
                            self.runtime.handle().clone(),
                        );
                    } else {
                        daemon::spawn_agent_window(
                            window_id,
                            persona,
                            prompt,
                            tx,
                            openai,
                            key,
                            rice_handle,
                            self.runtime.handle().clone(),
                        );
                    }
                }
            }
        }
    }

    /// Focus an agent window by its id — opens the session view.
    #[allow(dead_code)]
    pub(crate) fn focus_agent_window(&mut self, id: usize) {
        if self.agent_windows.iter().any(|w| w.id == id) {
            self.focused_window = Some(id);
            self.view_mode = ViewMode::AgentSession(id);
        }
    }

    /// Reply to a focused agent window that is waiting for input.
    /// Returns true if the reply was handled, false if no window was waiting.
    pub(crate) fn reply_to_agent_window(&mut self, window_id: usize, reply: &str) -> bool {
        let waiting = self
            .agent_windows
            .iter()
            .any(|w| w.id == window_id && w.status == AgentWindowStatus::WaitingForInput);

        if !waiting {
            return false;
        }

        // Update the window.
        if let Some(win) = self.agent_windows.iter_mut().find(|w| w.id == window_id) {
            win.output_lines.push(format!(">> You: {reply}"));
            win.status = AgentWindowStatus::Thinking;
            win.pending_question = None;
        }

        let _persona = self
            .agent_windows
            .iter()
            .find(|w| w.id == window_id)
            .map(|w| w.label.clone())
            .unwrap_or_default();

        let prompt = format!(
            "The user replied to your question: \"{reply}\"\n\
             Continue working on the original task."
        );

        self.log(
            LogLevel::Info,
            format!("Replied to agent window {window_id}: {reply}"),
        );

        // Spawn a continuation.
        let tx = self.daemon_tx.clone();
        let openai = self.openai.clone();
        let key = self.openai_key.clone();
        let rice_handle = self.runtime.spawn(RiceStore::connect());
        let active_persona = self.active_agent.persona.clone();

        daemon::spawn_agent_window(
            window_id,
            active_persona,
            prompt,
            tx,
            openai,
            key,
            rice_handle,
            self.runtime.handle().clone(),
        );

        true
    }

    /// Spawn a background daemon task, connecting it to the shared channel.
    pub(crate) fn spawn_daemon_task(&mut self, def: daemon::DaemonTaskDef) {
        let tx = self.daemon_tx.clone();
        let openai = self.openai.clone();
        let key = self.openai_key.clone();

        // Each daemon task gets its own Rice connection (async).
        let rice_handle = self.runtime.spawn(RiceStore::connect());

        let handle = daemon::spawn_task(
            def,
            tx,
            openai,
            key,
            rice_handle,
            self.runtime.handle().clone(),
        );
        self.log(
            LogLevel::Info,
            format!(
                "Task '{}' started (every {}s).",
                handle.def.name, handle.def.interval_secs
            ),
        );
        self.daemon_handles.push(handle);
    }

    /// Fire a one-shot background run of a daemon task definition.
    pub(crate) fn run_daemon_oneshot(&mut self, def: daemon::DaemonTaskDef) {
        let tx = self.daemon_tx.clone();
        let openai = self.openai.clone();
        let key = self.openai_key.clone();
        let rice_handle = self.runtime.spawn(RiceStore::connect());

        self.log(LogLevel::Info, format!("Running '{}' now...", def.name));
        daemon::spawn_oneshot(
            def,
            tx,
            openai,
            key,
            rice_handle,
            self.runtime.handle().clone(),
        );
    }
}
