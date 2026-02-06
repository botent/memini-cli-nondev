//! Compile-time constants and tunables shared across the crate.

/// Application name used for config directories, Rice agent IDs, etc.
pub const APP_NAME: &str = "memini";
/// Application version injected from `Cargo.toml` at compile time.
#[allow(dead_code)]
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Rice variable key for the persisted OpenAI API key.
pub const OPENAI_KEY_VAR: &str = "openai_api_key";
/// Rice variable key for the last-used MCP server.
pub const ACTIVE_MCP_VAR: &str = "active_mcp";

/// Default Rice run-ID when `MEMINI_RUN_ID` is not set.
pub const DEFAULT_RUN_ID: &str = "memini";
/// Default OpenAI chat model.
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
/// Default OpenAI API base URL.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Maximum number of tool-call round-trips per chat turn.
pub const MAX_TOOL_LOOPS: usize = 6;
/// Default number of Rice memory traces to recall.
pub const DEFAULT_MEMORY_LIMIT: u64 = 6;
/// Maximum number of log entries kept in the activity panel.
pub const MAX_LOGS: usize = 1000;
