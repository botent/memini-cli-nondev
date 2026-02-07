//! `/openai`, `/key`, and `/rice` command handlers, plus bootstrap
//! loaders that restore persisted keys on startup.

use std::env;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::constants::{ACTIVE_MCP_VAR, OPENAI_KEY_VAR};
use crate::mcp::config::McpServer;
use crate::rice::RiceStatus;

use super::super::App;
use super::super::log_src;
use super::super::logging::{LogLevel, mask_key};

// ── /openai ──────────────────────────────────────────────────────────

impl App {
    pub(crate) fn handle_openai_command(&mut self, args: Vec<&str>) {
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

    pub(crate) fn handle_key_command(&mut self, args: Vec<&str>) {
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

// ── /rice ────────────────────────────────────────────────────────────

impl App {
    pub(crate) fn handle_rice_command(&mut self, args: Vec<&str>) {
        if !args.is_empty() && args[0] == "setup" {
            self.start_rice_setup();
            return;
        }

        match &self.rice.status {
            RiceStatus::Connected => {
                self.log(LogLevel::Info, "🟢 Rice is connected.".to_string());
                self.log(
                    LogLevel::Info,
                    format!("   Run ID: {}", self.rice.active_run_id()),
                );
            }
            RiceStatus::Disabled(reason) => {
                log_src!(self, LogLevel::Warn, format!("Rice disabled: {reason}"));
                self.log(
                    LogLevel::Info,
                    "Run /rice setup to configure Rice interactively.".to_string(),
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
