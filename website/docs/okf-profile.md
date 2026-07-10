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
  proposals/
    pending/
      <proposal-id>.md
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
- `.memzoi/proposals/pending/*.md` is the review packet shape for proposed memory mutations before they become canonical records.
- `.memzoi/config.toml` can set repo workflow policy, such as `[workflow] proposal_approval = "manual"`. It overrides the user-global `${MEMZOI_HOME:-~/.memzoi}/config.toml`.
- Proposal files are schema-defined review packets. The current CLI/MCP proposal inbox is still DB-local workflow state until a later lifecycle slice wires file proposals into commands.
- `~/.memzoi/projects/<project-key>/memory.db` is derived runtime state for canonical records, plus current CLI/MCP proposal state. `memzoi rebuild` refuses to discard readable open proposals; if the existing DB is corrupt or unreadable, rebuild treats it as derived-cache recovery and may discard DB-local proposal state.
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
| `proposal_id` | Optional ID of the review packet that approved the record. This is proposal lineage, not evidence provenance, and is kept separate from `source`/`source_ref`. |
| `supersedes` | Optional record ID replaced by this record. Prefer this over mutating old records in place. |
| `expires` | Optional `YYYY-MM-DD` (start of that date in UTC) or RFC 3339 timestamp with an explicit timezone. At and after that instant, the active record is excluded from normal search, context, handoff, precheck, runtime lists/show, and generated exports without changing its canonical file or status. |

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

A proposal file is an intended memory mutation that has not yet been applied. It is a verbose review packet, not durable memory itself.

Pending proposal files live under:

```text
.memzoi/proposals/pending/<proposal-id>.md
```

The initial proposal status is always `proposed`. Do not confuse file proposal status with the operational CLI inbox states `pending`, `validated`, `approved`, `applied`, and `rejected`.

```md
---
id: mem_2026_07_06_auth_001
kind: proposal
version: okf/v0.1
profile: memzoi/v0
type: decision
lane: semantic
title: Protected routes must validate sessions server-side
description: Protected API routes must validate sessions server-side instead of trusting client auth state.
status: proposed
proposal:
  action: create
  proposed_by: agent
  proposed_at: 2026-07-06T00:00:00Z
  reason: Learned during auth middleware migration.
  confidence: medium
scope:
  kind: project
  paths:
    - src/auth/**
tags:
  - auth
  - middleware
  - security
timestamp: 2026-07-06T00:00:00Z
created_by: agent
sources:
  - path: src/auth/session.ts
supersedes: []
sensitivity: repo-safe
---
# Protected routes must validate sessions server-side

Protected API routes must validate the session server-side. Do not trust client-side auth state for authorization decisions.
```

Required proposal fields:

- `id`
- `type`
- `title`
- `description`
- `lane`
- `status`
- `proposal`
- `timestamp`
- `sensitivity`

The nested `proposal` object requires:

- `action`
- `proposed_by`
- `proposed_at`

Proposal actions:

| Action | Meaning |
| --- | --- |
| `create` | Propose a new canonical memory record. |
| `supersede` | Propose a new memory that replaces exactly one active memory. Requires one `supersedes` target and `proposal.reason`. |
| `tombstone` | Propose marking exactly one active memory intentionally inactive. Requires `proposal.target` and `proposal.reason`. |

`update` is intentionally unsupported in the first file profile. Meaningful changes should usually create a superseding memory rather than silently editing an existing record in place.

Proposal sensitivity values:

| Sensitivity | Meaning |
| --- | --- |
| `repo-safe` | Safe to commit after review. |
| `local-only` | Useful locally but should not become repo-shared memory. |
| `sensitive` | Requires explicit human review before any sharing. |
| `secret` | Must not be committed or applied into repo records. |
| `raw-transcript` | Raw conversation content that must not become repo-shared memory. |
| `private-personal-data` | Private personal information that must not become repo-shared memory. |
| `temporary-state` | Short-lived task state that belongs in local/session memory rather than canonical repo memory. |
| `unknown` | Conservative default requiring review. |

New packets should always declare sensitivity. Legacy packets that omit it
remain readable as `unknown`, and every non-`repo-safe` value is blocked at
canonical apply even if the packet or DB proposal was auto-approved.

Validation checks:

- Pending packets use `status: proposed`. Resolved packets use `status: applied`
  or `status: rejected` and include matching `resolution` metadata.
- `proposal.action` must be `create`, `supersede`, or `tombstone`.
- `lane` must be `session`, `semantic`, `episodic`, or `procedural`.
- `type` must use current lowercase Memzoi values such as `decision`, `fact`, `procedure`, `risk`, or `failed_attempt`.
- `sensitivity` must be one of the listed sensitivity values.
- `create` proposals cannot name a target.
- `supersede` proposals must include exactly one `supersedes` target and no
  `proposal.target`.
- `tombstone` proposals must include exactly one `proposal.target` and no
  `supersedes` entries.
- `supersede` and `tombstone` require a reviewable `proposal.reason`.
- Apply verifies that the target exists, is active, has the same scope kind and
  scope ID, and has not changed since `proposal.proposed_at`.
- The body must be non-empty and should include enough review context to understand the intended memory change.

Proposal-to-record mapping:

```text
.memzoi/proposals/pending/mem_2026_07_06_auth_001.md
  -> approve/apply
.memzoi/records/semantic/decisions/auth-session-validation.md
.memzoi/proposals/resolved/applied/mem_2026_07_06_auth_001.md
```

Rejected packets move to `resolved/rejected/` instead and create no canonical
record. The resolved packet retains the reviewed proposal evidence and adds a
Git-readable outcome, reviewer, timestamp, reason, and affected record IDs.

For an applied packet, canonical `source`/`source_ref` point to its original
evidence locator (for example `path` plus `src/auth/session.ts`), while
`proposal_id` points to the packet that approved the change. Rebuild and OKF
exports preserve both. Recall citations deliberately cite the evidence fields;
audit events and resolved packets carry the proposal lineage.

The resulting canonical record may be a compact projection of the proposal. Review-only fields such as `proposal.reason`, proposal confidence, and review notes do not need to be copied into canonical record frontmatter unless they remain durable project knowledge.

A supersede apply preserves the old record and its evidence with
`status: superseded`, then creates an active replacement whose `supersedes`
field points to the old record. A tombstone apply preserves the target's body,
source, tags, paths, and earlier lineage while changing its status to
`tombstoned`; the resolved packet retains the reason. Both changes and their
derived search updates commit through the same all-or-nothing lifecycle as
create.

Compactness policy:

```text
proposal = review packet
record   = compact durable memory
index    = generated machine projection
```

Prefer one canonical record per durable concept, decision, workflow, warning, handoff, or reusable cluster. Avoid generating one file per tiny observation.

## Generated exports

Runtime `exports/` is generated output. It may contain OKF-shaped Markdown and agent instruction projections, but those files are not canonical authored memory.

Use this boundary when deciding where to edit:

| Need | Edit |
| --- | --- |
| Add a proposed memory | DB-local proposal workflow (`memzoi propose`) |
| Apply an approved memory | `.memzoi/records/*.md` through the apply/importer flow |
| Rebuild search/context indexes | Runtime `memory.db` via importer/rebuild |
| Refresh agent-facing projections | Runtime `exports/*` via export commands |
