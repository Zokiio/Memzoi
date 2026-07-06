---
title: OKF Profile
---

# Memzoi OKF Profile

Memzoi uses an OKF v0.1 profile for reviewable, file-native memory. The profile defines where records live, which frontmatter fields Memzoi understands, and which files are reserved for human navigation or logs.

This page describes the target authored-memory shape. Runtime indexes are derived from these files and can be rebuilt.

## Source tree

```text
.memzoi/
  config.toml        # optional repo workflow policy
  index.md
  log.md
  records/
    <path-concept-id>.md

~/.memzoi/
  config.toml        # optional user-global workflow policy
  projects/<project-key>/
    memory.db
    exports/
```

Rules:

- `.memzoi/records/*.md` is the canonical home for applied durable records.
- `.memzoi/config.toml` can set repo workflow policy, such as `[workflow] proposal_approval = "manual"`. It overrides the user-global `${MEMZOI_HOME:-~/.memzoi}/config.toml`.
- Proposals are currently DB-local workflow state in `~/.memzoi/projects/<project-key>/memory.db`; file-backed `.memzoi/proposals/*.md` proposals are planned for a later profile slice.
- `~/.memzoi/projects/<project-key>/memory.db` is derived runtime state for canonical records, plus current proposal state. `memzoi rebuild` refuses to discard readable open proposals; if the existing DB is corrupt or unreadable, rebuild treats it as derived-cache recovery and may discard DB-local proposal state.
- `~/.memzoi/projects/<project-key>/exports/` contains generated projections such as OKF exports and agent instruction files. Do not author canonical records there.

## Path concept IDs

A path concept ID names the concept represented by an OKF file path. It is not required to be an exact repository file path; use `applies_to` for repository paths affected by a record.

For canonical records, the file path is:

```text
.memzoi/records/<path-concept-id>.md
```

Path concept ID rules:

- Use lowercase ASCII letters, digits, hyphens, and `/` separators.
- Start and end each segment with a letter or digit.
- Do not use empty segments, `.`, `..`, leading `/`, or trailing `/`.
- Do not include the `.md` extension in the concept ID.
- Keep IDs stable after review; supersede a record instead of renaming to change meaning.
- Avoid reserved names `index` and `log` as terminal segments.

Examples:

```text
.memzoi/records/project/package-manager.md
.memzoi/records/apps/active/data-fetching.md
.memzoi/records/security/no-secrets-in-memory.md
```

## Reserved `index.md` and `log.md`

`index.md` is reserved for human navigation. An OKF index file must not have YAML frontmatter and must not define a memory record or proposal.

`log.md` is reserved for append-only human-readable notes or import/apply receipts. It must not define a memory record or proposal. If a machine-readable event log is needed, store it in derived runtime state or a dedicated future profile file, not as record frontmatter on `log.md`.

## Record frontmatter

A canonical record is a Markdown file with YAML frontmatter followed by the human-readable memory body.

```md
---
id: use-react-query-in-apps-active
kind: memory
version: okf/v0.1
profile: memzoi/v0
type: decision
lane: semantic
title: Use React Query in apps/active
description: apps/active uses React Query for server state.
timestamp: 2026-07-05T00:00:00Z
status: active
visibility: repo
confidence: confirmed
applies_to:
  - apps/active/**
source: human
source_ref: issue://123
supersedes: old-data-client
expires: 2027-01-01
---
# Use React Query in apps/active

apps/active uses React Query for server state and should not add a second data-fetching cache.
```

### Memzoi extension fields

These fields extend OKF v0.1 for Memzoi:

| Field | Meaning |
| --- | --- |
| `lane` | Memzoi memory lane. Valid values are `session`, `semantic`, `episodic`, and `procedural`. Records without `lane` are accepted as `semantic` for backward compatibility. |
| `status` | Lifecycle state. Canonical active record value is `active`; inbound `current` is accepted as an alias for `active` and should be normalized on write. |
| `visibility` | Sharing boundary. Valid values are `public`, `private`, `repo`, `team`, and `org`. Exports skip `private` records. |
| `confidence` | Numeric confidence `0.0`-`1.0` or a label. Label mappings: `confirmed` -> `1.0`, `likely` -> `0.75`, `uncertain` -> `0.4`. |
| `applies_to` | Repository paths, path prefixes, or trailing `/**` scopes where the record is relevant. This is separate from the path concept ID. General glob syntax is not part of the current matcher. |
| `source` | Short provenance kind such as `human`, `agent`, `import`, `issue`, `pr`, or `doc`. |
| `source_ref` | Optional durable reference for provenance, such as `issue://123`, `pr://45`, a commit SHA, or a URL. |
| `supersedes` | Optional record ID replaced by this record. Prefer this over mutating old records in place. |
| `expires` | Optional date or timestamp after which the record should stop participating in recall/precheck unless renewed. |

Memory lanes:

- `session`: active task context, handoff notes, checkpoints, or current working assumptions. Raw transcripts should remain local by default.
- `semantic`: durable project truths such as facts, decisions, constraints, preferences, warnings, and risks.
- `episodic`: chronological project memory such as session summaries, incident notes, migration notes, and handoff history.
- `procedural`: reusable workflows, runbooks, debugging recipes, release processes, and agent procedures.

`lane` is orthogonal to `type`: `lane` describes how the memory is used and retained, while `type` describes the knowledge record's content shape.

Record status values:

- `active`
- `superseded`
- `expired`
- `tombstoned`
- `redacted`

Importer compatibility:

- Accept `current` as an alias for `active`.
- Accept numeric confidence values and the labels `confirmed`, `likely`, and `uncertain`.
- Normalize generated canonical files to `active` rather than `current`.

## Proposal frontmatter

A proposal is an intended memory mutation that has not yet been applied. Current CLI/MCP proposal state is DB-local in `~/.memzoi/projects/<project-key>/memory.db` and is not restored by `memzoi rebuild`; file-backed proposal Markdown under `.memzoi/proposals/` is planned for a later slice.

```md
---
id: prop_use-react-query
kind: proposal
version: okf/v0.1
profile: memzoi/v0
operation: create
status: pending
type: decision
lane: semantic
title: Use React Query in apps/active
description: apps/active should use React Query for server state.
timestamp: 2026-07-05T00:00:00Z
visibility: repo
confidence: likely
applies_to:
  - apps/active/**
source: agent
source_ref: task://data-fetching-review
---
# Use React Query in apps/active

apps/active should use React Query for server state and avoid a second data-fetching cache.
```

Proposal status values:

- `pending`: proposal exists but has not been approved.
- `validated`: validation passed, but approval is still required. This state remains supported even when most flows do not create it.
- `approved`: proposal is approved for durable write, but canonical `.memzoi/records/*.md` has not been written.
- `applied`: proposal produced an active canonical record and no longer blocks rebuilds.
- `rejected`: proposal was intentionally closed without applying.
- `open`: synthetic filter meaning `pending`, `validated`, or `approved`.

Target flow:

1. Agents and importers create proposed changes in DB-local proposal state.
2. Review validates the proposal content, scope, privacy, and duplication risk.
3. Approval marks the proposal `approved`.
4. Applying an approved proposal writes or supersedes a canonical `.memzoi/records/*.md` file.
5. The importer rebuilds or updates the local runtime database from canonical files.
6. Export commands regenerate local runtime `exports/*` projections from the DB and canonical record state.

The built-in proposal policy is `auto`, so valid CLI/MCP proposals can enter `approved` directly. Auto-approved is not applied; canonical records are written only by CLI apply flows such as `memzoi apply <proposal-id>`, `memzoi propose --apply`, or `memzoi proposals apply --all-approved`.

MCP tools may create proposals and can override one call with `approval_mode: "auto"` or `"manual"`, but durable apply remains an explicit CLI review/apply step.

## Generated exports

Runtime `exports/` is generated output. It may contain OKF-shaped Markdown and agent instruction projections, but those files are not canonical authored memory.

Use this boundary when deciding where to edit:

| Need | Edit |
| --- | --- |
| Add a proposed memory | DB-local proposal workflow (`memzoi propose`) |
| Apply an approved memory | `.memzoi/records/*.md` through the apply/importer flow |
| Rebuild search/context indexes | Runtime `memory.db` via importer/rebuild |
| Refresh agent-facing projections | Runtime `exports/*` via export commands |
