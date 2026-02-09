# Memini by AG\I

A lightweight Rust TUI for connecting to MCP servers and persisting state with Rice.

## What It Does

- Loads hosted MCP servers from `mcp.json` (Granola + Notion included).
- Lets you connect/select an MCP server with `/mcp connect <id>`.
- Persists the OpenAI API key and active MCP selection in Rice State.
- Provides a Ratatui-based command console.
- Treats non-slash input as chat with OpenAI, using Rice memory (and MCP tools when connected).

## Quick Start

```bash
# Clone the repo
git clone https://github.com/botent/agi-knowledge-base
cd agi-knowledge-base
cargo run
```

### Rice Configuration

Set these environment variables before running (the app also loads `.env`):

```bash
export RICE_STATE_URL="https://your-state-url"
export RICE_STATE_TOKEN="your-state-token"

# Optional, only if you want storage enabled too
export RICE_STORAGE_URL="https://your-storage-url"
export RICE_STORAGE_TOKEN="your-storage-token"

# Alternative names supported (Rice examples):
export STATE_INSTANCE_URL="grpc.example.com:80"
export STATE_AUTH_TOKEN="your-state-token"
export STORAGE_INSTANCE_URL="api.example.com:80"
export STORAGE_AUTH_TOKEN="your-storage-token"
```

### OpenAI API Key

You can import your environment key or set it manually:

```
/openai import-env
# or
/openai set sk-...
```

The key persists via Rice State under `openai_api_key`.

Optional OpenAI config:

```bash
export OPENAI_MODEL="gpt-4o-mini"
export OPENAI_EMBED_MODEL="text-embedding-3-small"
export OPENAI_BASE_URL="https://api.openai.com/v1"
```

## MCP Configuration

By default, the app loads `mcp.json` in this order:

1. `MEMINI_MCP_JSON` (if set)
2. `./mcp.json`
3. `~/.config/memini/mcp.json`
4. Embedded defaults

Example (`mcp.json`):

```json
{
  "servers": [
    {
      "id": "granola",
      "name": "Granola",
      "transport": "http",
      "url": "https://mcp.granola.ai/mcp",
      "auth": {
        "type": "oauth_browser",
        "login_url": "https://granola.ai/login",
        "notes": "Authenticate via browser OAuth flow.",
        "bearer_env": "GRANOLA_MCP_TOKEN"
      }
    },
    {
      "id": "notion",
      "name": "Notion",
      "transport": "http",
      "url": "https://mcp.notion.com/mcp",
      "auth": {
        "type": "oauth_browser",
        "notes": "Authenticate via browser OAuth flow."
      }
    }
  ]
}
```

Connect to Notion:

```
/mcp auth notion
/mcp connect notion
```

On startup, Memini by AG\I auto-connects to every configured MCP server that already
has a stored token (set `MEMINI_MCP_AUTOCONNECT=0` to disable).

When multiple MCP servers are connected, tools are namespaced as `id__tool`
(for example: `notion__search`).

## Commands

- `(no slash) chat message`
- `/help`
- `/mcp`
- `/mcp connect <id>`
- `/mcp auth <id>`
- `/mcp status`
- `/mcp tools`
- `/mcp call <tool> <json>`
- `/mcp ask <prompt>`
- `/mcp disconnect`
- `/mcp token <id> <token>`
- `/mcp token-clear <id>`
- `/openai`
- `/openai set <key>`
- `/key <key>`
- `/openai clear`
- `/openai import-env`
- `/rice`
- `/skills`
- `/skills import <skills.sh-url | github-url>`
- `/reply list`
- `/reply <id|next> <message>`
- `(plain text while asks pending) -> replies to oldest waiting agent (FIFO)`
- `/clear`
- `/quit`

## Persistence Notes

The following are persisted in Rice State:

- `openai_api_key`
- `active_mcp`
- `mcp_token_<id>`

Optional runtime config:

```bash
export MEMINI_RUN_ID="memini"
export MEMINI_MEMORY_LIMIT=6
# Optional: override local Memini home (defaults to ~/Memini)
export MEMINI_HOME="$HOME/Memini"
```

Ephemeral TUI state (like logs and cursor position) is kept in memory only.

## Homebrew Distribution

Install via Homebrew:

```bash
brew tap botent/tap
brew install memini
```

See `packaging/homebrew/memini.rb` for the formula template.
