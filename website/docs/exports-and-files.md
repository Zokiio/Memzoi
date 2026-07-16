---
title: Exports and Files
---

# Exports and Files

Memzoi keeps canonical authored memory under repo `.memzoi/records/` and keeps runtime state under
`~/.memzoi/projects/<repository-key>/`. OKF-compatible proposal files are schema-defined under
`.memzoi/proposals/pending/`, while the current CLI/MCP proposal inbox is still DB-local workflow
state in repository-wide `shared.db`. Valid CLI proposals default to `approved`, but approved is not
applied: canonical record files are written only by explicit CLI apply flows. Rebuild replaces only
the current worktree's disposable `index.db`; it preserves local/session memory and proposals in
`shared.db` and fails closed if that shared authority is unreadable. See the [OKF profile](./okf-profile.md)
for the file-native source layout.

## Format roles

- Markdown with YAML frontmatter under `.memzoi/records/` is the canonical durable memory source.
- `~/.memzoi/projects/<repository-key>/shared.db` stores repository-shared local/session memory and DB-local proposal state.
- `~/.memzoi/projects/<repository-key>/worktrees/<worktree-key>/index.db` is a disposable projection of the active checkout's canonical records.
- JSON is for single command responses and MCP payloads, including existing `--json` output.
- JSONL/NDJSON is opt-in for append-only or bulk streams. It is not canonical memory and is not rebuild input.


## Bundle layout

After `memzoi init`, the repo-local memory directory contains:

```text
.memzoi/
  config.toml        # optional repo workflow policy
  index.md
  proposals/
    pending/
  records/
```

The local Memzoi home can contain user-global workflow policy and generated project state:

```text
~/.memzoi/
  config.toml        # optional user-global workflow policy
  projects/<repository-key>/
    config.toml      # runtime project config, not workflow policy
    shared.db
    repo-lifecycle.lock
    worktrees/<worktree-key>/
      index.db
      exports/
```

Workflow policy config is separate from the runtime project config. Effective proposal approval mode is resolved in this order:

1. Built-in default: `auto`.
2. User-global `${MEMZOI_HOME:-~/.memzoi}/config.toml`.
3. Repo `.memzoi/config.toml`.
4. CLI or MCP per-call override.

```toml
[workflow]
proposal_approval = "manual" # or "auto"
```

The runtime project config under `~/.memzoi/projects/<repository-key>/config.toml` is shared by linked worktrees; it is not the repo/user workflow policy file. Worktree indexes and exports remain isolated. Legacy path-keyed directories are migrated non-destructively: durable local/session records and proposals are merged with conflict checks, canonical indexes are rebuilt, and legacy directories are retained with a migration receipt.

## Proposal inbox and rebuild

Open proposals are `pending`, `validated`, or `approved`. Use the inbox commands to inspect and close them before rebuilding:

```bash
memzoi proposals list --status open
memzoi proposals show <proposal-id>
memzoi proposals apply --all-approved
memzoi reject <proposal-id> --reason "not durable repo knowledge"
memzoi rebuild
```

`memzoi propose --manual` keeps one proposal pending. `memzoi propose --apply` is a CLI-only shortcut that writes a canonical record after approval. MCP proposal calls can auto-approve or stay manual, but MCP never applies.

## Export formats

```bash
memzoi export okf
memzoi export agents-md
memzoi export claude-md
```

`okf` writes a generated projection, not the canonical record source. It emits one Markdown file per
active, non-private memory record for the selected scope. Each export file includes YAML frontmatter
with stable fields such as id, type, scope, visibility, status, confidence, timestamps, source
metadata, content hash, and applicable paths. Canonical authored records under `.memzoi/records/`
use the [OKF profile](./okf-profile.md) fields and are restored by `memzoi rebuild`.

`agents-md` writes an AGENTS-style projection to:

```text
~/.memzoi/projects/<repository-key>/worktrees/<worktree-key>/exports/AGENTS.memory.md
```

`claude-md` writes a CLAUDE-style projection to:

```text
~/.memzoi/projects/<repository-key>/worktrees/<worktree-key>/exports/CLAUDE.memory.md
```

Instruction projections include active, non-private records of these types:

- `procedure`
- `decision`
- `warning`
- `risk`

They intentionally skip background fact records that are useful for search but too noisy for always-on agent instructions.

## Event-log JSONL

```bash
memzoi events export --jsonl
```

`memzoi events export --jsonl` streams runtime event-log rows from the SQLite `event_log`
table as JSONL: one compact event object per physical line, with no wrapper and no pretty
multi-line JSON. This operational stream is not canonical memory, is not consumed by
`memzoi rebuild`, and does not replace `.memzoi/records/*.md` records or OKF proposal
files under `.memzoi/proposals/pending/*.md`.


## Generated file policy

- Commit `.memzoi/records/*` when the records are durable repo knowledge.
- Commit `.memzoi/proposals/pending/*` only when the proposal is intentionally being reviewed in Git and has `sensitivity: repo-safe`.
- Commit `.memzoi/config.toml` only when the repo intentionally overrides workflow policy.
- Do not commit runtime `shared.db` or `index.db`; they live under the local Memzoi home directory.
- Keep generated runtime exports out of Git unless explicitly copied into reviewed agent instructions.
- Regenerate exports after memory lifecycle changes with `memzoi export agents-md`, `memzoi export claude-md`, or `memzoi export okf`.

## Scope and privacy

Exports include active records for the selected `--scope-kind`, defaulting to `repo`, and skip records with `private` visibility. Repo-shared memory should not contain secrets or private personal data.
