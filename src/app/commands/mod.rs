//! Slash-command dispatch and handler implementations.
//!
//! Every `/command` typed by the user is routed through [`App::handle_command`]
//! and dispatched to the appropriate handler in a focused submodule:
//!
//! | Module    | Commands                              |
//! |-----------|---------------------------------------|
//! | `mcp`     | `/mcp` – connect, auth, tools, call   |
//! | `openai`  | `/openai`, `/key`, `/rice`, bootstrap |
//! | `agents`  | `/agent`, `/thread`, `/memory`        |
//! | `daemons` | `/daemon`, `/auto`, `/spawn`, `/reply`|
//! | `share`   | `/share`                              |
//! | `skills`  | `/skills` — import/list skill packs   |

mod agents;
mod daemons;
mod mcp;
mod openai;
mod share;
mod skills;

use super::App;
use super::log_src;
use super::logging::LogLevel;

// ── Command dispatch ─────────────────────────────────────────────────

impl App {
    /// Route a slash-command to the matching handler.
    pub(crate) fn handle_command(&mut self, line: &str) -> anyhow::Result<()> {
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");

        match cmd {
            "/help" => self.show_help(),
            "/quit" | "/exit" => self.should_quit = true,
            "/clear" => self.logs.clear(),
            "/mcp" => self.handle_mcp_command(parts.collect()),
            "/openai" => self.handle_openai_command(parts.collect()),
            "/key" => self.handle_key_command(parts.collect()),
            "/rice" => self.handle_rice_command(parts.collect()),
            "/agent" => self.handle_agent_command(parts.collect()),
            "/thread" => self.handle_thread_command(parts.collect()),
            "/memory" | "/mem" => self.handle_memory_command(parts.collect()),
            "/daemon" | "/d" | "/auto" => self.handle_daemon_command(parts.collect()),
            "/spawn" => self.handle_spawn_command(parts.collect()),
            "/reply" | "/r" => self.handle_reply_command(parts.collect()),
            "/share" => self.handle_share_command(parts.collect()),
            "/skills" => self.handle_skills_command(parts.collect()),
            "/panel" => {
                self.show_side_panel = !self.show_side_panel;
                let state = if self.show_side_panel {
                    "shown"
                } else {
                    "hidden"
                };
                self.log(
                    LogLevel::Info,
                    format!("Side panel {state}. (You can also press Tab to toggle.)"),
                );
            }
            _ => log_src!(self, LogLevel::Warn, format!("Unknown command: {cmd}")),
        }

        Ok(())
    }
}

// ── Help ─────────────────────────────────────────────────────────────

impl App {
    fn show_help(&mut self) {
        let lines = [
            "━━━  ◆ Memini by AG\\I — your AI with a memory  ━━━",
            "",
            "Just type to chat — Memini by AG\\I remembers everything via Rice.",
            "",
            "Chat & Memory",
            "  (just type)             Talk to your AI — it recalls past chats",
            "  /memory <query>         Search your saved memories",
            "  /thread                 Show current conversation info",
            "  /thread clear           Start a fresh conversation",
            "",
            "Personas",
            "  /agent                  See available personas",
            "  /agent use <name>       Switch persona",
            "  /agent create <n> <d>   Create a custom persona",
            "  /agent delete <name>    Remove a custom persona",
            "  /agent info             Current persona details",
            "",
            "Autopilot (Background Tasks)",
            "  /auto                   See available background tasks",
            "  /auto run <name>        Run a task right now",
            "  /auto start <name>      Start a recurring task",
            "  /auto stop <name>       Stop a running task",
            "  /auto add <n> <s> <p>   Create a custom task (name, seconds, prompt)",
            "  /auto remove <name>     Remove a task",
            "  /auto results [name]    See recent task outputs",
            "",
            "Agents (Multi-Instance)",
            "  /spawn <prompt>         Spin up a live agent window",
            "  /spawn list             Show all agent windows + status",
            "  /reply list             Show agents waiting for input",
            "  /reply <id|next> <msg>  Reply from the main input box",
            "  #<id> <msg>             Inline reply shortcut",
            "  (plain text)            Sends to oldest waiting agent first (FIFO)",
            "  Tab                     Cycle through agents in grid",
            "  Enter                   Open selected agent session",
            "  Esc                     Return to dashboard from agent session",
            "  Ctrl+1..9               Jump to agent session by index",
            "",
            "Integrations",
            "  /mcp                    List available tools (MCP servers)",
            "  /mcp connect <id>       Connect to a tool (adds it to session)",
            "  /mcp auth <id>          Authenticate via browser (OAuth)",
            "  /mcp auth-code <id> <x> Finish OAuth with URL/code",
            "  /mcp ask <prompt>       Chat using connected tools",
            "  /mcp tools [id|all]     List MCP tools (namespaced: id__tool)",
            "  /mcp disconnect [id|all] Disconnect MCP server(s)",
            "",
            "Shared Workspaces (Team Memory)",
            "  /share                  Show current workspace status",
            "  /share join <name>      Join a shared workspace (team members use same name)",
            "  /share leave            Return to your private memory",
            "",
            "Skills (Agent Workflows)",
            "  /skills                 List imported skills",
            "  /skills import <url>    Import a skill from skills.sh or GitHub",
            "  /skills reload          Reload imported skills from disk",
            "",
            "Settings",
            "  /openai                 Show AI key status",
            "  /openai set <key>       Save your OpenAI key (stored in Rice)",
            "  /key <key>              Quick set OpenAI key",
            "  /rice                   Show Rice memory connection status",
            "  /rice setup             Interactive Rice environment wizard",
            "  /clear                  Clear the screen",
            "  /quit                   Exit Memini by AG\\I",
        ];
        for line in lines {
            self.log(LogLevel::Info, line.to_string());
        }
    }
}
