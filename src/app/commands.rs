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
            "(no slash)            Chat with OpenAI",
            "/mcp                   List MCP servers",
            "/mcp connect <id>      Set active MCP",
            "/mcp auth <id>         Run OAuth flow",
            "/mcp auth-code <id> <url-or-code>  Complete OAuth manually",
            "/mcp status            Show active MCP",
            "/mcp tools             List tools on active MCP",
            "/mcp call <tool> <json>Call MCP tool with JSON args",
            "/mcp ask <prompt>      Chat using MCP tools",
            "/mcp disconnect        Close MCP connection",
            "/mcp token <id> <tok>  Store bearer token in Rice",
            "/mcp token-clear <id>  Remove stored bearer token",
            "/mcp reload            Reload MCP config",
            "/openai                Show OpenAI key status",
            "/openai set <key>      Persist OpenAI key in Rice",
            "/openai key <key>      Alias for set",
            "/key <key>             Set OpenAI key",
            "/openai clear          Remove OpenAI key",
            "/openai import-env     Store $OPENAI_API_KEY",
            "/rice                  Show Rice connection status",
            "/clear                 Clear activity log",
            "/quit                  Exit",
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
