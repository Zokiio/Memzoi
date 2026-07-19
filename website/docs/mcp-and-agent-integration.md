---
title: MCP and Agent Integration
---

# MCP and Agent Integration

Memzoi ships a minimal stdio MCP server for repository-only, read-only agent access. Agents can search repository memory, build repository-only context, plan capture and repository maintenance, and run prechecks. MCP cannot create proposals, expose private runtime memory, or perform any lifecycle or repository mutation.

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
| `search_memory` | Search active, current repository records by text with optional scope, type, path, and limit filters. |
| `inspect_memory_expiry` | Retrieve a repository record by ID, including an expired record, and explain normal-read eligibility without mutation. Private history is rejected without being echoed. |
| `build_context_pack` | Build a repository-only prompt-ready context pack for a task. |
| `plan_capture` | Build a deterministic, evidence-backed capture plan from one explicit project-relative Markdown file without writing memory state. |
| `plan_maintenance` | Build an immutable maintenance evidence plan from canonical repository records without writing memory or Git state. |
| `precheck_path` | Check a path against warnings, risks, and failed attempts. |
| `precheck_action` | Check a planned action, optionally scoped to a path. |
| `precheck_command` | Check a planned shell command, optionally scoped to a path. |

The server does not expose proposal creation, private maintenance/lifecycle planning, grant creation or revocation, private inspection, lifecycle apply, maintenance execution, capture review/apply, or mutation tools such as approve, reject, apply, supersede, tombstone, or export. Those stay outside this MCP surface so all MCP calls remain read-only and repository-only.

The CLI-only `memzoi handoff` command is not exposed as a separate MCP tool in
this slice. MCP clients that need handoff-style context should call
`build_context_pack` with the same task, path, and token budget, then add
client-specific handoff framing outside Memzoi. MCP rejects `include_local`
and `include_session`; private context remains available only through explicit
local CLI/core routes.

## `plan_capture` contract

`plan_capture` accepts one strict request:

```json
{
  "schema": "memzoi/capture-request",
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

This boundary intentionally does not expand with the CLI capture adapters.
`instruction-deterministic`, `adr-deterministic`, and
`git-change-deterministic` profiles, plus `project_directory`,
`supplied_bytes`, and `git_range` locators, are rejected by MCP schema and
runtime validation. Use `memzoi capture plan --request-file ...` for those
explicit-source workflows, then keep review and apply on the human-controlled
CLI path.

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

## `plan_maintenance` contract

`plan_maintenance` accepts one strict repository-only request:

```json
{
  "schema": "memzoi/maintenance-request",
  "evaluated_at": "2026-07-18T12:00:00Z",
  "record_ids": ["maintenance-plans-separate-evidence-from-execution-authority"]
}
```

`schema` is required. `evaluated_at` is optional RFC 3339; omitting it captures
one system-clock instant. `record_ids` is optional; when present it contains at
most 256 unique canonical record IDs. Omitting it evaluates all admitted
canonical repository records, up to the same 256-record planning ceiling. Unknown
fields, output paths, private/local/session
selectors, grant fields, and mutation-like arguments are rejected.

The tool invokes the same standalone snapshot planner as
`memzoi maintenance plan`; it does not initialize the normal memory service or
read private runtime records. The complete `memzoi/maintenance-plan` appears in
`structuredContent`. Text content is a bounded, content-free summary containing
the plan identity, validity times, and aggregate finding/action counts.

The current contract is `maintenance-plan/2`; the stable request and plan
schema identifiers remain `memzoi/maintenance-request` and
`memzoi/maintenance-plan`. Pre-1.0 is current-schema-only: v1 artifacts are
rejected and must be regenerated, with no alias or compatibility reader.

Planning has a 60-second deadline and supports MCP request cancellation. Success,
failure, timeout, and cancellation write no records, proposals, indexes,
overlays, private runtime rows, artifacts, WAL/event logs, or Git state. Errors
at the MCP boundary use constant safe messages rather than echoing record
content or rejected input. The artifact reports findings and action candidates
only: MCP exposes no private planning, revalidation, apply, or execution tool.

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
- Runtime-plane local/session memory under `${MEMZOI_HOME:-~/.memzoi}/projects/<repository-key>/shared.db` is local continuity shared across linked worktrees, not shared Git truth. Each worktree has a separate disposable canonical index. Include local/session memory in context only with explicit `--include-local` or `--include-session` opt-in.
- `memzoi propose` creates reviewable operational proposal state; an `approved` proposal is not a canonical record. Durable canonical writes require an explicit supported apply route: DB proposals use `memzoi apply <proposal-id>` or `memzoi proposals apply --all-approved` after approval (or the one-shot `memzoi propose --apply` route), while file-backed packets require review followed by `memzoi proposal-files apply <proposal-id>`. Approval or review alone never writes `.memzoi/records/*.md`.
- MCP may search repository memory, build repository-only context, run prechecks, create capture plans, and create repository-only maintenance plans. It cannot create or change proposal state. MCP never plans or executes private lifecycle work, exposes private maintenance artifacts, reviews/applies capture, or applies canonical records. It must not claim or perform a direct canonical apply.
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
