//! AI chat flow — non-blocking launcher for the background chat task.

use std::env;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::constants::OPENAI_KEY_VAR;
use crate::mcp;

use super::App;
use super::daemon;
use super::log_src;
use super::logging::{LogLevel, mask_key};

impl App {
    /// Launch a non-blocking chat turn.
    ///
    /// All heavy work (memory recall → LLM → tool loops → Rice commit)
    /// runs on a background tokio task via `daemon::spawn_chat_task`.
    /// The function returns immediately so the TUI draw loop keeps running.
    pub(crate) fn handle_chat_message(&mut self, message: &str, _require_mcp: bool) {
        let key = match self.ensure_openai_key() {
            Ok(k) => k,
            Err(err) => {
                log_src!(self, LogLevel::Error, format!("OpenAI key missing: {err}"));
                self.log(
                    LogLevel::Info,
                    "Use /openai set <key> or /key <key> to configure.".to_string(),
                );
                return;
            }
        };

        // Snapshot everything the background task needs (all Clone / Send).
        let mcp_snapshots = self.build_mcp_snapshots(None);
        let builtin_tools = Self::builtin_tool_defs();

        let params = daemon::ChatTaskParams {
            key,
            message: message.to_string(),
            persona: self.active_agent.persona.clone(),
            agent_name: self.active_agent.name.clone(),
            skill_context: self.skills_prompt_context(message),
            memory_limit: self.memory_limit,
            conversation_thread: self.conversation_thread.clone(),
            mcp_snapshots,
            builtin_tools,
            next_window_id: self.next_window_id.clone(),
        };

        let tx = self.daemon_tx.clone();
        let openai = self.openai.clone();
        let rice_handle = self.runtime.spawn(crate::rice::RiceStore::connect());
        let rt = self.runtime.handle().clone();

        daemon::spawn_chat_task(params, tx, openai, rice_handle, rt);
    }

    /// Built-in tool definitions injected into every chat request.
    fn builtin_tool_defs() -> Vec<Value> {
        let spawn_tool = json!({
            "type": "function",
            "name": "spawn_agent",
            "description": "Spawn an independent execution agent in its own grid window. Each agent gets its own memory context, workspace tools (file read/write + shell command), and full tool loop; MCP tools are added when available. Use this to run sub-tasks in parallel. For code/document tasks, instruct the worker to create or edit files directly and run commands for verification. Pass a coordination_key so you can later collect results with collect_results.",
            "parameters": {
                "type": "object",
                "properties": {
                    "label": {
                        "type": "string",
                        "description": "Short name for the agent window (e.g. 'Research', 'Code Review', 'Summarizer')"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Detailed, execution-oriented instructions for the sub-agent. Include exact deliverables, required tools, expected output format, and target file paths when relevant."
                    },
                    "mcp_server": {
                        "type": "string",
                        "description": "Optional. The MCP server id to give this agent access to. If omitted, the agent gets access to ALL connected MCP servers."
                    },
                    "coordination_key": {
                        "type": "string",
                        "description": "A shared key to group parallel agents. Use the same key for agents whose results you want to collect together via collect_results."
                    }
                },
                "required": ["label", "prompt"]
            }
        });
        let collect_tool = json!({
            "type": "function",
            "name": "collect_results",
            "description": "Collect results from previously spawned agents that share a coordination_key. Returns all finished agent outputs stored in Rice state. Use this after spawning parallel agents to gather and synthesize their results.",
            "parameters": {
                "type": "object",
                "properties": {
                    "coordination_key": {
                        "type": "string",
                        "description": "The coordination key that was passed to spawn_agent."
                    }
                },
                "required": ["coordination_key"]
            }
        });
        let rice_memories_tool = json!({
            "type": "function",
            "name": "rice_memories",
            "description": "Fetch relevant memories from Rice. Use this for questions about past work, recent memory, or conversation history.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional memory search query. Use 'recent activity' when the user asks for recent memories."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Optional number of memories to return (default uses memory limit, max 50)."
                    }
                }
            }
        });
        let rice_state_get_tool = json!({
            "type": "function",
            "name": "rice_state_get",
            "description": "Read a Rice state variable by key.",
            "parameters": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "State variable key (e.g. openai_model, conversation_thread)."
                    }
                },
                "required": ["key"]
            }
        });
        vec![
            spawn_tool,
            collect_tool,
            rice_memories_tool,
            rice_state_get_tool,
        ]
    }

    /// Build [`McpServerSnapshot`]s from the currently active MCP connections,
    /// optionally filtering to a single server id.
    pub(crate) fn build_mcp_snapshots(
        &self,
        server_filter: Option<&str>,
    ) -> Vec<daemon::McpServerSnapshot> {
        let mut snapshots = Vec::new();
        for (id, conn) in &self.mcp_connections {
            if let Some(filter) = server_filter {
                if id != filter {
                    continue;
                }
            }
            let bearer = self.local_mcp_store.tokens.get(id).cloned().or_else(|| {
                conn.server
                    .auth
                    .as_ref()
                    .and_then(|a| a.bearer_token.clone())
                    .or_else(|| {
                        conn.server
                            .auth
                            .as_ref()
                            .and_then(|a| a.bearer_env.as_ref())
                            .and_then(|env_key| std::env::var(env_key).ok())
                    })
            });

            let openai_tools =
                mcp::tools_to_openai_namespaced(&conn.server, &conn.tool_cache).unwrap_or_default();

            snapshots.push(daemon::McpServerSnapshot {
                server: conn.server.clone(),
                bearer,
                openai_tools,
            });
        }
        snapshots
    }

    /// Ensure an OpenAI API key is available, loading from Rice or env if needed.
    fn ensure_openai_key(&mut self) -> Result<String> {
        if let Some(key) = &self.openai_key {
            return Ok(key.clone());
        }

        if let Ok(Some(Value::String(key))) = self
            .runtime
            .block_on(self.rice.get_variable(OPENAI_KEY_VAR))
        {
            self.openai_key_hint = Some(mask_key(&key));
            self.openai_key = Some(key.clone());
            return Ok(key);
        }

        if let Ok(key) = env::var("OPENAI_API_KEY") {
            self.persist_openai_key(&key);
            return Ok(key);
        }

        Err(anyhow!("OpenAI key not configured"))
    }
}
