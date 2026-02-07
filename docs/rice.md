# Rice Memory System

Rice is the persistent memory backend that gives Memini its "I remember everything" superpower. It stores conversation threads, API keys, agent configurations, and semantic memory traces.

## What Gets Stored in Rice

| Data                | Purpose                                |
| ------------------- | -------------------------------------- |
| Conversation thread | So you can pick up where you left off  |
| OpenAI API key      | Encrypted at rest, no need to re-enter |
| Active MCP server   | Remembers your last tool connection    |
| Custom personas     | Your created agent personalities       |
| Active persona      | Which persona you were using           |
| Shared workspace    | Team workspace you last joined         |
| Memory traces       | Semantic memory of past interactions   |

## Configuration

### Interactive Setup (Recommended)

From inside Memini:

```
/rice setup
```

This walks you through entering your Rice State URL, token, and optional Storage credentials. Everything is saved to `.env` automatically.

### Manual Setup

Add to your `.env` file:

```bash
# Required for memory
RICE_STATE_URL="grpc.your-instance.com:80"
RICE_STATE_TOKEN="your-token"

# Optional for file storage
RICE_STORAGE_URL="api.your-instance.com:80"
RICE_STORAGE_TOKEN="your-token"
```

Alternative variable names (from Rice examples):

```bash
STATE_INSTANCE_URL="grpc.example.com:80"
STATE_AUTH_TOKEN="your-token"
STORAGE_INSTANCE_URL="api.example.com:80"
STORAGE_AUTH_TOKEN="your-token"
```

### Checking Status

```
/rice
```

Shows whether Rice is connected and your current run ID.

## How Memory Works

### Focus

Every message you send is "focused" into Rice — creating a semantic embedding that can be recalled later.

### Recall

When you ask a question, Memini searches for relevant past interactions and includes them as context. This is controlled by:

```bash
export MEMINI_MEMORY_LIMIT=6   # number of traces to recall (default: 6)
```

### Commit

After each conversation turn, a trace (input + action + outcome) is committed to Rice for future recall.

## Shared Workspaces

Multiple users can share the same memory pool by joining a workspace:

```
/share join team-project
```

Everyone who joins the same workspace name on the same Rice instance shares:

- Memory traces (focus/recall/commit)
- Conversation context

To return to your private memory:

```
/share leave
```

The last workspace you joined is persisted and automatically restored on next launch.

## Run ID

Each Memini instance uses a run ID to scope its data. Default is `memini`. Override with:

```bash
export MEMINI_RUN_ID="my-project"
```

This lets you maintain separate memory pools for different projects.
