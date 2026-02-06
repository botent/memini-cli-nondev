//! Autonomous agent daemon — background tasks that run on schedules.
//!
//! Each [`DaemonTask`] wraps an agent persona, a prompt, and an interval.
//! The daemon spawns tokio tasks that loop on their schedule, call the LLM,
//! and push results back to the TUI via an [`mpsc`] channel.
//!
//! The user interacts through `/daemon` commands:
//! - `/daemon list`          — show running and paused tasks
//! - `/daemon run <name>`    — manually trigger a task now
//! - `/daemon pause <name>`  — pause a scheduled task
//! - `/daemon resume <name>` — resume it
//! - `/daemon add <name> <interval> <prompt>` — create a new periodic task
//! - `/daemon remove <name>` — remove a task
//! - `/daemon results [name]` — show recent results

use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Notify, mpsc};

use crate::openai::OpenAiClient;
use crate::rice::RiceStore;

// ── Public types ─────────────────────────────────────────────────────

/// A message from a background agent to the TUI.
#[derive(Clone, Debug)]
pub struct AgentEvent {
    /// Name of the daemon task that produced this.
    pub task_name: String,
    /// Human-readable result text.
    pub message: String,
    /// Wall-clock timestamp.
    pub timestamp: String,
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
                let _ = tx.send(AgentEvent {
                    task_name: def_clone.name.clone(),
                    message: "⚠️ No OpenAI key — skipping.".to_string(),
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

            let _ = tx.send(AgentEvent {
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
            let _ = tx.send(AgentEvent {
                task_name: def_clone.name.clone(),
                message: "⚠️ No OpenAI key — skipping.".to_string(),
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

        let _ = tx.send(AgentEvent {
            task_name: def_clone.name.clone(),
            message: output_text,
            timestamp: Local::now().format("%H:%M:%S").to_string(),
        });
    });
}
