//! `/mcp` command handlers — connect, disconnect, auth, tools, and token
//! management for MCP (Model Context Protocol) servers.

use std::env;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::constants::ACTIVE_MCP_VAR;
use crate::mcp;
use crate::mcp::config::{McpAuth, McpConfig, McpServer};
use crate::openai::format_json;

use super::super::App;
use super::super::log_src;
use super::super::logging::{LogLevel, mask_key};
use super::super::store::persist_local_mcp_store;

// ── MCP command dispatch ─────────────────────────────────────────────

impl App {
    pub(crate) fn handle_mcp_command(&mut self, args: Vec<&str>) {
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
            "disconnect" => {
                let target = args.get(1).copied();
                self.disconnect_mcp(target);
            }
            "status" => self.show_mcp_status(),
            "tools" => {
                let target = args.get(1).copied();
                self.list_mcp_tools(target);
            }
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
}

// ── Server listing ───────────────────────────────────────────────────

impl App {
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
}

// ── Connect / disconnect ─────────────────────────────────────────────

impl App {
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

        // Already connected? Just mark it active.
        if self.mcp_connections.contains_key(&server.id) {
            self.active_mcp = Some(server.clone());
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
            self.log(
                LogLevel::Info,
                format!(
                    "Using existing MCP connection to {}.",
                    server.display_name()
                ),
            );
            self.list_mcp_tools(Some(&server.id));
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
                self.mcp_connections.insert(server.id.clone(), connection);

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

                self.list_mcp_tools(Some(&server.id));
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

    fn disconnect_mcp(&mut self, target: Option<&str>) {
        if self.mcp_connections.is_empty() {
            self.log(LogLevel::Info, "No MCP connections to close.".to_string());
            return;
        }

        if matches!(target, Some("all")) {
            let count = self.mcp_connections.len();
            self.mcp_connections.clear();
            self.log(LogLevel::Info, format!("Closed {count} MCP connection(s)."));
            return;
        }

        let resolved_id = target
            .and_then(|q| self.mcp_config.find_by_id_or_name(q))
            .map(|server| server.id)
            .or_else(|| self.active_mcp.as_ref().map(|s| s.id.clone()))
            .or_else(|| self.mcp_connections.keys().next().cloned());

        let Some(id) = resolved_id else {
            self.log(LogLevel::Info, "No MCP connection to close.".to_string());
            return;
        };

        if self.mcp_connections.remove(&id).is_some() {
            self.log(LogLevel::Info, format!("Closed MCP connection '{id}'."));
        } else {
            self.log(
                LogLevel::Info,
                format!("No active MCP connection for '{id}'."),
            );
        }
    }
}

// ── Startup auto-connect ─────────────────────────────────────────────

impl App {
    /// Auto-connect to every configured MCP server that already has usable
    /// credentials stored (or requires no auth). This runs during startup.
    pub(crate) fn autoconnect_saved_mcps(&mut self) {
        let enabled = match env::var("MEMINI_MCP_AUTOCONNECT") {
            Ok(value) => {
                let value = value.to_ascii_lowercase();
                !(value == "0" || value == "false" || value == "no" || value == "off")
            }
            Err(_) => true,
        };
        if !enabled {
            return;
        }

        if self.mcp_config.servers.is_empty() {
            return;
        }

        let mut connect_plan: Vec<(McpServer, Option<String>)> = Vec::new();
        for server in self.mcp_config.servers.clone() {
            if self.mcp_connections.contains_key(&server.id) {
                continue;
            }

            let bearer = self.resolve_mcp_token(&server);
            let has_token = bearer
                .as_ref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false);

            let should_connect = match server.auth.as_ref().map(|a| a.auth_type.as_str()) {
                Some("oauth_browser") => has_token,
                Some(_) => has_token,
                None => true,
            };

            if should_connect {
                connect_plan.push((server, bearer));
            }
        }

        if connect_plan.is_empty() {
            return;
        }

        self.log(
            LogLevel::Info,
            format!("Auto-connecting {} MCP server(s)…", connect_plan.len()),
        );

        let connect_timeout = Duration::from_secs(10);
        let tools_timeout = Duration::from_secs(10);

        for (server, bearer) in connect_plan {
            let id = server.id.clone();
            let label = server.display_name();

            let connect_result = self.runtime.block_on(async {
                tokio::time::timeout(connect_timeout, mcp::connect_http(&server, bearer.clone()))
                    .await
            });

            let connection = match connect_result {
                Ok(Ok(connection)) => connection,
                Ok(Err(err)) => {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        format!("Auto-connect failed for {label}: {err:#}")
                    );
                    continue;
                }
                Err(_) => {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        format!("Auto-connect timed out for {label}.")
                    );
                    continue;
                }
            };

            self.mcp_connections.insert(id.clone(), connection);

            let tools_result = {
                let Some(conn) = self.mcp_connections.get_mut(&id) else {
                    continue;
                };
                self.runtime.block_on(async {
                    tokio::time::timeout(tools_timeout, mcp::refresh_tools(conn)).await
                })
            };

            match tools_result {
                Ok(Ok(tools)) => {
                    self.log(
                        LogLevel::Info,
                        format!("Connected MCP: {label} ({} tools).", tools.len()),
                    );
                }
                Ok(Err(err)) => {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        format!("Connected {label}, but tool list failed: {err:#}")
                    );
                }
                Err(_) => {
                    log_src!(
                        self,
                        LogLevel::Warn,
                        format!("Connected {label}, but tool list timed out.")
                    );
                }
            }
        }

        if self.active_mcp.is_none() {
            if let Some(connection) = self.mcp_connections.values().next() {
                self.active_mcp = Some(connection.server.clone());
            }
        }
    }
}

// ── Tool listing & invocation ────────────────────────────────────────

impl App {
    /// Refresh and display the tool list from the active MCP connection.
    pub(crate) fn list_mcp_tools(&mut self, target: Option<&str>) {
        if self.mcp_connections.is_empty() {
            log_src!(
                self,
                LogLevel::Warn,
                "No active MCP connections.".to_string()
            );
            return;
        }

        let mut ids = Vec::new();
        if matches!(target, Some("all")) || (target.is_none() && self.active_mcp.is_none()) {
            ids.extend(self.mcp_connections.keys().cloned());
        } else if let Some(target) = target {
            if let Some(server) = self.mcp_config.find_by_id_or_name(target) {
                ids.push(server.id);
            } else {
                ids.push(target.to_string());
            }
        } else if let Some(active) = &self.active_mcp {
            ids.push(active.id.clone());
        }

        if ids.is_empty() {
            log_src!(self, LogLevel::Warn, "No MCP server selected.".to_string());
            return;
        }

        for id in ids {
            let Some(connection) = self.mcp_connections.get_mut(&id) else {
                log_src!(self, LogLevel::Warn, format!("Not connected: {id}"));
                continue;
            };

            let tools_result = self.runtime.block_on(mcp::refresh_tools(connection));

            match tools_result {
                Ok(tools) => {
                    if tools.is_empty() {
                        self.log(
                            LogLevel::Info,
                            format!("No tools reported by MCP server '{id}'."),
                        );
                    } else {
                        self.log(LogLevel::Info, format!("MCP tools ({id}):"));
                        for tool in tools {
                            let name = mcp::namespaced_tool_name(&id, tool.name.as_ref());
                            self.log(LogLevel::Info, format!("- {}", name));
                        }
                    }
                }
                Err(err) => {
                    log_src!(
                        self,
                        LogLevel::Error,
                        format!("Failed to list tools for {id}: {err:#}")
                    );
                }
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

        if self.mcp_connections.is_empty() {
            log_src!(
                self,
                LogLevel::Warn,
                "No active MCP connections.".to_string()
            );
            return;
        }

        let (server_id, tool_name) = match self.resolve_tool_target(tool) {
            Ok(result) => result,
            Err(err) => {
                log_src!(self, LogLevel::Error, format!("{err:#}"));
                return;
            }
        };

        let tool_cache = self
            .mcp_connections
            .get(&server_id)
            .map(|connection| connection.tool_cache.clone())
            .unwrap_or_default();

        if !tool_cache.is_empty() && !tool_cache.iter().any(|t| t.name == tool_name) {
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

        let namespaced = mcp::namespaced_tool_name(&server_id, tool_name);
        match self.call_mcp_tool_value(&namespaced, arg_value) {
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
        let (server_id, tool_name) = self.resolve_tool_target(tool)?;
        let connection = self
            .mcp_connections
            .get(&server_id)
            .ok_or_else(|| anyhow!("No MCP connection for '{server_id}'"))?;
        let result = self
            .runtime
            .block_on(mcp::call_tool(connection, tool_name, arg_value))?;
        Ok(result)
    }

    fn show_mcp_status(&mut self) {
        if !self.mcp_connections.is_empty() {
            self.log(
                LogLevel::Info,
                format!("Connected MCP servers: {}", self.mcp_connections.len()),
            );
            let mut entries: Vec<(String, String, usize)> = self
                .mcp_connections
                .values()
                .map(|conn| {
                    (
                        conn.server.display_name(),
                        conn.server.id.clone(),
                        conn.tool_cache.len(),
                    )
                })
                .collect();
            entries.sort_by(|a, b| a.1.cmp(&b.1));
            for (name, id, tool_count) in entries {
                self.log(
                    LogLevel::Info,
                    format!("- {} ({}) [{} tools]", name, id, tool_count),
                );
            }
            if let Some(server) = &self.active_mcp {
                self.log(
                    LogLevel::Info,
                    format!("Active MCP: {} ({})", server.display_name(), server.id),
                );
            }
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
}

impl App {
    fn resolve_tool_target<'a>(&self, tool: &'a str) -> Result<(String, &'a str)> {
        if let Some((server_id, tool_name)) = mcp::split_namespaced_tool_name(tool) {
            return Ok((server_id.to_string(), tool_name));
        }

        if let Some(active) = &self.active_mcp {
            return Ok((active.id.clone(), tool));
        }

        if self.mcp_connections.len() == 1 {
            if let Some((id, _)) = self.mcp_connections.iter().next() {
                return Ok((id.clone(), tool));
            }
        }

        Err(anyhow!(
            "Ambiguous tool '{tool}'. Use <server_id>{}<{tool}> (e.g. notion{}search).",
            mcp::MCP_TOOL_NAMESPACE_SEP,
            mcp::MCP_TOOL_NAMESPACE_SEP
        ))
    }
}

// ── OAuth authentication ─────────────────────────────────────────────

impl App {
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
                // Auto-connect now that we have the token.
                let server_id = server.id.clone();
                self.connect_mcp(&server_id);
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
}

// ── Token / credential helpers ───────────────────────────────────────

impl App {
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
