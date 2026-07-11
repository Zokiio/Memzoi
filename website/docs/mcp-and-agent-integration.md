---
title: MCP and Agent Integration
---

# MCP and Agent Integration

Memzoi ships a minimal stdio MCP server for safe agent access. Agents can search, build context, plan capture, propose memory, and run prechecks, but they cannot review or apply capture, approve, reject, apply, supersede, tombstone, or export through MCP.

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
| `search_memory` | Search active, unexpired memory records by text with optional scope, type, path, and limit filters. |
| `inspect_memory_expiry` | Retrieve a record by ID, including an expired record, and explain normal-read eligibility without mutation. |
| `build_context_pack` | Build a prompt-ready context pack for a task, with optional local/session memory opt-in. |
| `plan_capture_v1` | Build a deterministic, evidence-backed capture plan from one explicit project-relative Markdown file without writing memory state. |
| `propose_memory` | Create a memory proposal using the effective approval policy or an `approval_mode` override. |
| `precheck_path` | Check a path against warnings, risks, and failed attempts. |
| `precheck_action` | Check a planned action, optionally scoped to a path. |
| `precheck_command` | Check a planned shell command, optionally scoped to a path. |

The server does not expose capture review/apply or lifecycle mutation tools such as approve, reject, apply, supersede, tombstone, or export. Those stay CLI-side so durable and private memory writes remain reviewable.

The CLI-only `memzoi handoff` command is not exposed as a separate MCP tool in this slice. MCP clients that need handoff-style context should call `build_context_pack` with the same task, path, token budget, and explicit opt-in policy. For CLI commands, the opt-in flags are `--include-local` and `--include-session`; for the MCP `build_context_pack` JSON input, use the fields `include_local` and `include_session`. Add any client-specific handoff framing outside Memzoi.

## `plan_capture_v1` contract

`plan_capture_v1` accepts one strict request:

```json
{
  "schema": "memzoi/capture-request-v1",
  "sources": [
    {
      "source_id": "session-findings",
      "locator": {
        "kind": "project_path",
        "path": "notes/session-findings.md"
      },
      "media_type": "text/markdown"
    }
  ],
  "extractor": {
    "profile": "markdown-deterministic"
  }
}
```

The request must contain exactly one source. It accepts only a regular UTF-8 Markdown file at a
safe POSIX project-relative path, rejects `.memzoi` and symbolic-link traversal, and reads at most
1 MiB. Unknown fields and mutation-like arguments such as `apply`, `approve`, `review`, or an
output path are rejected.

The tool uses the same deterministic extractor, duplicate/conflict inventory, identities, evidence
spans, and preconditions as `memzoi capture plan`. It is planning-only: it does not create the
runtime database, proposal directories, artifacts, events, exports, records, or any other managed
state. Its local work is bounded by the single source size limit and deterministic profile; there
is no network extractor or background capture job. The shared safeguards additionally cap the
number of headings and candidates, per-item and aggregate evidence bytes, duplicate inventory,
and serialized plan size. The server negotiates MCP `2025-06-18`: the complete plan appears in
`structuredContent` and, when the duplicated envelope fits, as serialized JSON text for client
compatibility. Near the 2 MiB wire ceiling, `content` becomes a compact plan ID/status summary
while `structuredContent` remains complete.

Only one capture planner runs at a time. Stdio input and output queues are bounded, planning has a
60-second deadline, and `notifications/cancelled` interrupts the matching request without sending
a late response. Cooperative checks cover source, inventory, extraction, and matching loops; if a
blocked worker does not stop within the 2-second grace, the server terminates instead of detaching
unbounded work. Closing stdin cancels an active plan. Capture file access currently fails closed
on Windows because the v0.4 implementation requires Unix handle-relative no-symlink opens.

The MCP boundary is deliberately stricter than the CLI artifact boundary:

- A `repo_safe` plan is returned as structured content.
- A `blocked` source returns only the redacted blocked plan and safe diagnostic codes; source
  snapshots, candidates, and evidence content are omitted.
- A `private` plan is denied by default with a constant safe error. The response does not echo
  private evidence or the rejected input.

MCP has no matching review or apply tool. Give a repo-safe plan artifact to a human-controlled CLI
workflow only after treating it as untrusted review input, then use `memzoi capture review` and
`memzoi capture apply` with their pinned IDs. See the
[capture reference](./reference.md#evidence-backed-capture) for the complete workflow.

## `propose_memory` contract

Required arguments:

- `title`
- `body`

Optional arguments:

- `type` or `memory_type`
- `scope_kind` or `scope`
- `scope_id`
- `visibility`
- `sensitivity`: `repo-safe`, `local-only`, `sensitive`, `secret`, `raw-transcript`, `private-personal-data`, `temporary-state`, or `unknown`
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
  "sensitivity": "repo-safe",
  "approval_mode": "manual"
}
```

Structured output includes the proposal ID, status, sensitivity inside the proposal payload, validation details when available, and `applied: false`. Omitted sensitivity is represented as `unknown`, never as repo-safe. Under the built-in default policy, a valid `repo-safe` proposal returns `status: "approved"` and `applied: false`; unknown or otherwise blocked sensitivity remains pending with an actionable validation issue. With `approval_mode: "manual"`, the proposal remains `pending` and `applied: false` until CLI-side review.

## Generated agent instructions

The integration CLI renders deterministic, profile-specific agent guidance from the canonical core policy metadata. It does not change memory state when listing or printing a prompt.

List the supported profiles (plain text or JSON):

```bash
memzoi integrate list
memzoi integrate list --json
```

The closed profile set is `codex`, `claude`, and `mcp`. `codex` and `claude` produce agent instruction guidance; `mcp` produces MCP setup and usage guidance. The JSON listing also describes each profile's possible default files and selection policy.

Print a one-shot generated prompt (the `--profile` option is required):

```bash
memzoi integrate prompt --profile codex
memzoi integrate prompt --profile claude
memzoi integrate prompt --profile mcp
```

The `mcp` prompt explains how to configure the server, but does not write an MCP configuration file. Generate that configuration separately with `memzoi mcp config --project-root .`.

The complete integration/workflow boundary documented by this page is:

- Git-plane repo memory in `.memzoi/records/*.md` is reviewed, durable, canonical project truth.
- Runtime-plane local/session memory under `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/` is local continuity and derived operational state, not shared Git truth. Include it in context only with explicit `--include-local` or `--include-session` opt-in.
- `memzoi propose` creates reviewable operational proposal state; an `approved` proposal is not a canonical record. Durable canonical writes require an explicit supported apply route: DB proposals use `memzoi apply <proposal-id>` or `memzoi proposals apply --all-approved` after approval (or the one-shot `memzoi propose --apply` route), while file-backed packets require review followed by `memzoi proposal-files apply <proposal-id>`. Approval or review alone never writes `.memzoi/records/*.md`.
- MCP may search, build context, run prechecks, create capture plans, and create proposal requests, but MCP never reviews/applies capture or applies canonical records. It must not claim or perform a direct canonical apply.
- Do not commit `secrets` (including credentials), `raw_chat_transcripts`, `private_personal_data`, `temporary_task_state`, or `local_only_state`. These are policy exclusions, not automatic detection, extraction, or sanitization; classify and sanitize discoveries before proposing them.

For the complete policy, see [The two planes](./memory-lifecycle.md#the-two-planes), [Destination, plane, lane, and provenance](./memory-lifecycle.md#destination-plane-lane-and-provenance), and [Command boundary](./memory-lifecycle.md#command-boundary). The [CLI reference](./reference.md) lists command syntax.

## Install generated instructions

Create or update a marked Memzoi block in an instruction file:

```bash
memzoi integrate instructions --profile codex --file AGENTS.md
memzoi integrate instructions --profile claude --file CLAUDE.md
memzoi integrate instructions --profile mcp --file AGENTS.md
```

The `--file` option is optional. Without it, `codex` targets `AGENTS.md`; `claude` reuses an existing `AGENTS.md` containing a valid Memzoi block and otherwise targets `CLAUDE.md`; `mcp` reuses a readable existing `AGENTS.md`, otherwise a readable `CLAUDE.md`, and otherwise creates `AGENTS.md` unless that path already exists, in which case it creates `CLAUDE.md`. Use `--json` for scriptable output:

```bash
memzoi integrate instructions --profile codex --file AGENTS.md --json
```

JSON output includes the resolved `file`, `profile`, `status` (`created` or `updated`), `marker`, and selection `reason`. The command replaces the content between the first valid ordered pair of markers:

```html
<!-- memzoi:start -->
<!-- memzoi:end -->
```

If no valid ordered `memzoi:start` then `memzoi:end` pair exists (including when markers are reversed), it appends a new generated block. Re-running the command replaces the same marked block with the same profile output, so installation is deterministic and idempotent while preserving content outside the markers. Instruction-file writes are integration-file writes; they do not create proposals or canonical memory records.
