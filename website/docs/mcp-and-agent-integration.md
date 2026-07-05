---
title: MCP and Agent Integration
---

# MCP and Agent Integration

Memzoi ships a minimal stdio MCP server for safe agent access. Agents can search, build context, propose memory, and run prechecks, but they cannot approve or apply proposals through MCP.

## Generate MCP config

```bash
memzoi mcp config --project-root .
```

Example output:

```json
{
  "mcpServers": {
    "memzoi": {
      "command": "memzoi-mcp",
      "args": ["--project-root", "/absolute/path/to/repo"],
      "env": {}
    }
  }
}
```

`memzoi mcp config` resolves `--project-root` to an absolute path so the MCP client can start the server from any working directory.

## Safe MCP tools

The server exposes:

| Tool | Purpose |
| --- | --- |
| `search_memory` | Search active memory records by text with optional scope, type, path, and limit filters. |
| `build_context_pack` | Build a prompt-ready context pack for a task. |
| `propose_memory` | Create a pending memory proposal. |
| `precheck_path` | Check a path against warnings, risks, and failed attempts. |
| `precheck_action` | Check a planned action, optionally scoped to a path. |
| `precheck_command` | Check a planned shell command, optionally scoped to a path. |

The server does not expose lifecycle mutation tools such as approve, apply, supersede, tombstone, or export. Those stay CLI-side so durable memory writes remain reviewable.

## Agent instruction prompt

Print a one-shot prompt that teaches an agent how to use Memzoi:

```bash
memzoi integrate prompt
```

The prompt tells agents to:

- Run `memzoi context --task "<task>"` before non-trivial work.
- Add `--path <relative/path>` when editing specific files.
- Run `memzoi precheck` before risky actions.
- Propose durable repo knowledge with `memzoi propose`.
- Avoid secrets, raw chat logs, temporary task progress, and private user facts.

## Install instruction block

Create or update a marked Memzoi block in an instruction file:

```bash
memzoi integrate instructions --file AGENTS.md
```

Use `--json` for scriptable output:

```bash
memzoi integrate instructions --file AGENTS.md --json
```

The command replaces content between:

```html
<!-- memzoi:start -->
<!-- memzoi:end -->
```

If the markers are missing, it appends a new block. This makes future instruction updates deterministic and reviewable.
