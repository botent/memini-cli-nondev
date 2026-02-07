//! AI chat flow — memory recall, OpenAI tool loops, and Rice trace commits.

use std::env;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::constants::OPENAI_KEY_VAR;
use crate::mcp;
use crate::openai::{
    ToolCall, extract_output_items, extract_output_text, extract_tool_calls,
    tool_loop_limit_reached,
};
use crate::rice::{agent_id_for, format_memories, system_prompt};

use super::App;
use super::daemon;
use super::log_src;
use super::logging::{LogLevel, mask_key};

impl App {
    /// Run a full chat turn: recall → LLM → tool loops → commit → persist thread.
    pub(crate) fn handle_chat_message(&mut self, message: &str, require_mcp: bool) {
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

        if require_mcp && self.mcp_connections.is_empty() {
            log_src!(self, LogLevel::Warn, "No MCP connections.".to_string());
            return;
        }

        // Focus Rice on the current message.
        if let Err(err) = self.runtime.block_on(self.rice.focus(message)) {
            log_src!(self, LogLevel::Warn, format!("Rice focus failed: {err:#}"));
        }

        // Recall relevant memories (Rice computes embeddings server-side).
        let memories =
            match self
                .runtime
                .block_on(self.rice.reminisce(vec![], self.memory_limit, message))
            {
                Ok(traces) => traces,
                Err(err) => {
                    log_src!(self, LogLevel::Warn, format!("Rice recall failed: {err:#}"));
                    Vec::new()
                }
            };

        if !memories.is_empty() {
            self.log(
                LogLevel::Info,
                format!("Rice recalled {} related memory(ies).", memories.len()),
            );
        }

        // Build LLM input: system prompt + memories + conversation thread + new message.
        let memory_context = format_memories(&memories);
        let mut input = Vec::new();
        input.push(json!({"role": "system", "content": system_prompt(&self.active_agent.persona, require_mcp)}));
        if !memory_context.is_empty() {
            input.push(json!({"role": "system", "content": memory_context}));
        }

        // Include conversation thread (previous turns give multi-turn context).
        for msg in &self.conversation_thread {
            input.push(msg.clone());
        }

        // New user message.
        input.push(json!({"role": "user", "content": message}));

        let mut tools = match self.openai_tools_for_mcp(require_mcp) {
            Ok(t) => t,
            Err(err) => {
                log_src!(
                    self,
                    LogLevel::Error,
                    format!("Failed to load MCP tools: {err:#}")
                );
                return;
            }
        };

        // Always inject built-in orchestration tools so the LLM can
        // delegate sub-tasks to parallel agent windows and collect results.
        let spawn_tool = json!({
            "type": "function",
            "name": "spawn_agent",
            "description": "Spawn an independent sub-agent that runs in its own window in the user's grid layout. Each agent gets its own MCP connection, memory context, and full tool loop. Use this to run tasks in PARALLEL across different MCP servers. Pass a coordination_key so you can later collect results with collect_results.",
            "parameters": {
                "type": "object",
                "properties": {
                    "label": {
                        "type": "string",
                        "description": "Short name for the agent window (e.g. 'Research', 'Code Review', 'Summarizer')"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The detailed task prompt for the sub-agent. Be specific about what it should do and which tools to use."
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
        let builtin_tools = tools.get_or_insert_with(Vec::new);
        builtin_tools.push(spawn_tool);
        builtin_tools.push(collect_tool);

        // Initial LLM request.
        let mut response =
            match self
                .runtime
                .block_on(self.openai.response(&key, &input, tools.as_deref()))
            {
                Ok(r) => r,
                Err(err) => {
                    log_src!(
                        self,
                        LogLevel::Error,
                        format!("OpenAI request failed: {err:#}")
                    );
                    return;
                }
            };

        let mut output_items = extract_output_items(&response);
        if !output_items.is_empty() {
            input.extend(output_items.clone());
        }
        let mut output_text = extract_output_text(&output_items);
        let mut tool_calls = extract_tool_calls(&output_items);
        let mut tool_loops = 0usize;

        // Tool-call loop.
        while !tool_calls.is_empty() {
            if tool_loop_limit_reached(tool_loops) {
                log_src!(self, LogLevel::Warn, "Tool loop limit reached.".to_string());
                break;
            }
            tool_loops += 1;

            for call in tool_calls {
                self.log(LogLevel::Info, format!("Calling tool: {}", call.name));
                let tool_output = if call.name == "spawn_agent" {
                    self.handle_spawn_agent_tool(&call)
                } else if call.name == "collect_results" {
                    self.handle_collect_results_tool(&call)
                } else {
                    match self.call_mcp_tool_value(&call.name, call.arguments) {
                        Ok(value) => {
                            serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
                        }
                        Err(err) => format!("{{\"error\":\"{err}\"}}"),
                    }
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call.call_id,
                    "output": tool_output
                }));
            }

            response =
                match self
                    .runtime
                    .block_on(self.openai.response(&key, &input, tools.as_deref()))
                {
                    Ok(r) => r,
                    Err(err) => {
                        log_src!(
                            self,
                            LogLevel::Error,
                            format!("OpenAI request failed: {err:#}")
                        );
                        break;
                    }
                };
            output_items = extract_output_items(&response);
            if !output_items.is_empty() {
                input.extend(output_items.clone());
            }
            output_text = extract_output_text(&output_items);
            tool_calls = extract_tool_calls(&output_items);
        }

        // Display result.
        if output_text.is_empty() {
            log_src!(self, LogLevel::Warn, "No response received.".to_string());
        } else {
            let label = format!("{}", self.active_agent.name);
            self.log_markdown(label, output_text.clone());
        }

        // Update conversation thread with this turn.
        self.conversation_thread
            .push(json!({"role": "user", "content": message}));
        if !output_text.is_empty() {
            self.conversation_thread
                .push(json!({"role": "assistant", "content": output_text}));
        }

        // Trim thread if over limit.
        let max = crate::constants::MAX_THREAD_MESSAGES;
        while self.conversation_thread.len() > max {
            self.conversation_thread.drain(0..2);
        }

        // Persist thread to Rice.
        if let Err(err) = self
            .runtime
            .block_on(self.rice.save_thread(&self.conversation_thread))
        {
            log_src!(self, LogLevel::Warn, format!("Thread save failed: {err:#}"));
        }

        // Commit trace to Rice long-term memory.
        let aid = agent_id_for(&self.active_agent.name);
        if let Err(err) = self.runtime.block_on(self.rice.commit_trace(
            message,
            &output_text,
            "chat",
            vec![],
            &aid,
        )) {
            log_src!(self, LogLevel::Warn, format!("Rice commit failed: {err:#}"));
        }
    }

    /// Build the OpenAI-compatible tool definitions from the active MCP connection.
    fn openai_tools_for_mcp(&mut self, require_mcp: bool) -> Result<Option<Vec<Value>>> {
        if self.mcp_connections.is_empty() {
            if require_mcp {
                return Err(anyhow!("No active MCP connection"));
            }
            return Ok(None);
        }

        let mut tool_warnings = Vec::new();
        let server_ids: Vec<String> = self.mcp_connections.keys().cloned().collect();
        for id in &server_ids {
            let refresh_result = {
                let Some(connection) = self.mcp_connections.get_mut(id) else {
                    continue;
                };
                if connection.tool_cache.is_empty() {
                    Some(self.runtime.block_on(mcp::refresh_tools(connection)))
                } else {
                    None
                }
            };
            if let Some(Err(err)) = refresh_result {
                tool_warnings.push(format!("MCP tools refresh failed for {id}: {err:#}"));
            }
        }
        for line in tool_warnings {
            log_src!(self, LogLevel::Warn, line);
        }

        let mut openai_tools = Vec::new();
        for id in server_ids {
            let Some(connection) = self.mcp_connections.get(&id) else {
                continue;
            };
            if connection.tool_cache.is_empty() {
                continue;
            }
            openai_tools.extend(mcp::tools_to_openai_namespaced(
                &connection.server,
                &connection.tool_cache,
            )?);
        }

        if openai_tools.is_empty() {
            if require_mcp {
                return Err(anyhow!("MCP connected but no tools available"));
            }
            return Ok(None);
        }

        Ok(Some(openai_tools))
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

    /// Handle the built-in `spawn_agent` tool call from the LLM.
    /// Creates an agent window and spawns the background task with MCP
    /// tool access, returning a JSON status string to feed back to the model.
    fn handle_spawn_agent_tool(&mut self, call: &ToolCall) -> String {
        let label = call
            .arguments
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("Sub-Agent")
            .to_string();
        let prompt = call
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mcp_server_filter = call
            .arguments
            .get("mcp_server")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let coordination_key = call
            .arguments
            .get("coordination_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if prompt.is_empty() {
            return r#"{"error":"prompt is required"}"#.to_string();
        }

        let window_id = self.next_window_id;
        self.next_window_id += 1;

        // Create the window in Thinking state so it appears in the grid.
        let window = daemon::AgentWindow {
            id: window_id,
            label: label.clone(),
            prompt: prompt.clone(),
            status: daemon::AgentWindowStatus::Thinking,
            output_lines: Vec::new(),
            pending_question: None,
            scroll: 0,
        };
        self.agent_windows.push(window);

        // Select the new agent in the grid.
        let idx = self.agent_windows.len().saturating_sub(1);
        self.grid_selected = idx;

        // Build MCP snapshots from active connections so the agent can
        // open its own independent connections.
        let mcp_snapshots = self.build_mcp_snapshots(mcp_server_filter.as_deref());
        let has_mcp = !mcp_snapshots.is_empty();

        // Spawn the background task.
        let tx = self.daemon_tx.clone();
        let openai = self.openai.clone();
        let key = self.openai_key.clone();
        let rice_handle = self.runtime.spawn(crate::rice::RiceStore::connect());
        let persona = self.active_agent.persona.clone();

        if has_mcp {
            daemon::spawn_agent_window_with_mcp(
                window_id,
                coordination_key.clone(),
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

        self.log(
            LogLevel::Info,
            format!(
                "LLM spawned agent: {label} (window #{window_id}){}",
                if has_mcp { " [with MCP tools]" } else { "" }
            ),
        );

        format!(
            r#"{{"status":"spawned","window_id":{window_id},"label":"{label}","has_mcp":{has_mcp},"coordination_key":"{coordination_key}"}}"#,
        )
    }

    /// Build [`McpServerSnapshot`]s from the currently active MCP connections,
    /// optionally filtering to a single server id.
    fn build_mcp_snapshots(&self, server_filter: Option<&str>) -> Vec<daemon::McpServerSnapshot> {
        let mut snapshots = Vec::new();
        for (id, conn) in &self.mcp_connections {
            if let Some(filter) = server_filter {
                if id != filter {
                    continue;
                }
            }
            // Resolve the bearer token the same way the main connect logic does.
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

            // Pre-build OpenAI tool definitions as a fallback.
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

    /// Handle the built-in `collect_results` tool call.
    /// Reads finished agent results from Rice state variables.
    fn handle_collect_results_tool(&mut self, call: &ToolCall) -> String {
        let coordination_key = call
            .arguments
            .get("coordination_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if coordination_key.is_empty() {
            return r#"{"error":"coordination_key is required"}"#.to_string();
        }

        // Collect results from Rice state for each agent window.
        let mut results: Vec<Value> = Vec::new();
        for window in &self.agent_windows {
            let coord_var = format!("agent_result:{}:{}", coordination_key, window.id);
            match self.runtime.block_on(self.rice.get_variable(&coord_var)) {
                Ok(Some(value)) => {
                    results.push(value);
                }
                _ => {
                    // Agent hasn't finished yet or wasn't part of this group.
                    if window.status == daemon::AgentWindowStatus::Thinking {
                        results.push(json!({
                            "window_id": window.id,
                            "label": window.label,
                            "status": "still_running",
                        }));
                    }
                }
            }
        }

        let summary = json!({
            "coordination_key": coordination_key,
            "agent_count": results.len(),
            "results": results,
        });

        self.log(
            LogLevel::Info,
            format!(
                "Collected {} result(s) for coordination key '{coordination_key}'.",
                results.len()
            ),
        );

        serde_json::to_string(&summary)
            .unwrap_or_else(|_| r#"{"error":"serialize failed"}"#.to_string())
    }
}
