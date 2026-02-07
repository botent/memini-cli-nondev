# MCP Tool Integrations

Memini connects to external tools via the [Model Context Protocol (MCP)](https://modelcontextprotocol.io). Out of the box, it ships with configs for Granola and Notion.

## Configuration

MCP servers are defined in `mcp.json`. Memini looks for this file in order:

1. Path in `MEMINI_MCP_JSON` env var
2. `./mcp.json` (project root)
3. `~/.config/memini/mcp.json`
4. Embedded defaults

### Example `mcp.json`

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

## Connecting

### OAuth Flow (Browser)

```
/mcp auth notion
```

This opens your browser for authentication. After logging in, the token is stored locally so you don't need to re-authenticate.

### Direct Connection

If you already have a token:

```
/mcp connect notion
```

### Auto-connect

On startup, Memini automatically connects to every MCP server that already has a stored token. Disable with:

```bash
export MEMINI_MCP_AUTOCONNECT=0
```

## Using Tools

Once connected, tools are available to the AI automatically. You can also invoke them explicitly:

```
/mcp ask search for my project notes
/mcp tools              # list all available tools
/mcp tools notion       # list tools from a specific server
```

### Namespacing

When multiple servers are connected, tools are namespaced as `serverId__toolName` (e.g. `notion__search`, `granola__list_meetings`).

## Disconnecting

```
/mcp disconnect notion   # disconnect one server
/mcp disconnect all      # disconnect everything
```
