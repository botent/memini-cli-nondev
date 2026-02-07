# Getting Started with Memini

Memini is an interactive TUI for chatting with AI through MCP servers, with persistent memory powered by Rice.

## Installation

```bash
# Clone and build
git clone <repo-url>
cd memini-cli-nondev
cargo build --release

# The binary is at target/release/memini
```

Or via Homebrew (if a tap is published):

```bash
brew tap <org>/<tap>
brew install memini
```

## First Launch

```bash
cargo run
# or
./target/release/memini
```

On first launch you'll see the Memini dashboard with an activity log on the left and a 3×3 agent grid on the right.

## Quick Setup (Interactive)

The fastest way to get started is the built-in setup wizard:

1. Launch Memini
2. Type `/rice setup` and press Enter
3. Follow the prompts to enter your Rice State URL, token, and optionally Storage URL/token
4. Memini saves everything to `.env` and reconnects automatically

That's it — you're connected!

## Manual Setup

If you prefer, create a `.env` file in the project root:

```bash
# Rice State (required for memory)
RICE_STATE_URL="grpc.your-rice-instance.com:80"
RICE_STATE_TOKEN="your-state-token"

# Rice Storage (optional)
RICE_STORAGE_URL="api.your-rice-instance.com:80"
RICE_STORAGE_TOKEN="your-storage-token"

# OpenAI (can also be set from inside the TUI)
OPENAI_API_KEY="sk-..."
```

Alternative env var names are also supported:

```bash
STATE_INSTANCE_URL="grpc.example.com:80"
STATE_AUTH_TOKEN="your-token"
STORAGE_INSTANCE_URL="api.example.com:80"
STORAGE_AUTH_TOKEN="your-token"
```

## Setting Your OpenAI Key

From within the TUI:

```
/key sk-your-openai-key
```

Or:

```
/openai set sk-your-openai-key
```

If `OPENAI_API_KEY` is in your environment, import it:

```
/openai import-env
```

The key is persisted in Rice so you only need to set it once.

## Your First Chat

Just type anything and press Enter — no slash prefix needed:

```
What's the weather like today?
```

Memini will use your OpenAI key to respond, and if Rice is connected, it'll remember the conversation.

## Next Steps

- Read [commands.md](commands.md) for a full command reference
- Read [agents.md](agents.md) to learn about personas and multi-agent workflows
- Read [mcp.md](mcp.md) to connect external tools (Notion, Granola, etc.)
- Read [rice.md](rice.md) for more on Rice memory and shared workspaces
