//! MCP (Model Context Protocol) client — connection, tool invocation, and
//! conversion helpers.

pub mod config;
pub mod oauth;

use anyhow::{Context, Result, anyhow};
use rmcp::model::{CallToolRequestParam, CallToolResult, Tool as McpTool};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

use crate::mcp::config::McpServer;
use crate::util::normalize_url;

pub const MCP_TOOL_NAMESPACE_SEP: &str = "__";

/// An active connection to a single MCP server.
pub struct McpConnection {
    pub server: McpServer,
    pub client: RunningService<RoleClient, ()>,
    pub tool_cache: Vec<McpTool>,
}

/// Open a Streamable-HTTP connection to the given MCP server.
pub async fn connect_http(server: &McpServer, bearer: Option<String>) -> Result<McpConnection> {
    let url = normalize_url(&server.url);

    let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
    config.allow_stateless = true;

    if let Some(token) = bearer {
        // rmcp adds the "Bearer " prefix internally — pass the raw token.
        let raw_token = token
            .strip_prefix("Bearer ")
            .or_else(|| token.strip_prefix("bearer "))
            .unwrap_or(&token)
            .to_string();
        config.auth_header = Some(raw_token);
    }

    let transport = StreamableHttpClientTransport::from_config(config);

    let client = ().serve(transport).await.with_context(|| format!("connect MCP at {url}"))?;

    Ok(McpConnection {
        server: server.clone(),
        client,
        tool_cache: Vec::new(),
    })
}

/// Fetch the latest tool list from the connected MCP server.
pub async fn refresh_tools(connection: &mut McpConnection) -> Result<Vec<McpTool>> {
    let tools = connection
        .client
        .list_all_tools()
        .await
        .context("list MCP tools")?;
    connection.tool_cache = tools.clone();
    Ok(tools)
}

/// Invoke a named tool on the MCP server with the given JSON arguments.
pub async fn call_tool(connection: &McpConnection, tool: &str, args: Value) -> Result<Value> {
    let arguments = match args {
        Value::Null => None,
        Value::Object(map) => Some(map),
        other => return Err(anyhow!("Tool args must be JSON object, got {other}")),
    };

    let result: CallToolResult = connection
        .client
        .call_tool(CallToolRequestParam {
            name: tool.to_string().into(),
            arguments,
        })
        .await
        .context("call MCP tool")?;

    let value = serde_json::to_value(&result).context("serialize tool result")?;
    Ok(value)
}

/// Convert MCP tool definitions into the OpenAI function-calling schema.
#[allow(dead_code)]
pub fn tools_to_openai(tools: &[McpTool]) -> Result<Vec<Value>> {
    let mut openai_tools = Vec::new();
    for tool in tools {
        let parameters =
            serde_json::to_value(&tool.input_schema).context("serialize tool schema")?;
        openai_tools.push(json!({
            "type": "function",
            "name": tool.name,
            "description": tool.description.as_deref().unwrap_or(""),
            "parameters": parameters
        }));
    }
    Ok(openai_tools)
}

pub fn namespaced_tool_name(server_id: &str, tool_name: &str) -> String {
    format!("{server_id}{MCP_TOOL_NAMESPACE_SEP}{tool_name}")
}

pub fn split_namespaced_tool_name(name: &str) -> Option<(&str, &str)> {
    name.split_once(MCP_TOOL_NAMESPACE_SEP)
}

/// Convert MCP tool definitions into an OpenAI function-calling schema, namespaced
/// by server id so multiple MCP servers can be used in one session.
pub fn tools_to_openai_namespaced(server: &McpServer, tools: &[McpTool]) -> Result<Vec<Value>> {
    let mut openai_tools = Vec::new();
    for tool in tools {
        let parameters =
            serde_json::to_value(&tool.input_schema).context("serialize tool schema")?;
        let tool_name = namespaced_tool_name(&server.id, tool.name.as_ref());
        let server_label = server.display_name();
        let base_description = tool.description.as_deref().unwrap_or("").trim();
        let description = if base_description.is_empty() {
            format!("[{server_label}]")
        } else {
            format!("[{server_label}] {base_description}")
        };
        openai_tools.push(json!({
            "type": "function",
            "name": tool_name,
            "description": description,
            "parameters": parameters
        }));
    }
    Ok(openai_tools)
}
