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
use self::daemon::{AgentEvent, DaemonHandle};
use self::logging::{LogContent, LogLevel, LogLine};
use self::store::{LocalMcpStore, load_local_mcp_store};

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
    pub(crate) mcp_connection: Option<McpConnection>,
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
    // Input history (up/down arrow cycling)
    pub(crate) input_history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) history_stash: String,
    // Daemon (autonomous background agents)
    pub(crate) daemon_tx: mpsc::UnboundedSender<AgentEvent>,
    pub(crate) daemon_rx: mpsc::UnboundedReceiver<AgentEvent>,
    pub(crate) daemon_handles: Vec<DaemonHandle>,
    pub(crate) daemon_results: Vec<AgentEvent>,
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
            mcp_connection: None,
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
            input_history: Vec::new(),
            history_index: None,
            history_stash: String::new(),
            daemon_tx,
            daemon_rx,
            daemon_handles: Vec::new(),
            daemon_results: Vec::new(),
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
    /// Route a terminal event to the appropriate handler.
    pub fn handle_event(&mut self, event: Event) -> Result<()> {
        // Drain any background agent results first.
        self.drain_daemon_events();

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

            KeyEvent { code, .. } => match code {
                KeyCode::Char(ch) => {
                    self.scroll_offset = 0; // snap to bottom on new input
                    self.history_index = None; // reset history browse
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
                KeyCode::Enter => {
                    self.scroll_offset = 0; // snap to bottom on submit
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
                _ => {}
            },
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

        if line.starts_with('/') {
            self.handle_command(&line)?;
        } else {
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
    /// Drain pending background agent results and display them.
    pub(crate) fn drain_daemon_events(&mut self) {
        while let Ok(event) = self.daemon_rx.try_recv() {
            let label = format!("{} (background)", event.task_name);
            self.log_markdown(label, event.message.clone());
            self.daemon_results.push(event);
            if self.daemon_results.len() > MAX_DAEMON_RESULTS {
                self.daemon_results.remove(0);
            }
        }
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
