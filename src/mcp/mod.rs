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

pub struct McpConnection {
    pub server: McpServer,
    pub client: RunningService<RoleClient, ()>,
    pub tool_cache: Vec<McpTool>,
}

pub async fn connect_http(server: &McpServer, bearer: Option<String>) -> Result<McpConnection> {
    let url = normalize_url(&server.url);

    let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
    config.allow_stateless = true;

    if let Some(token) = bearer {
        config.auth_header = Some(token);
    }

    let transport = StreamableHttpClientTransport::from_config(config);

    let client = ().serve(transport).await.with_context(|| format!("connect MCP at {url}"))?;

    Ok(McpConnection {
        server: server.clone(),
        client,
        tool_cache: Vec::new(),
    })
}

pub async fn refresh_tools(connection: &mut McpConnection) -> Result<Vec<McpTool>> {
    let tools = connection
        .client
        .list_all_tools()
        .await
        .context("list MCP tools")?;
    connection.tool_cache = tools.clone();
    Ok(tools)
}

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
