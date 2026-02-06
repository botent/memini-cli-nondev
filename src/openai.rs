//! OpenAI API client — chat responses, embeddings, and response helpers.

use anyhow::{Context, Result, anyhow};
use reqwest::Client as HttpClient;
use serde::Serialize;
use serde_json::{Value, json};

use crate::constants::{DEFAULT_OPENAI_BASE_URL, DEFAULT_OPENAI_MODEL, MAX_TOOL_LOOPS};
use crate::util::env_first;

/// A single tool-call extracted from an OpenAI response.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
    pub call_id: String,
}

/// Thin wrapper around the OpenAI HTTP API.
#[derive(Clone)]
pub struct OpenAiClient {
    pub model: String,
    pub base_url: String,
    http_client: HttpClient,
}

impl OpenAiClient {
    pub fn new() -> Self {
        let model = env_first(&["OPENAI_MODEL", "MEMINI_OPENAI_MODEL"])
            .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());
        let base_url = env_first(&["OPENAI_BASE_URL", "OPENAI_API_BASE"])
            .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string());
        OpenAiClient {
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            http_client: HttpClient::new(),
        }
    }

    pub async fn response(
        &self,
        key: &str,
        input: &[Value],
        tools: Option<&[Value]>,
    ) -> Result<Value> {
        let mut body = json!({
            "model": self.model,
            "input": input,
        });
        if let Some(tools) = tools {
            body["tools"] = Value::Array(tools.to_vec());
        }
        self.request(key, "responses", body).await
    }

    async fn request(&self, key: &str, path: &str, body: Value) -> Result<Value> {
        let client = self.http_client.clone();
        let base_url = self.base_url.clone();
        let url = format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let payload = body;
        let key = key.to_string();

        let response = client
            .post(url)
            .bearer_auth(key)
            .json(&payload)
            .send()
            .await
            .context("send OpenAI request")?;
        let status = response.status();
        let text = response.text().await.context("read OpenAI response")?;
        let json: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
        if !status.is_success() {
            return Err(anyhow!("OpenAI error {status}: {json}"));
        }
        Ok(json)
    }
}

/// Pull the top-level `output` array from an OpenAI response.
pub fn extract_output_items(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Concatenate all `output_text` blocks from the output items into a single string.
pub fn extract_output_text(output_items: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in output_items {
        let item_type = item.get("type").and_then(|v| v.as_str());
        if item_type != Some("message") {
            continue;
        }
        let content = match item.get("content").and_then(|v| v.as_array()) {
            Some(content) => content,
            None => continue,
        };
        for block in content {
            if block.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

/// Collect all `function_call` items into structured [`ToolCall`] values.
pub fn extract_tool_calls(output_items: &[Value]) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for item in output_items {
        if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
            continue;
        }
        let name = match item.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let call_id = match item.get("call_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let raw_args = item
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let arguments =
            serde_json::from_str(raw_args).unwrap_or_else(|_| json!({"_raw": raw_args}));
        calls.push(ToolCall {
            name,
            arguments,
            call_id,
        });
    }
    calls
}

/// Returns `true` when the tool-call loop has hit the configured ceiling.
pub fn tool_loop_limit_reached(tool_loops: usize) -> bool {
    tool_loops >= MAX_TOOL_LOOPS
}

/// Pretty-print any serialisable value as JSON, with a safe fallback.
pub fn format_json<T: Serialize>(value: T) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "<unrenderable>".to_string())
}
