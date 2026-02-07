//! Autonomous agent daemon — background tasks that run on schedules,
//! plus live agent windows with streaming output and interactive input.
//!
//! Each [`DaemonTask`] wraps an agent persona, a prompt, and an interval.
//! The daemon spawns tokio tasks that loop on their schedule, call the LLM,
//! and push results back to the TUI via an [`mpsc`] channel.
//!
//! Agent windows track real-time status (thinking/done/waiting) and stream
//! output line-by-line so the user can watch the reasoning unfold.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::mcp;
use crate::mcp::config::McpServer;
use crate::openai::{self, OpenAiClient};
use crate::rice::{self, RiceStore};

// ── Public types ─────────────────────────────────────────────────────

/// The kind of event a background agent sends to the TUI.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// Agent has started work (show "thinking" spinner).
    Started { window_id: usize },
    /// A progress line (streamed partial output / status update).
    Progress { window_id: usize, line: String },
    /// Agent finished successfully.
    Finished {
        window_id: usize,
        message: String,
        timestamp: String,
    },
    /// Agent needs user input to continue.
    NeedsInput { window_id: usize, question: String },
    /// Legacy: a simple result from a periodic daemon task.
    DaemonResult {
        task_name: String,
        message: String,
        timestamp: String,
    },

    // ── Main-chat events (non-blocking chat flow) ────────────────
    /// A progress/status line for the main chat (shows in activity log).
    ChatProgress { line: String, level: ChatLogLevel },
    /// Markdown output from the main chat LLM.
    ChatMarkdown { label: String, body: String },
    /// The main chat turn finished — update thread + commit to Rice.
    #[allow(dead_code)]
    ChatFinished {
        user_message: String,
        output_text: String,
        agent_name: String,
        thread_entries: Vec<Value>,
    },
    /// The LLM wants to spawn a sub-agent (from the background chat task).
    ChatSpawnAgent {
        window_id: usize,
        label: String,
        prompt: String,
        mcp_snapshots: Vec<McpServerSnapshot>,
        coordination_key: String,
        persona: String,
    },
}

/// Log level for ChatProgress events.
#[derive(Clone, Debug)]
pub enum ChatLogLevel {
    Info,
    Warn,
    Error,
}

/// Live state of an agent window in the side panel.
#[derive(Clone, Debug)]
pub struct AgentWindow {
    /// Unique id (1-based, displayed as the keyboard shortcut).
    pub id: usize,
    /// Short label for the window header.
    pub label: String,
    /// The user's original prompt.
    pub prompt: String,
    /// Current status.
    pub status: AgentWindowStatus,
    /// Accumulated output lines (streamed in real time).
    pub output_lines: Vec<String>,
    /// If the agent asked for input, what it asked.
    pub pending_question: Option<String>,
    /// Scroll offset within this window (for long output).
    #[allow(dead_code)]
    pub scroll: u16,
}

/// Status of an agent window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentWindowStatus {
    /// Agent is working (LLM call in flight).
    Thinking,
    /// Agent finished.
    Done,
    /// Agent needs user input.
    WaitingForInput,
}

/// Persisted definition of a daemon task (stored in Rice).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonTaskDef {
    pub name: String,
    pub persona: String,
    pub prompt: String,
    pub interval_secs: u64,
    pub paused: bool,
}

/// Runtime handle for a running daemon task.
pub struct DaemonHandle {
    pub def: DaemonTaskDef,
    /// Notify to wake a sleeping task for immediate execution.
    pub wake: Arc<Notify>,
    /// Abort handle for the spawned tokio task.
    pub abort: tokio::task::AbortHandle,
}

// ── Built-in task definitions ────────────────────────────────────────

/// Return the set of built-in daemon tasks that ship with Memini.
pub fn builtin_tasks() -> Vec<DaemonTaskDef> {
    vec![
        DaemonTaskDef {
            name: "briefing".to_string(),
            persona: "You are a concise daily briefing agent. Summarize what the user \
                      worked on recently based on memory context. Highlight anything \
                      that looks unfinished or time-sensitive."
                .to_string(),
            prompt: "Give me a quick briefing on what I've been working on and anything \
                     I should follow up on today."
                .to_string(),
            interval_secs: 3600, // every hour
            paused: true,        // off by default, user enables
        },
        DaemonTaskDef {
            name: "digest".to_string(),
            persona: "You are a memory digest agent. Look through the user's recent \
                      memories and create a short organized summary grouping related \
                      topics together."
                .to_string(),
            prompt: "Summarize my recent activity into a short organized digest. \
                     Group related items together."
                .to_string(),
            interval_secs: 7200, // every 2 hours
            paused: true,
        },
    ]
}

// ── Spawn a daemon task ──────────────────────────────────────────────

/// Spawn a background tokio task for a daemon definition.
///
/// The task loops: sleep for `interval_secs` (or until woken), then runs
/// the LLM with Rice memory context and sends the result through `tx`.
pub fn spawn_task(
    def: DaemonTaskDef,
    tx: mpsc::UnboundedSender<AgentEvent>,
    openai: OpenAiClient,
    openai_key: Option<String>,
    rice_future: tokio::task::JoinHandle<RiceStore>,
    rt: tokio::runtime::Handle,
) -> DaemonHandle {
    let wake = Arc::new(Notify::new());
    let wake_clone = wake.clone();
    let def_clone = def.clone();

    let handle = rt.spawn(async move {
        // Wait for our own Rice connection.
        let mut rice = match rice_future.await {
            Ok(r) => r,
            Err(_) => return,
        };

        let interval = Duration::from_secs(def_clone.interval_secs);

        loop {
            // Sleep or wait for manual wake-up.
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = wake_clone.notified() => {}
            }

            if def_clone.paused {
                continue;
            }

            let Some(key) = &openai_key else {
                let _ = tx.send(AgentEvent::DaemonResult {
                    task_name: def_clone.name.clone(),
                    message: "No OpenAI key -- skipping.".to_string(),
                    timestamp: Local::now().format("%H:%M:%S").to_string(),
                });
                continue;
            };

            // Recall memories for the prompt.
            let memories = match rice.reminisce(vec![], 6, &def_clone.prompt).await {
                Ok(traces) => traces,
                Err(_) => Vec::new(),
            };

            let memory_ctx = crate::rice::format_memories(&memories);
            let now = Local::now().format("%A, %B %e, %Y at %H:%M");

            let mut input = vec![json!({"role": "system", "content": format!(
                "{} The current date and time is {now}.",
                def_clone.persona
            )})];
            if !memory_ctx.is_empty() {
                input.push(json!({"role": "system", "content": memory_ctx}));
            }
            input.push(json!({"role": "user", "content": def_clone.prompt}));

            let result = openai.response(key, &input, None).await;

            let output_text = match result {
                Ok(response) => {
                    let items = crate::openai::extract_output_items(&response);
                    let text = crate::openai::extract_output_text(&items);
                    if text.is_empty() {
                        "(no output)".to_string()
                    } else {
                        text
                    }
                }
                Err(err) => format!("Error: {err:#}"),
            };

            // Commit to Rice memory.
            let _ = rice
                .commit_trace(
                    &def_clone.prompt,
                    &output_text,
                    &format!("daemon:{}", def_clone.name),
                    vec![],
                    &format!("memini:{}", def_clone.name),
                )
                .await;

            let _ = tx.send(AgentEvent::DaemonResult {
                task_name: def_clone.name.clone(),
                message: output_text,
                timestamp: Local::now().format("%H:%M:%S").to_string(),
            });
        }
    });

    DaemonHandle {
        def,
        wake,
        abort: handle.abort_handle(),
    }
}

/// Spawn an immediate one-shot run of a daemon task (doesn't loop).
pub fn spawn_oneshot(
    def: DaemonTaskDef,
    tx: mpsc::UnboundedSender<AgentEvent>,
    openai: OpenAiClient,
    openai_key: Option<String>,
    rice_future: tokio::task::JoinHandle<RiceStore>,
    rt: tokio::runtime::Handle,
) {
    let def_clone = def.clone();

    rt.spawn(async move {
        let mut rice = match rice_future.await {
            Ok(r) => r,
            Err(_) => return,
        };

        let Some(key) = &openai_key else {
            let _ = tx.send(AgentEvent::DaemonResult {
                task_name: def_clone.name.clone(),
                message: "No OpenAI key -- skipping.".to_string(),
                timestamp: Local::now().format("%H:%M:%S").to_string(),
            });
            return;
        };

        let memories = match rice.reminisce(vec![], 6, &def_clone.prompt).await {
            Ok(traces) => traces,
            Err(_) => Vec::new(),
        };

        let memory_ctx = crate::rice::format_memories(&memories);
        let now = Local::now().format("%A, %B %e, %Y at %H:%M");

        let mut input = vec![json!({"role": "system", "content": format!(
            "{} The current date and time is {now}.",
            def_clone.persona
        )})];
        if !memory_ctx.is_empty() {
            input.push(json!({"role": "system", "content": memory_ctx}));
        }
        input.push(json!({"role": "user", "content": def_clone.prompt}));

        let result = openai.response(key, &input, None).await;

        let output_text = match result {
            Ok(response) => {
                let items = crate::openai::extract_output_items(&response);
                let text = crate::openai::extract_output_text(&items);
                if text.is_empty() {
                    "(no output)".to_string()
                } else {
                    text
                }
            }
            Err(err) => format!("Error: {err:#}"),
        };

        let _ = rice
            .commit_trace(
                &def_clone.prompt,
                &output_text,
                &format!("daemon:{}", def_clone.name),
                vec![],
                &format!("memini:{}", def_clone.name),
            )
            .await;

        let _ = tx.send(AgentEvent::DaemonResult {
            task_name: def_clone.name.clone(),
            message: output_text,
            timestamp: Local::now().format("%H:%M:%S").to_string(),
        });
    });
}

// ── Spawn an agent window (streaming, interactive) ───────────────────

/// Spawn a one-shot agent that streams progress into an [`AgentWindow`].
///
/// Sends `Started`, then `Progress` lines as it works, then `Finished`.
/// The window_id must already be allocated by the caller.
pub fn spawn_agent_window(
    window_id: usize,
    persona: String,
    prompt: String,
    tx: mpsc::UnboundedSender<AgentEvent>,
    openai: OpenAiClient,
    openai_key: Option<String>,
    rice_future: tokio::task::JoinHandle<RiceStore>,
    rt: tokio::runtime::Handle,
) {
    rt.spawn(async move {
        let _ = tx.send(AgentEvent::Started { window_id });

        let mut rice = match rice_future.await {
            Ok(r) => r,
            Err(_) => {
                let _ = tx.send(AgentEvent::Progress {
                    window_id,
                    line: "[error] Could not connect to Rice.".to_string(),
                });
                let _ = tx.send(AgentEvent::Finished {
                    window_id,
                    message: "Failed to connect to Rice.".to_string(),
                    timestamp: Local::now().format("%H:%M:%S").to_string(),
                });
                return;
            }
        };

        let Some(key) = &openai_key else {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: "[error] No OpenAI key configured.".to_string(),
            });
            let _ = tx.send(AgentEvent::Finished {
                window_id,
                message: "No OpenAI key.".to_string(),
                timestamp: Local::now().format("%H:%M:%S").to_string(),
            });
            return;
        };

        // -- Step 1: Recall memories
        let _ = tx.send(AgentEvent::Progress {
            window_id,
            line: "Recalling memories from Rice...".to_string(),
        });

        let memories = match rice.reminisce(vec![], 6, &prompt).await {
            Ok(traces) => traces,
            Err(_) => Vec::new(),
        };

        if !memories.is_empty() {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: format!("Found {} related memory(ies).", memories.len()),
            });
        }

        let memory_ctx = crate::rice::format_memories(&memories);
        let now = Local::now().format("%A, %B %e, %Y at %H:%M");

        // -- Step 2: Build prompt and call LLM
        let _ = tx.send(AgentEvent::Progress {
            window_id,
            line: "Thinking...".to_string(),
        });

        let mut input = vec![json!({"role": "system", "content": format!(
            "{persona} The current date and time is {now}. \
             If you need more information from the user to complete the task, \
             end your response with exactly: [NEEDS_INPUT] followed by your question."
        )})];
        if !memory_ctx.is_empty() {
            input.push(json!({"role": "system", "content": memory_ctx}));
        }
        input.push(json!({"role": "user", "content": prompt.clone()}));

        let result = openai.response(key, &input, None).await;

        let output_text = match result {
            Ok(response) => {
                let items = crate::openai::extract_output_items(&response);
                let text = crate::openai::extract_output_text(&items);
                if text.is_empty() {
                    "(no output)".to_string()
                } else {
                    text
                }
            }
            Err(err) => format!("Error: {err:#}"),
        };

        // -- Step 3: Stream output line by line
        for line in output_text.lines() {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: line.to_string(),
            });
            // Small delay between lines for visual streaming effect.
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        // -- Step 4: Commit to Rice memory
        let _ = tx.send(AgentEvent::Progress {
            window_id,
            line: "Saving to Rice memory...".to_string(),
        });

        let _ = rice
            .commit_trace(
                &prompt,
                &output_text,
                &format!("agent-window:{window_id}"),
                vec![],
                &format!("memini:agent-{window_id}"),
            )
            .await;

        // -- Step 5: Check if agent needs user input
        if output_text.contains("[NEEDS_INPUT]") {
            let question = output_text
                .split("[NEEDS_INPUT]")
                .nth(1)
                .unwrap_or("How would you like me to proceed?")
                .trim()
                .to_string();
            let _ = tx.send(AgentEvent::NeedsInput {
                window_id,
                question,
            });
        } else {
            let _ = tx.send(AgentEvent::Finished {
                window_id,
                message: output_text,
                timestamp: Local::now().format("%H:%M:%S").to_string(),
            });
        }
    });
}

// ── MCP server info for agent spawning ───────────────────────────────

/// Serialisable snapshot of an MCP server + its bearer token, so that a
/// spawned agent task can open its own independent connection.
#[derive(Clone, Debug)]
pub struct McpServerSnapshot {
    pub server: McpServer,
    pub bearer: Option<String>,
    /// Pre-serialised OpenAI tool definitions for this server.
    pub openai_tools: Vec<Value>,
}

/// Spawn an agent window that has its own MCP connection(s) and runs a
/// full tool loop — just like the main chat flow, but in the background.
///
/// Results are written to a Rice state variable keyed by
/// `agent_result:<coordination_key>:<window_id>` so the orchestrator (or
/// another agent) can collect them.
pub fn spawn_agent_window_with_mcp(
    window_id: usize,
    coordination_key: String,
    persona: String,
    prompt: String,
    mcp_snapshots: Vec<McpServerSnapshot>,
    tx: mpsc::UnboundedSender<AgentEvent>,
    openai: OpenAiClient,
    openai_key: Option<String>,
    rice_future: tokio::task::JoinHandle<RiceStore>,
    rt: tokio::runtime::Handle,
) {
    rt.spawn(async move {
        let _ = tx.send(AgentEvent::Started { window_id });

        let mut rice = match rice_future.await {
            Ok(r) => r,
            Err(_) => {
                let _ = tx.send(AgentEvent::Progress {
                    window_id,
                    line: "[error] Could not connect to Rice.".to_string(),
                });
                let _ = tx.send(AgentEvent::Finished {
                    window_id,
                    message: "Failed to connect to Rice.".to_string(),
                    timestamp: Local::now().format("%H:%M:%S").to_string(),
                });
                return;
            }
        };

        let Some(key) = &openai_key else {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: "[error] No OpenAI key configured.".to_string(),
            });
            let _ = tx.send(AgentEvent::Finished {
                window_id,
                message: "No OpenAI key.".to_string(),
                timestamp: Local::now().format("%H:%M:%S").to_string(),
            });
            return;
        };

        // -- Step 1: Connect to MCP servers
        let mut connections: Vec<mcp::McpConnection> = Vec::new();
        let mut all_tools: Vec<Value> = Vec::new();

        for snap in &mcp_snapshots {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: format!("Connecting to MCP: {}...", snap.server.display_name()),
            });

            match mcp::connect_http(&snap.server, snap.bearer.clone()).await {
                Ok(mut conn) => {
                    // Refresh tools from the live connection.
                    match mcp::refresh_tools(&mut conn).await {
                        Ok(tools) => {
                            let _ = tx.send(AgentEvent::Progress {
                                window_id,
                                line: format!(
                                    "Connected to {} ({} tools).",
                                    snap.server.display_name(),
                                    tools.len()
                                ),
                            });
                            if let Ok(oai_tools) =
                                mcp::tools_to_openai_namespaced(&snap.server, &tools)
                            {
                                all_tools.extend(oai_tools);
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(AgentEvent::Progress {
                                window_id,
                                line: format!(
                                    "MCP tools refresh failed for {}: {err:#}",
                                    snap.server.display_name()
                                ),
                            });
                            // Still use pre-cached tool definitions.
                            all_tools.extend(snap.openai_tools.clone());
                        }
                    }
                    connections.push(conn);
                }
                Err(err) => {
                    let _ = tx.send(AgentEvent::Progress {
                        window_id,
                        line: format!(
                            "Failed to connect MCP {}: {err:#}. Using cached tools.",
                            snap.server.display_name()
                        ),
                    });
                    // Use pre-cached tool definitions anyway (calls will fail
                    // but the LLM can still reason about the task).
                    all_tools.extend(snap.openai_tools.clone());
                }
            }
        }

        // -- Step 2: Recall memories
        let _ = tx.send(AgentEvent::Progress {
            window_id,
            line: "Recalling memories from Rice...".to_string(),
        });

        let memories = match rice.reminisce(vec![], 6, &prompt).await {
            Ok(traces) => traces,
            Err(_) => Vec::new(),
        };

        if !memories.is_empty() {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: format!("Found {} related memory(ies).", memories.len()),
            });
        }

        let memory_ctx = crate::rice::format_memories(&memories);
        let now = Local::now().format("%A, %B %e, %Y at %H:%M");

        // -- Step 3: Build prompt and run tool loop
        let _ = tx.send(AgentEvent::Progress {
            window_id,
            line: "Thinking...".to_string(),
        });

        let mut input = vec![json!({"role": "system", "content": format!(
            "{persona} The current date and time is {now}. \
             Use available tools when needed to answer the user's request. \
             Be thorough and summarize results clearly. \
             If you need more information from the user to complete the task, \
             end your response with exactly: [NEEDS_INPUT] followed by your question."
        )})];
        if !memory_ctx.is_empty() {
            input.push(json!({"role": "system", "content": memory_ctx}));
        }
        input.push(json!({"role": "user", "content": prompt.clone()}));

        let tools_opt: Option<&[Value]> = if all_tools.is_empty() {
            None
        } else {
            Some(&all_tools)
        };

        let mut response = match openai.response(key, &input, tools_opt).await {
            Ok(r) => r,
            Err(err) => {
                let msg = format!("Error: {err:#}");
                let _ = tx.send(AgentEvent::Progress {
                    window_id,
                    line: msg.clone(),
                });
                let _ = tx.send(AgentEvent::Finished {
                    window_id,
                    message: msg,
                    timestamp: Local::now().format("%H:%M:%S").to_string(),
                });
                return;
            }
        };

        let mut output_items = openai::extract_output_items(&response);
        if !output_items.is_empty() {
            input.extend(output_items.clone());
        }
        let mut output_text = openai::extract_output_text(&output_items);
        let mut tool_calls = openai::extract_tool_calls(&output_items);
        let mut tool_loops = 0usize;

        while !tool_calls.is_empty() {
            if openai::tool_loop_limit_reached(tool_loops) {
                let _ = tx.send(AgentEvent::Progress {
                    window_id,
                    line: "Tool loop limit reached.".to_string(),
                });
                break;
            }
            tool_loops += 1;

            for call in &tool_calls {
                let _ = tx.send(AgentEvent::Progress {
                    window_id,
                    line: format!("Calling tool: {}", call.name),
                });

                // Find the right MCP connection for this namespaced tool.
                let tool_output = if let Some((server_id, tool_name)) =
                    mcp::split_namespaced_tool_name(&call.name)
                {
                    if let Some(conn) = connections.iter().find(|c| c.server.id == server_id) {
                        match mcp::call_tool(conn, tool_name, call.arguments.clone()).await {
                            Ok(value) => {
                                serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
                            }
                            Err(err) => format!(r#"{{"error":"{err}"}}"#),
                        }
                    } else {
                        format!(r#"{{"error":"No MCP connection for server '{server_id}'"}}"#)
                    }
                } else {
                    format!(r#"{{"error":"Unresolvable tool '{}'"}}"#, call.name)
                };

                let _ = tx.send(AgentEvent::Progress {
                    window_id,
                    line: format!("Tool {} returned.", call.name),
                });

                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call.call_id,
                    "output": tool_output
                }));
            }

            response = match openai.response(key, &input, tools_opt).await {
                Ok(r) => r,
                Err(err) => {
                    let _ = tx.send(AgentEvent::Progress {
                        window_id,
                        line: format!("LLM error: {err:#}"),
                    });
                    break;
                }
            };
            output_items = openai::extract_output_items(&response);
            if !output_items.is_empty() {
                input.extend(output_items.clone());
            }
            output_text = openai::extract_output_text(&output_items);
            tool_calls = openai::extract_tool_calls(&output_items);
        }

        // -- Step 4: Stream output
        for line in output_text.lines() {
            let _ = tx.send(AgentEvent::Progress {
                window_id,
                line: line.to_string(),
            });
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        // -- Step 5: Save to Rice — both as memory and as a coordination variable
        let _ = tx.send(AgentEvent::Progress {
            window_id,
            line: "Saving to Rice memory...".to_string(),
        });

        let _ = rice
            .commit_trace(
                &prompt,
                &output_text,
                &format!("agent-window:{window_id}"),
                vec![],
                &format!("memini:agent-{window_id}"),
            )
            .await;

        // Write result to coordination variable so the orchestrator can collect it.
        if !coordination_key.is_empty() {
            let coord_var = format!("agent_result:{coordination_key}:{window_id}");
            let result_value = json!({
                "window_id": window_id,
                "status": "done",
                "output": output_text,
                "timestamp": Local::now().format("%H:%M:%S").to_string(),
            });
            let _ = rice
                .set_variable(&coord_var, result_value, "agent-coordination")
                .await;
        }

        // -- Step 6: Check if agent needs user input
        if output_text.contains("[NEEDS_INPUT]") {
            let question = output_text
                .split("[NEEDS_INPUT]")
                .nth(1)
                .unwrap_or("How would you like me to proceed?")
                .trim()
                .to_string();
            let _ = tx.send(AgentEvent::NeedsInput {
                window_id,
                question,
            });
        } else {
            let _ = tx.send(AgentEvent::Finished {
                window_id,
                message: output_text,
                timestamp: Local::now().format("%H:%M:%S").to_string(),
            });
        }
    });
}

// ── Async main-chat task ─────────────────────────────────────────────

/// All state the background chat task needs (fully owned / cloned).
pub struct ChatTaskParams {
    pub key: String,
    pub message: String,
    pub persona: String,
    pub agent_name: String,
    pub memory_limit: u64,
    pub conversation_thread: Vec<Value>,
    pub mcp_snapshots: Vec<McpServerSnapshot>,
    pub builtin_tools: Vec<Value>,
    pub next_window_id: Arc<AtomicUsize>,
}

/// Spawn the main chat turn on a background tokio task.
///
/// Sends real-time `ChatProgress` / `ChatMarkdown` / `ChatFinished` events
/// through `tx` so the TUI keeps rendering and the user sees progress live.
pub fn spawn_chat_task(
    params: ChatTaskParams,
    tx: mpsc::UnboundedSender<AgentEvent>,
    openai: OpenAiClient,
    rice_future: tokio::task::JoinHandle<RiceStore>,
    rt: tokio::runtime::Handle,
) {
    rt.spawn(async move {
        let ChatTaskParams {
            key,
            message,
            persona,
            agent_name,
            memory_limit,
            conversation_thread,
            mcp_snapshots,
            builtin_tools,
            next_window_id,
        } = params;

        let mut rice = match rice_future.await {
            Ok(r) => r,
            Err(_) => {
                let _ = tx.send(AgentEvent::ChatProgress {
                    line: "Could not connect to Rice.".to_string(),
                    level: ChatLogLevel::Error,
                });
                let _ = tx.send(AgentEvent::ChatFinished {
                    user_message: message,
                    output_text: String::new(),
                    agent_name,
                    thread_entries: Vec::new(),
                });
                return;
            }
        };

        // ── Step 1: Focus Rice ───────────────────────────────────────
        let _ = rice.focus(&message).await;

        // ── Step 2: Recall memories ──────────────────────────────────
        let _ = tx.send(AgentEvent::ChatProgress {
            line: "⟳ Recalling memories…".to_string(),
            level: ChatLogLevel::Info,
        });

        let memories = match rice.reminisce(vec![], memory_limit, &message).await {
            Ok(traces) => traces,
            Err(err) => {
                let _ = tx.send(AgentEvent::ChatProgress {
                    line: format!("Rice recall failed: {err:#}"),
                    level: ChatLogLevel::Warn,
                });
                Vec::new()
            }
        };

        if !memories.is_empty() {
            let _ = tx.send(AgentEvent::ChatProgress {
                line: format!("Recalled {} memory(ies).", memories.len()),
                level: ChatLogLevel::Info,
            });
        }

        // ── Step 3: Connect to MCP servers ───────────────────────────
        let mut connections: Vec<mcp::McpConnection> = Vec::new();
        let mut all_tools: Vec<Value> = Vec::new();

        for snap in &mcp_snapshots {
            let _ = tx.send(AgentEvent::ChatProgress {
                line: format!("⟳ Connecting to MCP: {}…", snap.server.display_name()),
                level: ChatLogLevel::Info,
            });

            match mcp::connect_http(&snap.server, snap.bearer.clone()).await {
                Ok(mut conn) => {
                    match mcp::refresh_tools(&mut conn).await {
                        Ok(tools) => {
                            let _ = tx.send(AgentEvent::ChatProgress {
                                line: format!(
                                    "Connected to {} ({} tools).",
                                    snap.server.display_name(),
                                    tools.len()
                                ),
                                level: ChatLogLevel::Info,
                            });
                            if let Ok(oai_tools) =
                                mcp::tools_to_openai_namespaced(&snap.server, &tools)
                            {
                                all_tools.extend(oai_tools);
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(AgentEvent::ChatProgress {
                                line: format!(
                                    "Tools refresh failed for {}: {err:#}",
                                    snap.server.display_name()
                                ),
                                level: ChatLogLevel::Warn,
                            });
                            all_tools.extend(snap.openai_tools.clone());
                        }
                    }
                    connections.push(conn);
                }
                Err(err) => {
                    let _ = tx.send(AgentEvent::ChatProgress {
                        line: format!(
                            "MCP connect failed for {}: {err:#}",
                            snap.server.display_name()
                        ),
                        level: ChatLogLevel::Warn,
                    });
                    all_tools.extend(snap.openai_tools.clone());
                }
            }
        }

        // Add built-in tools (spawn_agent, collect_results).
        all_tools.extend(builtin_tools);

        // ── Step 4: Build LLM input ──────────────────────────────────
        let memory_context = rice::format_memories(&memories);
        let sys = rice::system_prompt(&persona, !mcp_snapshots.is_empty());
        let mut input: Vec<Value> = Vec::new();
        input.push(json!({"role": "system", "content": sys}));
        if !memory_context.is_empty() {
            input.push(json!({"role": "system", "content": memory_context}));
        }
        for msg in &conversation_thread {
            input.push(msg.clone());
        }
        input.push(json!({"role": "user", "content": message}));

        let tools_opt: Option<&[Value]> = if all_tools.is_empty() {
            None
        } else {
            Some(&all_tools)
        };

        // ── Step 5: Initial LLM call ─────────────────────────────────
        let _ = tx.send(AgentEvent::ChatProgress {
            line: "⟳ Thinking…".to_string(),
            level: ChatLogLevel::Info,
        });

        let mut response = match openai.response(&key, &input, tools_opt).await {
            Ok(r) => r,
            Err(err) => {
                let _ = tx.send(AgentEvent::ChatProgress {
                    line: format!("OpenAI request failed: {err:#}"),
                    level: ChatLogLevel::Error,
                });
                let _ = tx.send(AgentEvent::ChatFinished {
                    user_message: message,
                    output_text: String::new(),
                    agent_name,
                    thread_entries: Vec::new(),
                });
                return;
            }
        };

        let mut output_items = openai::extract_output_items(&response);
        if !output_items.is_empty() {
            input.extend(output_items.clone());
        }
        let mut output_text = openai::extract_output_text(&output_items);
        let mut tool_calls = openai::extract_tool_calls(&output_items);
        let mut tool_loops = 0usize;

        // ── Step 6: Tool-call loop ───────────────────────────────────
        while !tool_calls.is_empty() {
            if openai::tool_loop_limit_reached(tool_loops) {
                let _ = tx.send(AgentEvent::ChatProgress {
                    line: "Tool loop limit reached.".to_string(),
                    level: ChatLogLevel::Warn,
                });
                break;
            }
            tool_loops += 1;

            for call in &tool_calls {
                let _ = tx.send(AgentEvent::ChatProgress {
                    line: format!("⚙ Calling tool: {}", call.name),
                    level: ChatLogLevel::Info,
                });

                let tool_output = if call.name == "spawn_agent" {
                    handle_spawn_agent_bg(call, &mcp_snapshots, &next_window_id, &tx, &persona)
                } else if call.name == "collect_results" {
                    handle_collect_results_bg(call, &mut rice).await
                } else {
                    // MCP tool call.
                    if let Some((server_id, tool_name)) =
                        mcp::split_namespaced_tool_name(&call.name)
                    {
                        if let Some(conn) = connections.iter().find(|c| c.server.id == server_id) {
                            match mcp::call_tool(conn, tool_name, call.arguments.clone()).await {
                                Ok(value) => {
                                    let _ = tx.send(AgentEvent::ChatProgress {
                                        line: format!("✓ Tool {} returned.", call.name),
                                        level: ChatLogLevel::Info,
                                    });
                                    serde_json::to_string(&value)
                                        .unwrap_or_else(|_| "{}".to_string())
                                }
                                Err(err) => format!(r#"{{"error":"{err}"}}"#),
                            }
                        } else {
                            format!(r#"{{"error":"No MCP connection for server '{server_id}'"}}"#)
                        }
                    } else {
                        format!(r#"{{"error":"Unknown tool '{}'"}}"#, call.name)
                    }
                };

                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call.call_id,
                    "output": tool_output
                }));
            }

            let _ = tx.send(AgentEvent::ChatProgress {
                line: "⟳ Thinking…".to_string(),
                level: ChatLogLevel::Info,
            });

            response = match openai.response(&key, &input, tools_opt).await {
                Ok(r) => r,
                Err(err) => {
                    let _ = tx.send(AgentEvent::ChatProgress {
                        line: format!("OpenAI request failed: {err:#}"),
                        level: ChatLogLevel::Error,
                    });
                    break;
                }
            };
            output_items = openai::extract_output_items(&response);
            if !output_items.is_empty() {
                input.extend(output_items.clone());
            }
            output_text = openai::extract_output_text(&output_items);
            tool_calls = openai::extract_tool_calls(&output_items);
        }

        // ── Step 7: Send result ──────────────────────────────────────
        if output_text.is_empty() {
            let _ = tx.send(AgentEvent::ChatProgress {
                line: "No response received.".to_string(),
                level: ChatLogLevel::Warn,
            });
        } else {
            let _ = tx.send(AgentEvent::ChatMarkdown {
                label: agent_name.clone(),
                body: output_text.clone(),
            });
        }

        // ── Step 8: Commit to Rice ───────────────────────────────────
        let mut thread_entries = Vec::new();
        thread_entries.push(json!({"role": "user", "content": message}));
        if !output_text.is_empty() {
            thread_entries.push(json!({"role": "assistant", "content": output_text.clone()}));
        }

        let aid = rice::agent_id_for(&agent_name);
        let _ = rice
            .commit_trace(&message, &output_text, "chat", vec![], &aid)
            .await;

        let _ = tx.send(AgentEvent::ChatFinished {
            user_message: message,
            output_text,
            agent_name,
            thread_entries,
        });
    });
}

/// Handle `spawn_agent` tool call from the background chat task.
fn handle_spawn_agent_bg(
    call: &openai::ToolCall,
    mcp_snapshots: &[McpServerSnapshot],
    next_window_id: &Arc<AtomicUsize>,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    persona: &str,
) -> String {
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

    let window_id = next_window_id.fetch_add(1, Ordering::SeqCst);

    // Filter snapshots if a specific server was requested.
    let filtered: Vec<McpServerSnapshot> = if let Some(filter) = &mcp_server_filter {
        mcp_snapshots
            .iter()
            .filter(|s| s.server.id == *filter)
            .cloned()
            .collect()
    } else {
        mcp_snapshots.to_vec()
    };

    let has_mcp = !filtered.is_empty();

    // Send event to main thread to create the window + spawn the sub-agent.
    let _ = tx.send(AgentEvent::ChatSpawnAgent {
        window_id,
        label: label.clone(),
        prompt: prompt.clone(),
        mcp_snapshots: filtered,
        coordination_key: coordination_key.clone(),
        persona: persona.to_string(),
    });

    let _ = tx.send(AgentEvent::ChatProgress {
        line: format!("↗ Spawned agent: {label} (#{window_id})"),
        level: ChatLogLevel::Info,
    });

    format!(
        r#"{{"status":"spawned","window_id":{window_id},"label":"{label}","has_mcp":{has_mcp},"coordination_key":"{coordination_key}"}}"#,
    )
}

/// Handle `collect_results` tool call from the background chat task.
async fn handle_collect_results_bg(call: &openai::ToolCall, rice: &mut RiceStore) -> String {
    let coordination_key = call
        .arguments
        .get("coordination_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if coordination_key.is_empty() {
        return r#"{"error":"coordination_key is required"}"#.to_string();
    }

    // Scan Rice variables for this coordination key (window IDs 1..50).
    let mut results: Vec<Value> = Vec::new();
    for wid in 1..50usize {
        let coord_var = format!("agent_result:{coordination_key}:{wid}");
        if let Ok(Some(value)) = rice.get_variable(&coord_var).await {
            results.push(value);
        }
    }

    let summary = json!({
        "coordination_key": coordination_key,
        "agent_count": results.len(),
        "results": results,
    });

    serde_json::to_string(&summary)
        .unwrap_or_else(|_| r#"{"error":"serialize failed"}"#.to_string())
}
