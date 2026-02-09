//! MCP server configuration — loading, lookup, and serialisation.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::constants::APP_NAME;

/// Top-level MCP configuration containing one or more server entries.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpConfig {
    pub servers: Vec<McpServer>,
}

/// A single MCP server entry with connection and auth details.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpServer {
    pub id: String,
    pub name: Option<String>,
    pub url: String,
    #[serde(default)]
    pub sse_url: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    pub auth: Option<McpAuth>,
}

/// Authentication configuration for a single MCP server.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpAuth {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub login_url: Option<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub bearer_env: Option<String>,
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_id_env: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub client_secret_env: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub authorization_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

impl McpServer {
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.id.clone())
    }
}

/// Where the MCP configuration was loaded from.
#[derive(Clone, Debug)]
pub enum McpSource {
    Embedded,
    File(PathBuf),
}

impl McpSource {
    pub fn label(&self) -> String {
        match self {
            McpSource::Embedded => "embedded defaults".to_string(),
            McpSource::File(path) => path.display().to_string(),
        }
    }
}

impl McpConfig {
    pub fn load() -> Result<(Self, McpSource)> {
        if let Ok(path) = env::var("MEMINI_MCP_JSON") {
            let path = PathBuf::from(path);
            return Ok((Self::load_from_path(&path)?, McpSource::File(path)));
        }

        let cwd_path = PathBuf::from("mcp.json");
        if cwd_path.exists() {
            return Ok((Self::load_from_path(&cwd_path)?, McpSource::File(cwd_path)));
        }

        if let Some(config_path) = config_dir_file("mcp.json") {
            if config_path.exists() {
                return Ok((
                    Self::load_from_path(&config_path)?,
                    McpSource::File(config_path),
                ));
            }
        }

        let embedded: McpConfig = serde_json::from_str(include_str!("../../mcp.json"))
            .context("parse embedded mcp.json")?;
        Ok((embedded, McpSource::Embedded))
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("read mcp config from {}", path.display()))?;
        let config = serde_json::from_str(&contents)
            .with_context(|| format!("parse mcp config from {}", path.display()))?;
        Ok(config)
    }

    pub fn find_by_id_or_name(&self, query: &str) -> Option<McpServer> {
        let query = query.to_lowercase();
        self.servers
            .iter()
            .find(|server| {
                server.id.to_lowercase() == query
                    || server
                        .name
                        .as_ref()
                        .map(|name| name.to_lowercase() == query)
                        .unwrap_or(false)
            })
            .cloned()
    }
}

fn config_dir_file(filename: &str) -> Option<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", APP_NAME, APP_NAME)?;
    Some(proj_dirs.config_dir().join(filename))
}
