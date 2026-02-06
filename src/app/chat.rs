//! AI chat flow — memory recall, OpenAI tool loops, and Rice trace commits.

use std::env;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::constants::OPENAI_KEY_VAR;
use crate::mcp;
use crate::openai::{
    extract_output_items, extract_output_text, extract_tool_calls, tool_loop_limit_reached,
};
use crate::rice::{agent_id, format_memories, system_prompt};

use super::App;
use super::log_src;
use super::logging::{LogLevel, mask_key};

impl App {
    /// Run a full chat turn: embed → recall → LLM → tool loops → commit.
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

        if require_mcp && self.mcp_connection.is_none() {
            log_src!(
                self,
                LogLevel::Warn,
                "No active MCP connection.".to_string()
            );
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

        // Build LLM input.
        let memory_context = format_memories(&memories);
        let mut input = Vec::new();
        input.push(json!({"role": "system", "content": system_prompt(require_mcp)}));
        if !memory_context.is_empty() {
            input.push(json!({"role": "system", "content": memory_context}));
        }
        input.push(json!({"role": "user", "content": message}));

        let tools = match self.openai_tools_for_mcp(require_mcp) {
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
                let tool_output = match self.call_mcp_tool_value(&call.name, call.arguments) {
                    Ok(value) => serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
                    Err(err) => format!("{{\"error\":\"{err}\"}}"),
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
            log_src!(
                self,
                LogLevel::Warn,
                "OpenAI returned no text output.".to_string()
            );
        } else {
            self.log(LogLevel::Info, format!("Assistant: {output_text}"));
        }

        // Commit trace to Rice (Rice computes embeddings server-side).
        if let Err(err) = self.runtime.block_on(self.rice.commit_trace(
            message,
            &output_text,
            "chat",
            vec![],
            agent_id(),
        )) {
            log_src!(self, LogLevel::Warn, format!("Rice commit failed: {err:#}"));
        }
    }

    /// Build the OpenAI-compatible tool definitions from the active MCP connection.
    fn openai_tools_for_mcp(&mut self, require_mcp: bool) -> Result<Option<Vec<Value>>> {
        if self.mcp_connection.is_none() {
            if require_mcp {
                return Err(anyhow!("No active MCP connection"));
            }
            return Ok(None);
        }

        let cache_empty = self
            .mcp_connection
            .as_ref()
            .map(|conn| conn.tool_cache.is_empty())
            .unwrap_or(true);
        if cache_empty {
            self.list_mcp_tools();
        }

        let tools = self
            .mcp_connection
            .as_ref()
            .map(|conn| conn.tool_cache.clone())
            .unwrap_or_default();

        if tools.is_empty() {
            if require_mcp {
                return Err(anyhow!("MCP connected but no tools available"));
            }
            return Ok(None);
        }

        let openai_tools = mcp::tools_to_openai(&tools)?;
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
}
