# Agents & Personas

Memini supports two kinds of agents: **personas** (how the AI talks to you) and **spawned agents** (independent workers in the grid).

## Personas

A persona customises the AI's personality and behaviour. The default persona is `memini`.

### Switching Personas

```
/agent                    # list available personas
/agent use researcher     # switch to "researcher"
/agent info               # see current persona details
```

### Creating Custom Personas

```
/agent create mybot You are a helpful coding assistant who writes Rust.
```

The first word after `create` is the name; everything after is the persona description (system prompt).

### Deleting Personas

```
/agent delete mybot
```

Custom personas are persisted in Rice, so they survive restarts.

## Spawned Agents (Multi-Instance)

Spawned agents run independently in the 3×3 grid on the dashboard. Each gets its own context and can call MCP tools.

### Spawning an Agent

```
/spawn summarize my last 3 meetings
```

This creates a new agent window that starts working immediately. You can see its progress in the grid card.

### Interacting with Agents

- **Tab** to cycle through grid cells
- **Enter** to open a full-screen session with the selected agent
- **Esc** to return to the dashboard
- **Ctrl+1..9** to jump directly to agent #1–9

If an agent needs input (e.g. clarification), it will automatically open its session view and prompt you.

### How Agents Delegate

When you chat with Memini and it has MCP tools connected, it **always** delegates work to spawned agents rather than calling tools directly. This means:

1. You ask a question
2. Memini breaks it into sub-tasks
3. Each sub-task gets its own agent window
4. Agents work in parallel in the grid
5. Results are collected and synthesized

This gives you visibility into every step of the work.
