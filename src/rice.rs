//! Rice SDK integration — state variables, memory traces, and focus.

use std::env;

use anyhow::{Context, Result, anyhow};
use rice_sdk::Client;
use rice_sdk::rice_core::config::{RiceConfig, StateConfig, StorageConfig};
use rice_sdk::rice_state::proto::Trace;
use serde_json::Value;

use crate::constants::{
    ACTIVE_AGENT_VAR, APP_NAME, CONVERSATION_THREAD_VAR, CUSTOM_AGENTS_VAR, DEFAULT_RUN_ID,
    SHARED_WORKSPACE_VAR,
};
use crate::util::{env_first, normalize_url};

/// Persistent store backed by the Rice State gRPC service.
pub struct RiceStore {
    client: Option<Client>,
    pub status: RiceStatus,
    run_id: String,
    /// When set, all memory operations target this shared workspace
    /// instead of the personal `run_id`.
    pub shared_run_id: Option<String>,
}

/// Connection state of the Rice backend.
#[derive(Clone, Debug)]
pub enum RiceStatus {
    Connected,
    Disabled(String),
}

impl RiceStore {
    pub async fn connect() -> Self {
        let Some(config) = rice_config_from_env() else {
            return RiceStore {
                client: None,
                status: RiceStatus::Disabled("Rice env not configured".to_string()),
                run_id: rice_run_id(),
                shared_run_id: None,
            };
        };

        match Client::new(config).await {
            Ok(client) => {
                let status = if client.state.is_some() {
                    RiceStatus::Connected
                } else {
                    RiceStatus::Disabled("Rice state module not enabled".to_string())
                };
                RiceStore {
                    client: Some(client),
                    status,
                    run_id: rice_run_id(),
                    shared_run_id: None,
                }
            }
            Err(err) => RiceStore {
                client: None,
                status: RiceStatus::Disabled(format!("Client init failed: {err}")),
                run_id: rice_run_id(),
                shared_run_id: None,
            },
        }
    }

    pub fn status_label(&self) -> String {
        match &self.status {
            RiceStatus::Connected => "connected".to_string(),
            RiceStatus::Disabled(reason) => format!("off ({reason})"),
        }
    }

    /// The run-id currently in effect.  Returns the shared workspace id
    /// when the user has joined one, otherwise their personal id.
    pub fn active_run_id(&self) -> String {
        self.shared_run_id
            .clone()
            .unwrap_or_else(|| self.run_id.clone())
    }

    /// Switch to a shared workspace.  All subsequent memory operations
    /// (focus, recall, commit) will target this workspace so that every
    /// user on the same Rice endpoint who joins the same name shares
    /// the same memory pool.
    pub fn join_workspace(&mut self, name: &str) {
        self.shared_run_id = Some(name.to_string());
    }

    /// Return to the personal (private) workspace.
    pub fn leave_workspace(&mut self) {
        self.shared_run_id = None;
    }

    /// Persist the current shared workspace name into Rice (personal
    /// scope) so it can be restored on next launch.
    pub async fn save_shared_workspace(&mut self) -> Result<()> {
        match &self.shared_run_id {
            Some(name) => {
                // Save to personal scope (use the real personal run_id).
                let client = self
                    .client
                    .as_mut()
                    .ok_or_else(|| anyhow!("Rice not connected"))?;
                let state = client
                    .state
                    .as_mut()
                    .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
                let value_json =
                    serde_json::to_string(&Value::String(name.clone())).context("serialize")?;
                state
                    .set_variable(
                        self.run_id.clone(),
                        SHARED_WORKSPACE_VAR.to_string(),
                        value_json,
                        "share".to_string(),
                    )
                    .await
                    .context("save shared workspace")?;
            }
            None => {
                let client = self
                    .client
                    .as_mut()
                    .ok_or_else(|| anyhow!("Rice not connected"))?;
                let state = client
                    .state
                    .as_mut()
                    .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
                state
                    .delete_variable(
                        self.run_id.clone(),
                        SHARED_WORKSPACE_VAR.to_string(),
                    )
                    .await
                    .context("clear shared workspace")?;
            }
        }
        Ok(())
    }

    /// Load a previously-saved shared workspace from Rice.
    pub async fn load_shared_workspace(&mut self) -> Result<Option<String>> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        let variable = state
            .get_variable(
                self.run_id.clone(),
                SHARED_WORKSPACE_VAR.to_string(),
            )
            .await
            .context("load shared workspace")?;
        if variable.value_json.trim().is_empty() {
            return Ok(None);
        }
        match serde_json::from_str::<Value>(&variable.value_json) {
            Ok(Value::String(name)) => Ok(Some(name)),
            _ => Ok(None),
        }
    }

    pub async fn set_variable(&mut self, name: &str, value: Value, source: &str) -> Result<()> {
        let rid = self.active_run_id();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        let value_json = serde_json::to_string(&value).context("serialize value")?;
        state
            .set_variable(
                rid,
                name.to_string(),
                value_json,
                source.to_string(),
            )
            .await
            .context("set variable")?;
        Ok(())
    }

    pub async fn get_variable(&mut self, name: &str) -> Result<Option<Value>> {
        let rid = self.active_run_id();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        let variable = state
            .get_variable(rid, name.to_string())
            .await
            .context("get variable")?;
        if variable.value_json.trim().is_empty() {
            return Ok(None);
        }
        let value =
            serde_json::from_str::<Value>(&variable.value_json).context("parse value_json")?;
        Ok(Some(value))
    }

    pub async fn delete_variable(&mut self, name: &str) -> Result<()> {
        let rid = self.active_run_id();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        state
            .delete_variable(rid, name.to_string())
            .await
            .context("delete variable")?;
        Ok(())
    }

    pub async fn focus(&mut self, content: &str) -> Result<()> {
        let rid = self.active_run_id();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        state
            .focus(content.to_string(), rid)
            .await
            .context("focus")?;
        Ok(())
    }

    pub async fn reminisce(
        &mut self,
        embedding: Vec<f32>,
        limit: u64,
        query_text: &str,
    ) -> Result<Vec<Trace>> {
        let rid = self.active_run_id();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        let response = state
            .reminisce(
                embedding,
                limit,
                query_text.to_string(),
                rid,
            )
            .await
            .context("reminisce")?;
        Ok(response.traces)
    }

    pub async fn commit_trace(
        &mut self,
        input: &str,
        outcome: &str,
        action: &str,
        embedding: Vec<f32>,
        agent_id: &str,
    ) -> Result<()> {
        let rid = self.active_run_id();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| anyhow!("Rice not connected"))?;
        let state = client
            .state
            .as_mut()
            .ok_or_else(|| anyhow!("Rice state module not enabled"))?;
        let trace = Trace {
            input: input.to_string(),
            reasoning: String::new(),
            action: action.to_string(),
            outcome: outcome.to_string(),
            agent_id: agent_id.to_string(),
            embedding,
            run_id: rid,
        };
        state.commit(trace).await.context("commit trace")?;
        Ok(())
    }

    // ── Conversation thread ──────────────────────────────────────────

    pub async fn save_thread(&mut self, messages: &[Value]) -> Result<()> {
        self.set_variable(
            CONVERSATION_THREAD_VAR,
            Value::Array(messages.to_vec()),
            "chat",
        )
        .await
    }

    pub async fn load_thread(&mut self) -> Result<Vec<Value>> {
        match self.get_variable(CONVERSATION_THREAD_VAR).await? {
            Some(Value::Array(messages)) => Ok(messages),
            _ => Ok(Vec::new()),
        }
    }

    pub async fn clear_thread(&mut self) -> Result<()> {
        self.delete_variable(CONVERSATION_THREAD_VAR).await
    }

    // ── Agent persistence ────────────────────────────────────────────

    pub async fn save_custom_agents(&mut self, agents_json: Value) -> Result<()> {
        self.set_variable(CUSTOM_AGENTS_VAR, agents_json, "agent").await
    }

    pub async fn load_custom_agents(&mut self) -> Result<Option<Value>> {
        self.get_variable(CUSTOM_AGENTS_VAR).await
    }

    pub async fn save_active_agent_name(&mut self, name: &str) -> Result<()> {
        self.set_variable(
            ACTIVE_AGENT_VAR,
            Value::String(name.to_string()),
            "agent",
        )
        .await
    }

    pub async fn load_active_agent_name(&mut self) -> Result<Option<String>> {
        match self.get_variable(ACTIVE_AGENT_VAR).await? {
            Some(Value::String(name)) => Ok(Some(name)),
            _ => Ok(None),
        }
    }
}

fn rice_run_id() -> String {
    env::var("MEMINI_RUN_ID").unwrap_or_else(|_| DEFAULT_RUN_ID.to_string())
}

fn rice_config_from_env() -> Option<RiceConfig> {
    let state = env_first(&["RICE_STATE_URL", "STATE_INSTANCE_URL"]).map(|url| StateConfig {
        enabled: true,
        base_url: Some(normalize_url(&url)),
        auth_token: env_first(&["RICE_STATE_TOKEN", "STATE_AUTH_TOKEN"]),
        llm_mode: None,
        flux: None,
    });

    let storage =
        env_first(&["RICE_STORAGE_URL", "STORAGE_INSTANCE_URL"]).map(|url| StorageConfig {
            enabled: true,
            base_url: Some(normalize_url(&url)),
            auth_token: env_first(&["RICE_STORAGE_TOKEN", "STORAGE_AUTH_TOKEN"]),
            username: None,
            password: None,
        });

    if state.is_none() && storage.is_none() {
        return None;
    }

    Some(RiceConfig { state, storage })
}

pub fn format_memories(traces: &[Trace]) -> String {
    if traces.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    lines.push("Relevant memory from Rice:".to_string());
    for trace in traces {
        let input = trace.input.trim();
        let outcome = trace.outcome.trim();
        if input.is_empty() && outcome.is_empty() {
            continue;
        }
        let action = trace.action.trim();
        if action.is_empty() {
            lines.push(format!("- input: {input} | outcome: {outcome}"));
        } else {
            lines.push(format!(
                "- input: {input} | action: {action} | outcome: {outcome}"
            ));
        }
    }
    lines.join("\n")
}

pub fn system_prompt(persona: &str, require_mcp: bool) -> String {
    let now = chrono::Local::now().format("%A, %B %e, %Y at %H:%M");
    if require_mcp {
        format!(
            "{persona} The current date and time is {now}. \
             Use available tools when needed to answer the user's request. Summarize results clearly."
        )
    } else {
        format!(
            "{persona} The current date and time is {now}. \
             Use any provided memory context when helpful and answer clearly."
        )
    }
}

pub fn agent_id_for(agent_name: &str) -> String {
    format!("{APP_NAME}:{agent_name}")
}
