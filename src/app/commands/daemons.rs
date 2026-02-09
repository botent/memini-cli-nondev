//! `/daemon` (`/auto`) and `/spawn` command handlers — background task
//! management and live agent window creation.

use super::super::App;
use super::super::daemon;
use super::super::log_src;
use super::super::logging::LogLevel;

// ── /daemon ──────────────────────────────────────────────────────────

impl App {
    pub(crate) fn handle_daemon_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.list_daemons();
            return;
        }

        match args[0] {
            "list" => self.list_daemons(),
            "run" => {
                if let Some(name) = args.get(1) {
                    self.run_daemon_now(name);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /daemon run <name>".to_string()
                    );
                }
            }
            "start" => {
                if let Some(name) = args.get(1) {
                    self.start_daemon(name);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /daemon start <name>".to_string()
                    );
                }
            }
            "stop" => {
                if let Some(name) = args.get(1) {
                    self.stop_daemon(name);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /daemon stop <name>".to_string()
                    );
                }
            }
            "add" => {
                // /daemon add <name> <interval_secs> <prompt...>
                if args.len() >= 4 {
                    let name = args[1].to_string();
                    let interval: u64 = args[2]
                        .parse()
                        .unwrap_or(crate::constants::DEFAULT_AGENT_INTERVAL_SECS);
                    let prompt = args[3..].join(" ");
                    self.add_daemon_task(&name, interval, &prompt);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /daemon add <name> <interval_secs> <prompt>".to_string()
                    );
                }
            }
            "remove" => {
                if let Some(name) = args.get(1) {
                    self.remove_daemon_task(name);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /daemon remove <name>".to_string()
                    );
                }
            }
            "results" => {
                let filter = args.get(1).copied();
                self.show_daemon_results(filter);
            }
            other => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    format!("Unknown /daemon command: {other}")
                );
            }
        }
    }

    fn list_daemons(&mut self) {
        let builtins = daemon::builtin_tasks();

        if self.daemon_handles.is_empty() && builtins.is_empty() {
            self.log(LogLevel::Info, "No daemon tasks configured.".to_string());
            return;
        }

        self.log(LogLevel::Info, "Background tasks:".to_string());

        // Show built-in tasks (even if not running).
        for builtin in &builtins {
            let running = self
                .daemon_handles
                .iter()
                .any(|h| h.def.name == builtin.name);
            let status = if running { "running" } else { "available" };
            self.log(
                LogLevel::Info,
                format!(
                    "  {} -- {} [{}s interval, {}]",
                    builtin.name, builtin.prompt, builtin.interval_secs, status
                ),
            );
        }

        // Show custom running daemons not in builtins.
        let custom_daemons: Vec<_> = self
            .daemon_handles
            .iter()
            .filter(|h| !builtins.iter().any(|b| b.name == h.def.name))
            .map(|h| {
                (
                    h.def.name.clone(),
                    h.def.prompt.clone(),
                    h.def.interval_secs,
                )
            })
            .collect();
        for (name, prompt, interval) in &custom_daemons {
            self.log(
                LogLevel::Info,
                format!("  {name} -- {prompt} [{interval}s interval, running]"),
            );
        }
    }

    fn run_daemon_now(&mut self, name: &str) {
        // Check if it's a running handle — wake it.
        for handle in &self.daemon_handles {
            if handle.def.name == name {
                handle.wake.notify_one();
                self.log(
                    LogLevel::Info,
                    format!("Woke task '{name}' for immediate run."),
                );
                return;
            }
        }

        // Otherwise, find it in builtins and run as one-shot.
        let builtins = daemon::builtin_tasks();
        if let Some(def) = builtins.into_iter().find(|b| b.name == name) {
            self.run_daemon_oneshot(def);
            return;
        }

        log_src!(self, LogLevel::Warn, format!("Unknown daemon task: {name}"));
    }

    fn start_daemon(&mut self, name: &str) {
        // Don't start if already running.
        if self.daemon_handles.iter().any(|h| h.def.name == name) {
            self.log(
                LogLevel::Info,
                format!("Daemon '{name}' is already running."),
            );
            return;
        }

        // Find in builtins.
        let builtins = daemon::builtin_tasks();
        if let Some(mut def) = builtins.into_iter().find(|b| b.name == name) {
            def.paused = false;
            self.spawn_daemon_task(def);
            return;
        }

        log_src!(self, LogLevel::Warn, format!("Unknown daemon task: {name}"));
    }

    fn stop_daemon(&mut self, name: &str) {
        if let Some(pos) = self.daemon_handles.iter().position(|h| h.def.name == name) {
            let handle = self.daemon_handles.remove(pos);
            handle.abort.abort();
            self.log(LogLevel::Info, format!("Task '{name}' stopped."));
        } else {
            log_src!(
                self,
                LogLevel::Warn,
                format!("No running daemon named '{name}'.")
            );
        }
    }

    fn add_daemon_task(&mut self, name: &str, interval: u64, prompt: &str) {
        // Don't allow duplicate names.
        let builtins = daemon::builtin_tasks();
        if builtins.iter().any(|b| b.name == name)
            || self.daemon_handles.iter().any(|h| h.def.name == name)
        {
            log_src!(
                self,
                LogLevel::Warn,
                format!("A daemon named '{name}' already exists.")
            );
            return;
        }

        let def = daemon::DaemonTaskDef {
            name: name.to_string(),
            persona: format!(
                "You are a background autonomous agent named '{name}'. \
                 You run periodically and have access to the user's memory. \
                 Be concise and actionable."
            ),
            prompt: prompt.to_string(),
            interval_secs: interval,
            paused: false,
        };

        self.spawn_daemon_task(def);
        self.log(
            LogLevel::Info,
            format!("Custom task '{name}' created and started (every {interval}s)."),
        );
    }

    fn remove_daemon_task(&mut self, name: &str) {
        // Stop if running.
        if let Some(pos) = self.daemon_handles.iter().position(|h| h.def.name == name) {
            let handle = self.daemon_handles.remove(pos);
            handle.abort.abort();
        }

        self.log(LogLevel::Info, format!("Task '{name}' removed."));
    }

    fn show_daemon_results(&mut self, filter: Option<&str>) {
        let results: Vec<_> = self
            .daemon_results
            .iter()
            .filter(|r| filter.is_none() || Some(r.0.as_str()) == filter)
            .map(|r| (r.2.clone(), r.0.clone(), r.1.clone()))
            .collect();

        if results.is_empty() {
            self.log(
                LogLevel::Info,
                "No daemon results yet. Run /daemon run <name> to trigger one.".to_string(),
            );
            return;
        }

        self.log(
            LogLevel::Info,
            format!("Recent daemon results ({}):", results.len()),
        );
        for (ts, name, msg) in results.iter().rev().take(10) {
            self.log(LogLevel::Info, format!("  [{ts}] {name} -- {msg}"));
        }
    }
}

// ── /reply ───────────────────────────────────────────────────────────

impl App {
    pub(crate) fn handle_reply_command(&mut self, args: Vec<&str>) {
        if args.is_empty() || args[0] == "list" {
            self.list_waiting_agent_questions();
            return;
        }

        if args.len() < 2 {
            log_src!(
                self,
                LogLevel::Warn,
                "Usage: /reply <id|next> <message>  or  /reply list".to_string()
            );
            return;
        }

        let target = args[0];
        let reply = args[1..].join(" ");
        if reply.trim().is_empty() {
            log_src!(
                self,
                LogLevel::Warn,
                "Reply message cannot be empty.".to_string()
            );
            return;
        }

        let window_id = if target.eq_ignore_ascii_case("next") {
            self.first_waiting_window_id()
        } else {
            target.parse::<usize>().ok()
        };

        let Some(window_id) = window_id else {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Invalid target '{target}'. Use /reply list.")
            );
            return;
        };

        if !self.reply_to_agent_window(window_id, &reply) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Agent #{window_id} is not waiting for input.")
            );
        }
    }

    fn list_waiting_agent_questions(&mut self) {
        let waiting = self.waiting_window_summaries();
        if waiting.is_empty() {
            self.log(
                LogLevel::Info,
                "No agents are waiting for input.".to_string(),
            );
            return;
        }

        self.log(
            LogLevel::Info,
            format!("Agents waiting for input ({}):", waiting.len()),
        );
        for (id, label, question) in waiting {
            let preview: String = question.chars().take(140).collect();
            self.log(LogLevel::Info, format!("  #{id} {label} -- {preview}"));
        }
        self.log(
            LogLevel::Info,
            "Reply with /reply <id|next> <message> or inline #<id> <message>.".to_string(),
        );
    }
}

// ── /spawn ───────────────────────────────────────────────────────────

impl App {
    pub(crate) fn handle_spawn_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.log(
                LogLevel::Info,
                "Usage: /spawn <prompt>  or  /spawn list".to_string(),
            );
            self.log(
                LogLevel::Info,
                "Spin up a live agent window. Watch it think in real time, and reply if it needs help.".to_string(),
            );
            self.log(
                LogLevel::Info,
                "Use Ctrl+1..9 to focus a window, Ctrl+0 to unfocus.".to_string(),
            );
            return;
        }

        if args[0] == "list" {
            self.list_spawned_agents();
            return;
        }

        // Everything after /spawn is the prompt.
        let prompt = args.join(" ");
        self.spawn_agent_window_cmd(&prompt);
    }

    fn spawn_agent_window_cmd(&mut self, prompt: &str) {
        use std::sync::atomic::Ordering;
        let window_id = self.next_window_id.fetch_add(1, Ordering::SeqCst);
        let label = format!("Agent #{window_id}");

        // Create the window in Thinking state.
        let window = daemon::AgentWindow {
            id: window_id,
            label: label.clone(),
            prompt: prompt.to_string(),
            status: daemon::AgentWindowStatus::Thinking,
            output_lines: Vec::new(),
            pending_question: None,
            scroll: 0,
        };
        self.agent_windows.push(window);

        // Spawn the background task.
        let tx = self.daemon_tx.clone();
        let openai = self.openai.clone();
        let key = self.openai_key.clone();
        let rice_handle = self.runtime.spawn(crate::rice::RiceStore::connect());
        let persona = self.active_agent.persona.clone();
        let skill_context = self.skills_prompt_context(prompt);

        daemon::spawn_agent_window(
            window_id,
            persona,
            prompt.to_string(),
            skill_context,
            tx,
            openai,
            key,
            rice_handle,
            self.runtime.handle().clone(),
        );

        self.log(
            LogLevel::Info,
            format!("Spawned {label} — opening session."),
        );

        // Auto-navigate into the agent session.
        self.focused_window = Some(window_id);
        self.view_mode = super::super::ViewMode::AgentSession(window_id);
        // Also select this cell in the grid for when we come back.
        let idx = self.agent_windows.len().saturating_sub(1);
        self.grid_selected = idx;
    }

    fn list_spawned_agents(&mut self) {
        if self.agent_windows.is_empty() {
            self.log(
                LogLevel::Info,
                "No agent windows. Use /spawn <prompt> to create one.".to_string(),
            );
            return;
        }

        self.log(
            LogLevel::Info,
            format!("Agent windows ({}):", self.agent_windows.len()),
        );
        let windows: Vec<_> = self
            .agent_windows
            .iter()
            .map(|w| {
                let status = match w.status {
                    daemon::AgentWindowStatus::Thinking => "thinking",
                    daemon::AgentWindowStatus::Done => "done",
                    daemon::AgentWindowStatus::WaitingForInput => "WAITING FOR INPUT",
                };
                (w.id, w.label.clone(), w.prompt.clone(), status)
            })
            .collect();
        for (id, label, prompt, status) in &windows {
            let preview: String = prompt.chars().take(60).collect();
            self.log(
                LogLevel::Info,
                format!("  [{id}] {label} -- {preview}  [{status}]"),
            );
        }
    }
}
