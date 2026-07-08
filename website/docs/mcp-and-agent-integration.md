---
title: MCP and Agent Integration
---

# MCP and Agent Integration

Memzoi ships a minimal stdio MCP server for safe agent access. Agents can search, build context, propose memory, and run prechecks, but they cannot approve, reject, apply, supersede, tombstone, or export through MCP.

MCP proposals follow the effective proposal approval policy:

- Built-in default is `auto`, so valid proposals return `approved`.
- `approved` is not `applied`; no canonical `.memzoi/records/*.md` file is written by MCP.
- A client can pass `approval_mode: "manual"` on `propose_memory` to keep that proposal `pending`.
- A client can pass `approval_mode: "auto"` to force auto-approval for that proposal.
- Apply-like arguments such as `apply` or `auto_apply` are rejected. Use the CLI apply workflow for durable writes.

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
| `build_context_pack` | Build a prompt-ready context pack for a task, with optional local/session memory opt-in. |
| `propose_memory` | Create a memory proposal using the effective approval policy or an `approval_mode` override. |
| `precheck_path` | Check a path against warnings, risks, and failed attempts. |
| `precheck_action` | Check a planned action, optionally scoped to a path. |
| `precheck_command` | Check a planned shell command, optionally scoped to a path. |

The server does not expose lifecycle mutation tools such as approve, reject, apply, supersede, tombstone, or export. Those stay CLI-side so durable memory writes remain reviewable.

The CLI-only `memzoi handoff` command is not exposed as a separate MCP tool in this slice. MCP clients that need handoff-style context should call `build_context_pack` with the same task, path, token budget, and explicit local/session opt-in policy, then add any client-specific handoff framing outside Memzoi.

## `propose_memory` contract

Required arguments:

- `title`
- `body`

Optional arguments:

- `type` or `memory_type`
- `scope_kind` or `scope`
- `scope_id`
- `visibility`
- `tags`
- `source_kind`
- `source_ref`
- `confidence`
- `actor`
- `approval_mode`: `"auto"` or `"manual"`

Example manual proposal:

```json
{
  "title": "Keep MCP writes reviewable",
  "body": "MCP clients may propose memory, but durable record writes must use the CLI apply workflow.",
  "type": "decision",
  "approval_mode": "manual"
}
```

Structured output includes the proposal ID, status, validation details when available, and `applied: false`. Under the built-in default policy, a valid proposal returns `status: "approved"` and `applied: false`. With `approval_mode: "manual"`, it returns `status: "pending"` and `applied: false`.

## Agent instruction prompt

Print a one-shot prompt that teaches an agent how to use Memzoi:

```bash
memzoi integrate prompt
```

The prompt tells agents to:

- Run `memzoi context --task "<task>"` before non-trivial work.
- Use `memzoi handoff --task "<task>"` when switching CLI agents or harnesses.
- Add `--path <relative/path>` when editing specific files.
- Add `--include-local` or `--include-session` only when private runtime memory or explicit checkpoints should be part of the context.
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
