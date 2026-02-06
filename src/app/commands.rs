//! Slash-command dispatch and handler implementations.
//!
//! Every `/command` typed by the user is routed through [`App::handle_command`]
//! and dispatched to the appropriate handler method in this module.

use std::env;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::constants::{ACTIVE_MCP_VAR, OPENAI_KEY_VAR};
use crate::mcp;
use crate::mcp::config::{McpAuth, McpConfig, McpServer};
use crate::openai::format_json;
use crate::rice::RiceStatus;

use super::App;
use super::agents::Agent;
use super::log_src;
use super::logging::{LogLevel, mask_key};
use super::store::persist_local_mcp_store;

// ── Command dispatch ─────────────────────────────────────────────────

impl App {
    /// Route a slash-command to the matching handler.
    pub(crate) fn handle_command(&mut self, line: &str) -> Result<()> {
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");

        match cmd {
            "/help" => self.show_help(),
            "/quit" | "/exit" => self.should_quit = true,
            "/clear" => self.logs.clear(),
            "/mcp" => self.handle_mcp_command(parts.collect()),
            "/openai" => self.handle_openai_command(parts.collect()),
            "/key" => self.handle_key_command(parts.collect()),
            "/rice" => self.handle_rice_command(),
            "/agent" => self.handle_agent_command(parts.collect()),
            "/thread" => self.handle_thread_command(parts.collect()),
            "/memory" | "/mem" => self.handle_memory_command(parts.collect()),
            "/daemon" | "/d" => self.handle_daemon_command(parts.collect()),
            _ => log_src!(self, LogLevel::Warn, format!("Unknown command: {cmd}")),
        }

        Ok(())
    }
}

// ── Help ─────────────────────────────────────────────────────────────

impl App {
    fn show_help(&mut self) {
        let lines = [
            "Commands:",
            "(no slash)              Chat with AI (multi-turn, persisted)",
            "/agent                  List agents",
            "/agent use <name>       Switch to agent",
            "/agent create <n> <d>   Create custom agent",
            "/agent delete <name>    Delete custom agent",
            "/agent info             Show current agent details",
            "/thread                 Show conversation info",
            "/thread clear           Clear conversation thread",
            "/memory <query>         Search Rice memories",
            "/daemon                 List background daemon tasks",
            "/daemon run <name>      Run a daemon task now",
            "/daemon start <name>    Start a periodic daemon",
            "/daemon stop <name>     Stop a running daemon",
            "/daemon add <n> <s> <p> Add custom daemon (name, secs, prompt)",
            "/daemon remove <name>   Remove a daemon task",
            "/daemon results [name]  Show recent daemon results",
            "/mcp                    List MCP servers",
            "/mcp connect <id>       Set active MCP",
            "/mcp ask <prompt>       Chat using MCP tools",
            "/mcp auth <id>          Run OAuth flow",
            "/mcp tools              List tools on active MCP",
            "/mcp call <tool> <json> Call MCP tool with JSON args",
            "/mcp disconnect         Close MCP connection",
            "/mcp reload             Reload MCP config",
            "/openai                 Show OpenAI key status",
            "/openai set <key>       Persist OpenAI key in Rice",
            "/key <key>              Set OpenAI key",
            "/openai clear           Remove OpenAI key",
            "/rice                   Show Rice connection status",
            "/clear                  Clear activity log",
            "/quit                   Exit",
        ];
        for line in lines {
            self.log(LogLevel::Info, line.to_string());
        }
    }
}

// ── MCP commands ─────────────────────────────────────────────────────

impl App {
    fn handle_mcp_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.list_mcp_servers();
            return;
        }

        match args[0] {
            "connect" | "use" => {
                if let Some(target) = args.get(1) {
                    self.connect_mcp(target);
                } else {
                    log_src!(self, LogLevel::Warn, "Usage: /mcp connect <id>".to_string());
                }
            }
            "disconnect" => self.disconnect_mcp(),
            "status" => self.show_mcp_status(),
            "tools" => self.list_mcp_tools(),
            "call" => {
                let tool = args.get(1).copied();
                let rest = if args.len() > 2 { &args[2..] } else { &[] };
                self.call_mcp_tool(tool, rest);
            }
            "ask" => {
                if args.len() > 1 {
                    let prompt = args[1..].join(" ");
                    self.handle_chat_message(&prompt, true);
                } else {
                    log_src!(self, LogLevel::Warn, "Usage: /mcp ask <prompt>".to_string());
                }
            }
            "auth" => {
                if let Some(target) = args.get(1) {
                    self.authenticate_mcp(target);
                } else {
                    log_src!(self, LogLevel::Warn, "Usage: /mcp auth <id>".to_string());
                }
            }
            "auth-code" => {
                if args.len() >= 3 {
                    let id = args[1].to_string();
                    let code_or_url = args[2..].join(" ");
                    self.complete_oauth_manual(&id, &code_or_url);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /mcp auth-code <id> <url-or-code>".to_string()
                    );
                }
            }
            "token" => {
                if args.len() >= 3 {
                    let id = args[1];
                    let token = args[2];
                    self.store_mcp_token(id, token);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /mcp token <id> <token>".to_string()
                    );
                }
            }
            "token-clear" => {
                if let Some(target) = args.get(1) {
                    self.clear_mcp_token(target);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /mcp token-clear <id>".to_string()
                    );
                }
            }
            "reload" => self.reload_mcp_config(),
            other => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    format!("Unknown /mcp command: {other}")
                );
            }
        }
    }

    fn list_mcp_servers(&mut self) {
        if self.mcp_config.servers.is_empty() {
            log_src!(
                self,
                LogLevel::Warn,
                "No MCP servers configured.".to_string()
            );
            return;
        }

        self.log(LogLevel::Info, "Available MCP servers:".to_string());
        let servers = self.mcp_config.servers.clone();
        for server in servers {
            let transport = server.transport.as_deref().unwrap_or("http");
            let auth = server
                .auth
                .as_ref()
                .map(|auth| auth.auth_type.as_str())
                .unwrap_or("none");
            self.log(
                LogLevel::Info,
                format!(
                    "- {} ({}) [transport: {transport}, auth: {auth}]",
                    server.display_name(),
                    server.url
                ),
            );
        }
    }

    fn connect_mcp(&mut self, target: &str) {
        let Some(server) = self.mcp_config.find_by_id_or_name(target) else {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Unknown MCP server: {target}")
            );
            return;
        };

        let transport = server.transport.as_deref().unwrap_or("http");
        if transport != "http" {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Transport '{transport}' not supported yet.")
            );
            return;
        }

        let bearer = self.resolve_mcp_token(&server);
        if bearer.is_none() {
            if let Some(auth) = &server.auth {
                if auth.auth_type == "oauth_browser" {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "No bearer token found. Run /mcp auth <id> for OAuth.".to_string()
                    );
                }
            }
        }

        let connect_result = self
            .runtime
            .block_on(mcp::connect_http(&server, bearer.clone()));

        match connect_result {
            Ok(connection) => {
                self.active_mcp = Some(server.clone());
                self.mcp_connection = Some(connection);

                let store_result = self.runtime.block_on(self.rice.set_variable(
                    ACTIVE_MCP_VAR,
                    serde_json::to_value(&server).unwrap_or(Value::Null),
                    "explicit",
                ));
                if let Err(err) = store_result {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        format!("Failed to persist MCP: {err:#}")
                    );
                }

                let token_hint = bearer.as_ref().map(|token| mask_key(token));
                match token_hint {
                    Some(hint) => self.log(
                        LogLevel::Info,
                        format!("Connected to {} (auth {hint}).", server.display_name()),
                    ),
                    None => self.log(
                        LogLevel::Info,
                        format!("Connected to {}.", server.display_name()),
                    ),
                }

                self.list_mcp_tools();
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Error,
                    format!("Failed to connect MCP: {err:#}")
                );
            }
        }
    }

    fn disconnect_mcp(&mut self) {
        if self.mcp_connection.is_some() {
            self.mcp_connection = None;
            self.log(LogLevel::Info, "MCP connection closed.".to_string());
        } else {
            self.log(LogLevel::Info, "No MCP connection to close.".to_string());
        }
    }

    /// Refresh and display the tool list from the active MCP connection.
    pub(crate) fn list_mcp_tools(&mut self) {
        let tools_result = {
            let Some(connection) = self.mcp_connection.as_mut() else {
                log_src!(
                    self,
                    LogLevel::Warn,
                    "No active MCP connection.".to_string()
                );
                return;
            };
            self.runtime.block_on(mcp::refresh_tools(connection))
        };

        match tools_result {
            Ok(tools) => {
                if tools.is_empty() {
                    self.log(LogLevel::Info, "No tools reported by MCP.".to_string());
                } else {
                    self.log(LogLevel::Info, "MCP tools:".to_string());
                    for tool in tools {
                        self.log(LogLevel::Info, format!("- {}", tool.name));
                    }
                }
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Error,
                    format!("Failed to list tools: {err:#}")
                );
            }
        }
    }

    fn call_mcp_tool(&mut self, tool: Option<&str>, args: &[&str]) {
        let Some(tool) = tool else {
            log_src!(
                self,
                LogLevel::Warn,
                "Usage: /mcp call <tool> <json>".to_string()
            );
            return;
        };

        let tool_cache = match self.mcp_connection.as_ref() {
            Some(connection) => connection.tool_cache.clone(),
            None => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    "No active MCP connection.".to_string()
                );
                return;
            }
        };

        if !tool_cache.is_empty() && !tool_cache.iter().any(|t| t.name == tool) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Tool '{tool}' not in cached list; attempting anyway.")
            );
        }

        let arg_value = if args.is_empty() {
            json!({})
        } else {
            let raw = args.join(" ");
            match serde_json::from_str::<Value>(&raw) {
                Ok(value) => value,
                Err(err) => {
                    log_src!(self, LogLevel::Error, format!("Invalid JSON args: {err}"));
                    return;
                }
            }
        };

        match self.call_mcp_tool_value(tool, arg_value) {
            Ok(value) => {
                let rendered = format_json(value);
                self.log(LogLevel::Info, format!("Tool {tool} result:"));
                self.log(LogLevel::Info, rendered);
            }
            Err(err) => {
                log_src!(self, LogLevel::Error, format!("Tool call failed: {err:#}"));
            }
        }
    }

    /// Invoke a single MCP tool and return its raw result.
    pub(crate) fn call_mcp_tool_value(&mut self, tool: &str, arg_value: Value) -> Result<Value> {
        let connection = self
            .mcp_connection
            .as_ref()
            .ok_or_else(|| anyhow!("No active MCP connection"))?;
        let result = self
            .runtime
            .block_on(mcp::call_tool(connection, tool, arg_value))?;
        Ok(result)
    }

    fn show_mcp_status(&mut self) {
        if let Some(connection) = &self.mcp_connection {
            let tool_count = connection.tool_cache.len();
            self.log(
                LogLevel::Info,
                format!(
                    "Connected MCP: {} ({} tools)",
                    connection.server.display_name(),
                    tool_count
                ),
            );
        } else if let Some(server) = &self.active_mcp {
            self.log(
                LogLevel::Info,
                format!(
                    "Active MCP (saved): {} ({})",
                    server.display_name(),
                    server.url
                ),
            );
        } else {
            self.log(LogLevel::Info, "No active MCP.".to_string());
        }
    }

    fn authenticate_mcp(&mut self, target: &str) {
        let Some(server) = self.mcp_config.find_by_id_or_name(target) else {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Unknown MCP server: {target}")
            );
            return;
        };

        let Some(auth) = &server.auth else {
            log_src!(
                self,
                LogLevel::Warn,
                "No auth config for server.".to_string()
            );
            return;
        };

        if auth.auth_type != "oauth_browser" {
            log_src!(
                self,
                LogLevel::Warn,
                "Auth flow only supports oauth_browser.".to_string()
            );
            return;
        }

        let client_id = self.resolve_mcp_client_id(&server, auth);
        let client_secret = self.resolve_mcp_client_secret(auth);
        let mut oauth_logs = Vec::new();

        let http_client = reqwest::Client::new();

        let prepare_result = self.runtime.block_on(mcp::oauth::prepare_auth(
            &http_client,
            &server,
            auth,
            client_id,
            client_secret,
            |line| oauth_logs.push(line),
        ));

        for line in oauth_logs.drain(..) {
            self.log(LogLevel::Info, line);
        }

        let (auth_url, pending) = match prepare_result {
            Ok(result) => result,
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Error,
                    format!("OAuth preparation failed: {err:#}")
                );
                return;
            }
        };

        self.pending_oauth = Some((server.id.clone(), pending.clone()));

        self.log(
            LogLevel::Info,
            "Opening browser for authorization...".to_string(),
        );
        self.log(LogLevel::Info, format!("URL: {auth_url}"));
        if let Err(err) = open::that(&auth_url) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Could not open browser: {err}")
            );
        }

        self.log(
            LogLevel::Info,
            format!(
                "Waiting for callback on {}... If the browser redirects elsewhere, \
                copy the URL from the browser and run:\n  /mcp auth-code {} <url>",
                pending.redirect_uri, server.id
            ),
        );

        let wait_result = self.runtime.block_on(async {
            mcp::oauth::wait_for_oauth_callback(&pending, Duration::from_secs(120)).await
        });

        match wait_result {
            Ok(token) => {
                self.pending_oauth = None;
                self.store_mcp_token(&server.id, &token.access_token);
                if let Some(refresh) = &token.refresh_token {
                    self.store_mcp_refresh_token(&server.id, refresh);
                }
                if let Some(client_id) = &token.client_id {
                    self.store_mcp_client_id(&server.id, client_id, auth);
                }
                self.log(LogLevel::Info, "OAuth complete. Token stored.".to_string());
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Warn,
                    format!("Callback not received: {err:#}")
                );
                self.log(
                    LogLevel::Info,
                    format!(
                        "If the browser showed a page with ?code= in the URL, \
                        copy that full URL and run:\n  /mcp auth-code {} <url>",
                        server.id
                    ),
                );
            }
        }
    }

    fn complete_oauth_manual(&mut self, server_id: &str, raw_input: &str) {
        let Some((pending_id, pending)) = &self.pending_oauth else {
            log_src!(
                self,
                LogLevel::Warn,
                "No pending OAuth flow. Run /mcp auth <id> first.".to_string()
            );
            return;
        };

        if pending_id != server_id {
            log_src!(
                self,
                LogLevel::Warn,
                format!(
                    "Pending OAuth is for '{}', not '{server_id}'. Run /mcp auth {server_id} first.",
                    pending_id
                )
            );
            return;
        }

        let pending = pending.clone();
        let http_client = reqwest::Client::new();

        let result = self
            .runtime
            .block_on(mcp::oauth::exchange_manual_code_with_input(
                &http_client,
                &pending,
                raw_input,
            ));

        match result {
            Ok(token) => {
                self.pending_oauth = None;
                self.store_mcp_token(server_id, &token.access_token);
                if let Some(refresh) = &token.refresh_token {
                    self.store_mcp_refresh_token(server_id, refresh);
                }
                if let Some(client_id) = &token.client_id {
                    if let Some(server) = self.mcp_config.find_by_id_or_name(server_id) {
                        if let Some(auth) = &server.auth {
                            self.store_mcp_client_id(server_id, client_id, auth);
                        }
                    }
                }
                self.log(
                    LogLevel::Info,
                    "OAuth complete (manual). Token stored.".to_string(),
                );
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Error,
                    format!("Manual token exchange failed: {err:#}")
                );
            }
        }
    }

    // ── Token / credential helpers ───────────────────────────────────

    fn resolve_mcp_client_id(&mut self, server: &McpServer, auth: &McpAuth) -> Option<String> {
        if let Some(client_id) = &auth.client_id {
            return Some(client_id.clone());
        }
        if let Some(env_key) = &auth.client_id_env {
            if let Ok(value) = env::var(env_key) {
                return Some(value);
            }
        }
        if let Some(value) = self.local_mcp_store.client_ids.get(&server.id) {
            return Some(value.clone());
        }
        if auth.redirect_uri.is_none() {
            return None;
        }
        let key = format!("mcp_client_{}", server.id);
        if let Ok(Some(Value::String(value))) = self.runtime.block_on(self.rice.get_variable(&key))
        {
            return Some(value);
        }
        None
    }

    fn resolve_mcp_client_secret(&mut self, auth: &McpAuth) -> Option<String> {
        if let Some(secret) = &auth.client_secret {
            return Some(secret.clone());
        }
        if let Some(env_key) = &auth.client_secret_env {
            if let Ok(value) = env::var(env_key) {
                return Some(value);
            }
        }
        None
    }

    fn store_mcp_client_id(&mut self, id: &str, client_id: &str, auth: &McpAuth) {
        if auth.redirect_uri.is_none() {
            return;
        }
        self.local_mcp_store
            .client_ids
            .insert(id.to_string(), client_id.to_string());
        if let Err(err) = persist_local_mcp_store(&self.local_mcp_store) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to persist local client id: {err:#}")
            );
        }
        let key = format!("mcp_client_{id}");
        if let Err(err) = self.runtime.block_on(self.rice.set_variable(
            &key,
            Value::String(client_id.to_string()),
            "explicit",
        )) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to store client id: {err:#}")
            );
        }
    }

    fn store_mcp_refresh_token(&mut self, id: &str, token: &str) {
        let key = format!("mcp_refresh_{id}");
        self.local_mcp_store
            .refresh_tokens
            .insert(id.to_string(), token.to_string());
        if let Err(err) = persist_local_mcp_store(&self.local_mcp_store) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to persist local refresh token: {err:#}")
            );
        }
        if let Err(err) = self.runtime.block_on(self.rice.set_variable(
            &key,
            Value::String(token.to_string()),
            "explicit",
        )) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to store refresh token: {err:#}")
            );
        }
    }

    fn store_mcp_token(&mut self, id: &str, token: &str) {
        let key = format!("mcp_token_{id}");
        self.local_mcp_store
            .tokens
            .insert(id.to_string(), token.to_string());
        if let Err(err) = persist_local_mcp_store(&self.local_mcp_store) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to persist local token: {err:#}")
            );
        }
        if let Err(err) = self.runtime.block_on(self.rice.set_variable(
            &key,
            Value::String(token.to_string()),
            "explicit",
        )) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Stored token locally, but Rice persistence failed: {err:#}")
            );
        } else {
            self.log(
                LogLevel::Info,
                format!("Stored MCP token for {id} ({}).", mask_key(token)),
            );
        }
    }

    fn clear_mcp_token(&mut self, id: &str) {
        let key = format!("mcp_token_{id}");
        self.local_mcp_store.tokens.remove(id);
        if let Err(err) = persist_local_mcp_store(&self.local_mcp_store) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to update local token cache: {err:#}")
            );
        }
        if let Err(err) = self.runtime.block_on(self.rice.delete_variable(&key)) {
            log_src!(
                self,
                LogLevel::Error,
                format!("Failed to delete token: {err:#}")
            );
            return;
        }
        self.log(LogLevel::Info, format!("Cleared MCP token for {id}."));
    }

    fn resolve_mcp_token(&mut self, server: &McpServer) -> Option<String> {
        if let Some(token) = self.local_mcp_store.tokens.get(&server.id) {
            return Some(token.clone());
        }
        let key = format!("mcp_token_{}", server.id);
        if let Ok(Some(Value::String(token))) = self.runtime.block_on(self.rice.get_variable(&key))
        {
            return Some(token);
        }

        if let Some(auth) = &server.auth {
            if let Some(token) = &auth.bearer_token {
                return Some(token.clone());
            }
            if let Some(env_key) = &auth.bearer_env {
                if let Ok(token) = env::var(env_key) {
                    return Some(token);
                }
            }
        }

        None
    }

    fn reload_mcp_config(&mut self) {
        match McpConfig::load() {
            Ok((config, source)) => {
                self.mcp_config = config;
                self.mcp_source = source;
                self.log(
                    LogLevel::Info,
                    format!(
                        "Reloaded MCP config from {} ({} servers).",
                        self.mcp_source.label(),
                        self.mcp_config.servers.len()
                    ),
                );
            }
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Error,
                    format!("Failed to reload MCP config: {err:#}")
                );
            }
        }
    }
}

// ── OpenAI key management ────────────────────────────────────────────

impl App {
    fn handle_openai_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.show_openai_status();
            return;
        }

        match args[0] {
            "set" | "key" => {
                if let Some(key) = args.get(1) {
                    self.persist_openai_key(key);
                } else {
                    log_src!(self, LogLevel::Warn, "Usage: /openai set <key>".to_string());
                }
            }
            "clear" => self.clear_openai_key(),
            "import-env" => self.import_openai_env(),
            other => log_src!(
                self,
                LogLevel::Warn,
                format!("Unknown /openai command: {other}")
            ),
        }
    }

    fn handle_key_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.show_openai_status();
            return;
        }
        self.persist_openai_key(args[0]);
    }

    fn show_openai_status(&mut self) {
        match &self.openai_key_hint {
            Some(hint) => self.log(LogLevel::Info, format!("OpenAI key stored ({hint}).")),
            None => self.log(LogLevel::Info, "OpenAI key not set.".to_string()),
        }
    }

    /// Store an OpenAI key in Rice and update local state.
    pub(crate) fn persist_openai_key(&mut self, key: &str) {
        if let Err(err) = self.runtime.block_on(self.rice.set_variable(
            OPENAI_KEY_VAR,
            Value::String(key.to_string()),
            "explicit",
        )) {
            log_src!(
                self,
                LogLevel::Error,
                format!("Failed to store OpenAI key: {err:#}")
            );
            return;
        }

        self.openai_key = Some(key.to_string());
        self.openai_key_hint = Some(mask_key(key));
        self.log(LogLevel::Info, "OpenAI key stored in Rice.".to_string());
    }

    fn clear_openai_key(&mut self) {
        if let Err(err) = self
            .runtime
            .block_on(self.rice.delete_variable(OPENAI_KEY_VAR))
        {
            log_src!(
                self,
                LogLevel::Error,
                format!("Failed to delete key: {err:#}")
            );
            return;
        }

        self.openai_key = None;
        self.openai_key_hint = None;
        self.log(LogLevel::Info, "OpenAI key removed.".to_string());
    }

    fn import_openai_env(&mut self) {
        match env::var("OPENAI_API_KEY") {
            Ok(key) => self.persist_openai_key(&key),
            Err(_) => log_src!(self, LogLevel::Warn, "OPENAI_API_KEY not set.".to_string()),
        }
    }
}

// ── Rice status ──────────────────────────────────────────────────────

impl App {
    fn handle_rice_command(&mut self) {
        match &self.rice.status {
            RiceStatus::Connected => {
                self.log(LogLevel::Info, "Rice is connected.".to_string());
            }
            RiceStatus::Disabled(reason) => {
                log_src!(self, LogLevel::Warn, format!("Rice disabled: {reason}"));
                self.log(
                    LogLevel::Info,
                    "Set STATE_INSTANCE_URL/STATE_AUTH_TOKEN in .env to enable Rice State."
                        .to_string(),
                );
            }
        };
    }
}

// ── Bootstrap loaders ────────────────────────────────────────────────

impl App {
    /// Load the persisted OpenAI key from Rice (or fall back to env).
    pub(crate) fn load_openai_from_rice(&mut self) -> Result<()> {
        let value = self
            .runtime
            .block_on(self.rice.get_variable(OPENAI_KEY_VAR))?;
        if let Some(Value::String(key)) = value {
            self.openai_key = Some(key.clone());
            self.openai_key_hint = Some(mask_key(&key));
            return Ok(());
        }

        if let Ok(key) = env::var("OPENAI_API_KEY") {
            self.log(
                LogLevel::Info,
                "OPENAI_API_KEY found in env; storing in Rice.".to_string(),
            );
            self.persist_openai_key(&key);
        }

        Ok(())
    }

    /// Restore the last-used MCP server from Rice.
    pub(crate) fn load_active_mcp_from_rice(&mut self) -> Result<()> {
        let value = self
            .runtime
            .block_on(self.rice.get_variable(ACTIVE_MCP_VAR))?;
        if let Some(value) = value {
            let server: McpServer =
                serde_json::from_value(value).context("decode active MCP from Rice")?;
            self.active_mcp = Some(server);
        }
        Ok(())
    }
}

// ── Agent commands ────────────────────────────────────────────────────

impl App {
    fn handle_agent_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.list_agents();
            return;
        }
        match args[0] {
            "use" | "switch" => {
                if let Some(name) = args.get(1) {
                    self.switch_agent(name);
                } else {
                    log_src!(self, LogLevel::Warn, "Usage: /agent use <name>".to_string());
                }
            }
            "create" | "new" => {
                if args.len() >= 3 {
                    let name = args[1];
                    let description = args[2..].join(" ");
                    self.create_agent(name, &description);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /agent create <name> <description>".to_string()
                    );
                }
            }
            "delete" | "remove" => {
                if let Some(name) = args.get(1) {
                    self.delete_agent(name);
                } else {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        "Usage: /agent delete <name>".to_string()
                    );
                }
            }
            "info" => self.show_agent_info(),
            _ => self.list_agents(),
        }
    }

    fn list_agents(&mut self) {
        self.log(LogLevel::Info, "Available agents:".to_string());
        let default = Agent::default();
        let marker = if self.active_agent.name == "memini" {
            " \u{2B50}"
        } else {
            ""
        };
        self.log(
            LogLevel::Info,
            format!(
                "  \u{1F916} {} \u{2014} {}{marker}",
                default.name, default.description
            ),
        );
        let agents = self.custom_agents.clone();
        let active_name = self.active_agent.name.clone();
        for agent in &agents {
            let marker = if agent.name == active_name {
                " \u{2B50}"
            } else {
                ""
            };
            self.log(
                LogLevel::Info,
                format!(
                    "  \u{1F916} {} \u{2014} {}{marker}",
                    agent.name, agent.description
                ),
            );
        }
    }

    fn switch_agent(&mut self, name: &str) {
        let agent = if name == "memini" {
            Agent::default()
        } else if let Some(a) = self.custom_agents.iter().find(|a| a.name == name).cloned() {
            a
        } else {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Unknown agent: {name}. Use /agent to see available agents.")
            );
            return;
        };

        // Clear conversation thread when switching agents.
        self.conversation_thread.clear();
        if let Err(err) = self.runtime.block_on(self.rice.clear_thread()) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Thread clear failed: {err:#}")
            );
        }

        self.active_agent = agent.clone();
        if let Err(err) = self
            .runtime
            .block_on(self.rice.save_active_agent_name(&agent.name))
        {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to persist agent: {err:#}")
            );
        }

        self.log(
            LogLevel::Info,
            format!(
                "\u{1F916} Switched to agent: {} \u{2014} {}",
                agent.name, agent.description
            ),
        );
    }

    fn create_agent(&mut self, name: &str, description: &str) {
        if name == "memini" {
            log_src!(
                self,
                LogLevel::Warn,
                "Cannot override the built-in 'memini' agent.".to_string()
            );
            return;
        }
        if self.custom_agents.iter().any(|a| a.name == name) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Agent '{name}' already exists. Delete it first.")
            );
            return;
        }

        let persona = format!(
            "You are {name}, a specialized AI assistant. {description} \
             You have access to long-term memory and remember past conversations. \
             Be concise but thorough."
        );
        let agent = Agent {
            name: name.to_string(),
            description: description.to_string(),
            persona,
        };
        self.custom_agents.push(agent);

        let agents_json =
            serde_json::to_value(&self.custom_agents).unwrap_or(serde_json::Value::Array(vec![]));
        if let Err(err) = self
            .runtime
            .block_on(self.rice.save_custom_agents(agents_json))
        {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to save agents: {err:#}")
            );
        }

        self.log(
            LogLevel::Info,
            format!("\u{2728} Agent '{name}' created! Use /agent use {name} to switch."),
        );
    }

    fn delete_agent(&mut self, name: &str) {
        if name == "memini" {
            log_src!(
                self,
                LogLevel::Warn,
                "Cannot delete the built-in 'memini' agent.".to_string()
            );
            return;
        }

        let before = self.custom_agents.len();
        self.custom_agents.retain(|a| a.name != name);
        if self.custom_agents.len() == before {
            log_src!(self, LogLevel::Warn, format!("Agent '{name}' not found."));
            return;
        }

        // If deleting the active agent, switch back to default.
        if self.active_agent.name == name {
            self.active_agent = Agent::default();
            self.conversation_thread.clear();
            let _ = self.runtime.block_on(self.rice.clear_thread());
            let _ = self
                .runtime
                .block_on(self.rice.save_active_agent_name("memini"));
        }

        let agents_json =
            serde_json::to_value(&self.custom_agents).unwrap_or(serde_json::Value::Array(vec![]));
        if let Err(err) = self
            .runtime
            .block_on(self.rice.save_custom_agents(agents_json))
        {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Failed to save agents: {err:#}")
            );
        }

        self.log(
            LogLevel::Info,
            format!("\u{1F5D1}\u{FE0F} Agent '{name}' deleted."),
        );
    }

    fn show_agent_info(&mut self) {
        let name = self.active_agent.name.clone();
        let description = self.active_agent.description.clone();
        let thread_len = self.conversation_thread.len();
        self.log(LogLevel::Info, format!("\u{1F916} Active agent: {name}"));
        self.log(LogLevel::Info, format!("   Description: {description}"));
        self.log(LogLevel::Info, format!("   Thread: {thread_len} messages"));
    }
}

// ── Thread commands ───────────────────────────────────────────────────

impl App {
    fn handle_thread_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.show_thread_info();
            return;
        }
        match args[0] {
            "clear" | "reset" => self.clear_thread(),
            _ => self.show_thread_info(),
        }
    }

    fn show_thread_info(&mut self) {
        let count = self.conversation_thread.len();
        let turns = count / 2;
        self.log(
            LogLevel::Info,
            format!(
                "\u{1F4DD} Conversation: {count} messages ({turns} turns) | Agent: {}",
                self.active_agent.name
            ),
        );
        if count == 0 {
            self.log(
                LogLevel::Info,
                "   Thread is empty. Start chatting to build context.".to_string(),
            );
        }
    }

    fn clear_thread(&mut self) {
        self.conversation_thread.clear();
        if let Err(err) = self.runtime.block_on(self.rice.clear_thread()) {
            log_src!(
                self,
                LogLevel::Warn,
                format!("Thread clear failed: {err:#}")
            );
        }
        self.log(
            LogLevel::Info,
            "\u{1F5D1}\u{FE0F} Conversation thread cleared.".to_string(),
        );
    }
}

// ── Memory commands ───────────────────────────────────────────────────

impl App {
    fn handle_memory_command(&mut self, args: Vec<&str>) {
        if args.is_empty() {
            self.log(LogLevel::Info, "Usage: /memory <search query>".to_string());
            return;
        }
        let query = args.join(" ");
        self.search_memory(&query);
    }

    fn search_memory(&mut self, query: &str) {
        let memories =
            match self
                .runtime
                .block_on(self.rice.reminisce(vec![], self.memory_limit, query))
            {
                Ok(traces) => traces,
                Err(err) => {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        format!("Memory search failed: {err:#}")
                    );
                    return;
                }
            };

        if memories.is_empty() {
            self.log(LogLevel::Info, "No matching memories found.".to_string());
            return;
        }

        self.log(
            LogLevel::Info,
            format!("\u{1F9E0} Found {} memory(ies):", memories.len()),
        );
        for trace in &memories {
            let input = trace.input.trim();
            let outcome = trace.outcome.trim();
            let action = trace.action.trim();
            if input.is_empty() && outcome.is_empty() {
                continue;
            }
            if action.is_empty() {
                self.log(
                    LogLevel::Info,
                    format!("  \u{21B3} {input} \u{2192} {outcome}"),
                );
            } else {
                self.log(
                    LogLevel::Info,
                    format!("  \u{21B3} [{action}] {input} \u{2192} {outcome}"),
                );
            }
        }
    }
}

// ── Daemon commands ──────────────────────────────────────────────────

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
        let builtins = super::daemon::builtin_tasks();

        if self.daemon_handles.is_empty() && builtins.is_empty() {
            self.log(LogLevel::Info, "No daemon tasks configured.".to_string());
            return;
        }

        self.log(LogLevel::Info, "Background daemon tasks:".to_string());

        // Show built-in tasks (even if not running).
        for builtin in &builtins {
            let running = self
                .daemon_handles
                .iter()
                .any(|h| h.def.name == builtin.name);
            let status = if running {
                "🟢 running"
            } else {
                "⚪ available"
            };
            self.log(
                LogLevel::Info,
                format!(
                    "  ⚡ {} — {} [{}s interval, {}]",
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
                format!("  ⚡ {name} — {prompt} [{interval}s interval, 🟢 running]"),
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
                    format!("🚀 Woke daemon '{name}' for immediate run."),
                );
                return;
            }
        }

        // Otherwise, find it in builtins and run as one-shot.
        let builtins = super::daemon::builtin_tasks();
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
        let builtins = super::daemon::builtin_tasks();
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
            self.log(LogLevel::Info, format!("🛑 Daemon '{name}' stopped."));
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
        let builtins = super::daemon::builtin_tasks();
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

        let def = super::daemon::DaemonTaskDef {
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
            format!("✨ Custom daemon '{name}' created and started (every {interval}s)."),
        );
    }

    fn remove_daemon_task(&mut self, name: &str) {
        // Stop if running.
        if let Some(pos) = self.daemon_handles.iter().position(|h| h.def.name == name) {
            let handle = self.daemon_handles.remove(pos);
            handle.abort.abort();
        }

        self.log(LogLevel::Info, format!("🗑️ Daemon '{name}' removed."));
    }

    fn show_daemon_results(&mut self, filter: Option<&str>) {
        let results: Vec<_> = self
            .daemon_results
            .iter()
            .filter(|r| filter.is_none() || Some(r.task_name.as_str()) == filter)
            .map(|r| (r.timestamp.clone(), r.task_name.clone(), r.message.clone()))
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
            self.log(LogLevel::Info, format!("  [{ts}] 🤖 {name} — {msg}"));
        }
    }
}
