# Command Reference

All commands start with `/`. Anything without a `/` is sent as a chat message.

## General

| Command           | Description                       |
| ----------------- | --------------------------------- |
| `/help`           | Show the full command list in-app |
| `/clear`          | Clear the activity log            |
| `/quit` / `/exit` | Exit Memini                       |

## Chat & Memory

| Command           | Description                                       |
| ----------------- | ------------------------------------------------- |
| _(just type)_     | Chat with your AI — it recalls past conversations |
| `/memory <query>` | Search your saved memories                        |
| `/thread`         | Show current conversation info                    |
| `/thread clear`   | Start a fresh conversation                        |

## Personas

| Command                              | Description                   |
| ------------------------------------ | ----------------------------- |
| `/agent`                             | List available personas       |
| `/agent use <name>`                  | Switch to a different persona |
| `/agent create <name> <description>` | Create a custom persona       |
| `/agent delete <name>`               | Remove a custom persona       |
| `/agent info`                        | Show current persona details  |

## Agents (Multi-Instance)

| Command           | Description                                |
| ----------------- | ------------------------------------------ |
| `/spawn <prompt>` | Spin up a live agent window                |
| `/spawn list`     | Show all agent windows and their status    |
| `Tab`             | Cycle through agents in the grid           |
| `Enter`           | Open the selected agent session            |
| `Esc`             | Return to dashboard from an agent session  |
| `Ctrl+1..9`       | Jump directly to an agent session by index |

## Autopilot (Background Tasks)

| Command                               | Description                     |
| ------------------------------------- | ------------------------------- |
| `/auto`                               | List available background tasks |
| `/auto run <name>`                    | Run a task immediately          |
| `/auto start <name>`                  | Start a recurring task          |
| `/auto stop <name>`                   | Stop a running task             |
| `/auto add <name> <seconds> <prompt>` | Create a custom recurring task  |
| `/auto remove <name>`                 | Remove a task                   |
| `/auto results [name]`                | View recent task outputs        |

## Integrations (MCP)

| Command                      | Description                       |
| ---------------------------- | --------------------------------- |
| `/mcp`                       | List available tool servers       |
| `/mcp connect <id>`          | Connect to a tool server          |
| `/mcp auth <id>`             | Authenticate via browser (OAuth)  |
| `/mcp auth-code <id> <code>` | Complete OAuth with a URL or code |
| `/mcp ask <prompt>`          | Chat using connected tools        |
| `/mcp tools [id\|all]`       | List available MCP tools          |
| `/mcp disconnect [id\|all]`  | Disconnect MCP server(s)          |

## Shared Workspaces

| Command              | Description                   |
| -------------------- | ----------------------------- |
| `/share`             | Show current workspace status |
| `/share join <name>` | Join a shared workspace       |
| `/share leave`       | Return to private memory      |

## Settings

| Command             | Description                         |
| ------------------- | ----------------------------------- |
| `/openai`           | Show AI key status                  |
| `/openai set <key>` | Save your OpenAI key                |
| `/key <key>`        | Quick-set OpenAI key                |
| `/rice`             | Show Rice connection status         |
| `/rice setup`       | Interactive Rice environment wizard |

## Keyboard Shortcuts

| Key                   | Action                    |
| --------------------- | ------------------------- |
| `Ctrl+C`              | Quit                      |
| `Ctrl+L`              | Clear activity log        |
| `Tab`                 | Cycle grid selection      |
| `Enter`               | Open agent / submit input |
| `Esc`                 | Back / clear input / quit |
| `PageUp` / `PageDown` | Scroll activity log       |
| `Up` / `Down`         | Browse input history      |
