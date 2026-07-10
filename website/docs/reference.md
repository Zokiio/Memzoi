---
title: Reference
---

# Reference

This page summarizes Memzoi v0's public CLI, MCP, and model values.

## CLI commands

| Command | Purpose |
| --- | --- |
| `memzoi init` | Initialize repo `.memzoi/` memory and local runtime state. |
| `memzoi propose` | Propose a new memory record. Built-in default auto-approves valid proposals but does not apply them. |
| `memzoi proposals` | List, show, and bulk-apply proposal inbox state. |
| `memzoi proposal-files` | List, show, validate, and apply OKF proposal files under `.memzoi/proposals/pending/`. |
| `memzoi local` | Add, list, and search local-only runtime memory records. |
| `memzoi checkpoint` | Add and list runtime session checkpoints. |
| `memzoi events` | Export runtime event-log rows. |
| `memzoi session-end` | Promote explicit structured session-end candidates into proposal files or runtime memory. |
| `memzoi approve` | Approve a pending or validated memory proposal. |
| `memzoi reject` | Reject a proposed memory. |
| `memzoi apply` | Apply an approved memory proposal into canonical `.memzoi/records/*.md`. |
| `memzoi supersede` | Atomically supersede an active, non-private repo record with a same-scope repo-safe replacement. |
| `memzoi tombstone` | Atomically tombstone an active, non-private repo record. |
| `memzoi search` | Search active, unexpired memory records. |
| `memzoi expiry` | Inspect a record by ID and explain its expiry eligibility without mutating it. |
| `memzoi context` | Build a prompt-ready context pack for a task. |
| `memzoi handoff` | Build a compact context pack for switching agents or harnesses. |
| `memzoi precheck` | Check planned work against risky memories before acting. |
| `memzoi export` | Export active repo memory into reviewable files. |
| `memzoi rebuild` | Rebuild the derived SQLite database from canonical `.memzoi/records/` files. |
| `memzoi doctor` | Check installation and repo memory readiness. |
| `memzoi quickstart` | Print or run a tiny first-run workflow. |
| `memzoi update` | Check for or apply a Memzoi release update. |
| `memzoi mcp` | Print MCP integration configuration. |
| `memzoi integrate` | Generate or install agent integration prompts and instructions. |

Run `memzoi <command> --help` for exact options.

## Common command options

| Command | Important options |
| --- | --- |
| `init` | `--force`, `--json` |
| `propose` | `--type`, `--scope-kind`, `--visibility`, `--sensitivity`, `--source-kind`, `--source-ref`, `--title`, `--body`, `--actor`, `--manual`, `--auto-approve`, `--apply`, `--json` |
| `proposals list` | `--status open\|pending\|validated\|approved\|rejected\|applied\|all`, `--json` |
| `proposals show` | `<proposal-id>`, `--json` |
| `proposals apply` | `--all-approved`, `--actor`, `--json` |
| `proposal-files list` | `--json` |
| `proposal-files show` | `<proposal-id>`, `--json` |
| `proposal-files validate` | `--json` |
| `proposal-files apply` | `<proposal-id>`, `--actor`, `--json` |
| `proposal-files reject` | `<proposal-id>`, `--reason`, `--actor`, `--json` |
| `local add` | `--type`, `--title`, `--body`, `--actor`, `--json` |
| `local list` | `--json` |
| `local search` | `<query>`, `--limit`, `--json` |
| `checkpoint add` | `--task`, `--note` or `--from-file`, `--actor`, `--json` |
| `checkpoint list` | `--json` |
| `events export` | `--jsonl` |
| `session-end` | `--from-file <path>` or `--from-checkpoint <checkpoint-id>`, `--actor`, `--json` |
| `approve` | `<proposal-id>`, `--actor`, `--json` |
| `reject` | `<proposal-id>`, `--reason`, `--actor`, `--json` |
| `apply` | `<proposal-id>`, `--actor`, `--json` |
| `supersede` | `<record-id>`, `--type`, `--scope-kind`, `--visibility`, `--sensitivity`, `--source-kind`, `--source-ref`, `--title`, `--body`, `--actor`, `--json` |
| `tombstone` | `<record-id>`, `--reason`, `--actor`, `--json` |
| `search` | `<query>`, `--scope-kind`, `--type`, `--path`, `--limit`, `--json` |
| `expiry` | `<record-id>`, `--json` |
| `context` | `--task`, `--path`, `--token-budget`, `--include-local`, `--include-session`, `--json` |
| `handoff` | `--task` or `--path`, `--token-budget`, `--include-local`, `--include-session`, `--json` |
| `precheck` | `--path`, `--action`, `--command`, `--scope-kind`, `--json` |
| `export` | `<format>`, `--scope-kind`, `--json` |
| `rebuild` | `--json` |
| `doctor` | `--project-root`, `--json` |
| `quickstart` | `--apply-sample`, `--json` |
| `update` | `--check`, `--ref`, `--json` |
| `mcp config` | `--project-root` |
| `integrate list` | `--json` |
| `integrate prompt` | `--profile` |
| `integrate instructions` | `--profile`, `--file`, `--json` |

## Classified import

The import workflow accepts a compact, explicit manifest. It does not discover or parse
agent instruction files, chat transcripts, ADRs, or other source formats, and it does not
infer a destination from prose. Each candidate already carries its intended destination
and a reason for that classification. The lifecycle policy that governs the destination
boundary is documented in [Destination classification in the lifecycle policy](./memory-lifecycle.md#destination-plane-lane-and-provenance).

### Commands and options

```bash
memzoi import plan --from-file <manifest.yml> [--actor cli] [--json]
memzoi import apply --from-file <manifest.yml> --plan-id <import_…> [--actor cli] [--json]
```

`--from-file` is required for both commands. `--actor` defaults to `cli` and is part of
the plan fingerprint; use the same actor when applying a plan. `--json` emits one JSON
object instead of the human-readable summary. `plan` is the review step and is
mutation-free. `apply` recomputes the plan from the manifest and current memory state,
then requires the supplied `--plan-id` to match before it writes anything.

### Manifest (`memzoi/import-v1`)

The YAML document has exactly these top-level keys; unknown keys are rejected:

```yaml
version: memzoi/import-v1
sources:
  - path: imports/source.yml       # or url: https://… or ref: issue://123
candidates:
  - destination: repo              # repo | local | session | discard | needs_review
    reason: durable project convention
    type: decision                  # optional when it can be inferred
    lane: semantic                  # optional
    title: Explicit candidate title
    body: Explicit candidate body
    sensitivity: repo-safe          # repo-safe | local-only | sensitive | secret | unknown
    scope:
      kind: repo                    # optional; defaults to repo
      id: null                      # optional
      paths: [src/**]               # optional; project-relative paths
    tags: [workflow]                # optional; defaults to []
```

There must be at least one source and one candidate. Each source needs a non-empty
`path`, `url`, or `ref`; a `path` must be a POSIX project-relative path and cannot be
absolute or contain `.`/`..` components, backslashes, or a drive prefix. Candidate
`destination`, `reason`, `title`, and `body` are required and are trimmed before use.
`repo` candidates must declare `sensitivity: repo-safe`. Scope paths have the same
project-relative validation, and tags cannot be empty.

The parser is strict at every manifest object (`version`, `sources`, `candidates`,
source fields, candidate fields, and scope fields). It rejects malformed YAML, an empty
document, unsupported versions, missing required values, empty source locators, invalid
paths, an empty candidate list, and candidates whose type cannot be inferred.

Inference is deliberately narrow and deterministic:

- Without `type`, `lane: episodic` or `lane: session` infers `type: episode`, and
  `lane: procedural` infers `type: procedure`. Other non-session candidates must provide
  `type`.
- Without `lane`, `type: procedure` infers `procedural`, `type: episode` infers
  `episodic`, and other types infer `semantic`.
- A `session` destination is always normalized to `type: episode` and `lane: session`,
  regardless of a conflicting input value.
- Missing `scope` means `{kind: repo, id: null, paths: []}`. Tags and scope paths are
  trimmed, sorted, and deduplicated; source locators are trimmed and sorted.

### Plan and apply semantics

`import plan` returns schema `memzoi/import-plan-v1`, a deterministic `plan_id`, the
normalized `sources`, a `summary`, and one normalized result per candidate. With `--json`,
the plan envelope also includes `mode: "plan"`, the effective `actor`, and the manifest
`source_file`; the plan envelope has no `writes` field.
`source_file` is project-relative when the manifest resolves under the project root; it is
`null` when the manifest is outside that root or either path cannot be resolved.

The plan fingerprint uses the trimmed actor and normalized plan (including the current
duplicate scan), so it is stable for the same actor, manifest, and current memory state.
Planning does not create proposal files, canonical records, local/session records, or
runtime database writes. A plan may contain private/local candidates; do not blindly
commit plan output.

The summary always contains these counters: `total`, `create_proposals`,
`local_writes`, `session_writes`, `duplicates`, `discarded`, and `needs_review`.
Each candidate includes `index`, `classification`, `policy`, normalized `type`, `lane`,
`title`, `body`, optional `sensitivity`, `scope`, `tags`, a trimmed-body BLAKE3
`content_hash`, `duplicates`, and `action`.

Action JSON is tagged by `action.kind`:

```json
{"kind":"create_proposal","proposal_id":"mem_import_example","path":".memzoi/proposals/pending/mem_import_example.md"}
{"kind":"create_runtime","route":"runtime_local"}
{"kind":"create_runtime","route":"runtime_session"}
{"kind":"duplicate","matches":[{"kind":"canonical_record","id":"mem_…","destination":"repo","candidate_index":null}]}
{"kind":"no_write","reason":"stale transient note"}
{"kind":"blocked","reason":"ambiguous privacy boundary"}
```

`repo` uses `create_proposal` and writes only a pending review packet. `local` and
`session` use `create_runtime` and write private runtime records on guarded apply.
`discard` is `no_write`; `needs_review` is `blocked`. A duplicate action takes
precedence over destination handling and also does not write.

`import apply` returns the same plan plus `mode: "apply"`, `actor`, `source_file`,
`expected_plan_id`, and `writes`. It recomputes the plan and fails with a stale-plan
error when the ID differs; that guard makes a wrong or stale ID a zero-write operation.
`create_proposal` and `create_runtime` actions produce typed entries in `writes`:

```json
{"kind":"proposal_file","index":0,"proposal_id":"mem_import_example","path":".memzoi/proposals/pending/mem_import_example.md"}
{"kind":"runtime_record","index":1,"record_id":"local-example","destination":"local"}
{"kind":"runtime_record","index":2,"record_id":"session-example","destination":"session"}
```

Repo writes create `status: proposed` OKF proposal files under
`.memzoi/proposals/pending/`; they **do not create canonical records** under
`.memzoi/records/`. Local/session writes create private active records only in the
runtime SQLite plane. Review and explicitly apply the pending repo proposal with the
proposal-file workflow when it is appropriate; successful apply updates the
derived repo index in the same operation.

### Duplicate and no-write behavior

Duplicate detection hashes the trimmed candidate body with BLAKE3 and compares it with
canonical records, pending proposal files, active runtime records, and earlier candidates
in the same input. Matches are reported as `canonical_record`, `pending_proposal`,
`runtime_record`, or `earlier_candidate`, with their ID and (when applicable) destination
or candidate index. Duplicate matches are sorted deterministically; the duplicate action
prevents another proposal file.

No files or runtime rows are created when planning, when a candidate is discarded,
blocked, or duplicated, when the manifest fails validation, or when apply receives a
wrong/stale plan ID. Proposal-file and runtime writes are one guarded operation: a
runtime failure rolls back the SQLite transaction and removes proposal files created by
that attempt. Import apply never promotes a candidate implicitly and never writes a
canonical record directly.

## Context JSON

`memzoi context --json` and MCP `build_context_pack` return the prompt-ready pack plus metadata. Existing fields such as `prompt`, `records`, `citations`, and `token_budget` remain available. Recalled record JSON may include `proposal_id` as review lineage. Citation JSON intentionally uses the original evidence fields instead: `provenance` (plane), `destination`, optional `source_kind`, and optional `source_ref`.

The serialized `provenance` values are `git` and `runtime`. `git` identifies ownership by canonical `.memzoi/records/*.md` files; it does not mean the record bypassed SQLite, because SQLite is a derived runtime index/cache. For recalled records, serialized `destination` remains `repo`, `local`, or `session`; destination is routing, not provenance. `source_kind` and `source_ref` are nullable evidence metadata and remain independent of both one another and `proposal_id`. Apply/rebuild/export round-trip all three; audit events also identify the approving proposal.

The additive metadata fields are:

- `budget`: requested budget, effective budget, approximate used budget, and estimate unit.
- `included`: selected records with compact citation, provenance, destination, score, rationale, and estimated size metadata.
- `omitted`: capped repo-record metadata for relevant records excluded by budget.
- `warnings`: structured notices, currently empty for context ranking.
- `next_queries`: targeted follow-up queries, currently empty.

## Memory planes and destinations

The policy API accepts these serialized storage-plane values for `provenance` (and `MemoryPlane`):

- `git`
- `runtime`

`MemoryDestination::ALL` accepts these serialized destination values:

- `repo`
- `local`
- `session`
- `discard`
- `needs_review`

The policy mapping is:

| Destination | Plane | Write route | Review |
| --- | --- | --- | --- |
| `repo` | `git` | `file_backed_proposal` | `proposal_review` |
| `local` | `runtime` | `runtime_local` | `no_review` |
| `session` | `runtime` | `runtime_session` | `no_review` |
| `discard` | `null` (no plane) | `no_write` | `no_review` |
| `needs_review` | `null` (no plane) | `no_write` | `human_decision` |

`team` and `cloud` are future-only destination labels; they are not accepted serialized values in the current policy. Recalled records can have only the plane-backed destinations `repo`, `local`, or `session`. See [Destination classification in the lifecycle policy](./memory-lifecycle.md#destination-plane-lane-and-provenance) for destination behavior and lifecycle commands; this reference page intentionally does not duplicate that command matrix.

## Handoff JSON

`memzoi handoff --json` returns handoff metadata plus the full context pack under `context`. It requires `--task` or `--path`; path-only handoff uses the stable effective task `Handoff for path <path>`.

Top-level fields include:

- `id`: handoff pack id.
- `task`: effective task.
- `path_prefix`: requested path, if supplied.
- `token_budget`, `include_local`, `include_session`: requested handoff options.
- `proposal_inbox`: DB-backed open proposal counts from the proposal inbox, not `.memzoi/proposals/pending`.
- `context`: full context pack JSON, including `records`, `citations`, `policy`, `budget`, `included`, `omitted`, and `warnings`.
- `created_at`: creation timestamp.

## Event JSONL export

`memzoi events export --jsonl` emits runtime event-log rows from SQLite as JSONL. Each
non-empty line is one compact standalone JSON object; there is no top-level array, wrapper,
or pretty multi-line JSON. An empty event log succeeds with empty stdout.

Event objects include:

- `id`
- `event_type`
- `actor`
- `payload`
- `record_id`
- `proposal_id`
- `created_at`

The JSONL stream is operational runtime state for bulk or append-only consumption. It is
not canonical memory, not rebuild input, and does not replace `.memzoi/records/*.md` or
`.memzoi/proposals/pending/*.md` files.


## Update Command

`memzoi update` checks GitHub Releases and updates supported Mac/Linux release-binary installs. Automatic apply mode never installs from branches, SHAs, URLs, or shell scripts; unsupported installs may print manual commands that use the official install scripts. Use `memzoi update --check` to report update state without changing files.

Supported refs:

- `latest`: resolve the latest GitHub release.
- `vX.Y.Z`: install a stable release tag.
- `X.Y.Z`: normalize to `vX.Y.Z`.

JSON status values:

- `up_to_date`
- `update_available`
- `updated`
- `unsupported`
- `invalid_ref`
- `download_failed`
- `checksum_mismatch`
- `rollback_failed`

`--check --json` works from source, Cargo, package-managed, Windows, and CI installs. Apply mode is limited to release-binary installs where `memzoi` and `memzoi-mcp` are sibling binaries in a writable, non-package-managed directory.

## MCP tools

| Tool | Required arguments | Optional arguments |
| --- | --- | --- |
| `search_memory` | `query` | `scope_kind`, `scope`, `type`, `memory_type`, `path`, `path_prefix`, `limit` |
| `inspect_memory_expiry` | `record_id` | none |
| `build_context_pack` | `task` | `path`, `path_prefix`, `token_budget`, `include_local`, `include_session` |
| `propose_memory` | `title`, `body` | `type`, `memory_type`, `scope_kind`, `scope`, `scope_id`, `visibility`, `sensitivity`, `tags`, `source_kind`, `source_ref`, `confidence`, `actor`, `approval_mode` |
| `precheck_path` | `path` | `scope_kind`, `scope` |
| `precheck_action` | `action` | `path`, `scope_kind`, `scope` |
| `precheck_command` | `command` | `path`, `scope_kind`, `scope` |

## Memory types

Valid `--type` and `memory_type` values:

- `fact`
- `preference`
- `decision`
- `procedure`
- `episode`
- `relationship`
- `warning`
- `failed_attempt`
- `risk`
- `instruction_projection`

## Memory lanes

Valid record `lane` values:

- `session`
- `semantic`
- `episodic`
- `procedural`

Records without `lane` remain valid and are treated as `semantic`. `lane` is separate from `type`: lane describes memory usage and retention, while type describes the record content.

## Proposal file schema values

OKF-compatible proposal files live under `.memzoi/proposals/pending/*.md`. They are review packets and use:

```yaml
status: proposed
proposal:
  action: create
```

Valid proposal file actions:

- `create`
- `supersede`
- `tombstone`

`supersede` proposals require exactly one `supersedes` target and a reason.
`tombstone` proposals require exactly one `proposal.target` and a reason.
Create packets cannot name a target. Before mutation, apply rejects a target
that is missing, inactive, cross-scope, or newer than `proposal.proposed_at`.
`update` is intentionally unsupported in the file profile.

Valid proposal sensitivity values:

- `repo-safe`
- `local-only`
- `sensitive`
- `secret`
- `raw-transcript`
- `private-personal-data`
- `temporary-state`
- `unknown`

The current CLI/MCP proposal inbox remains DB-local workflow state and uses the operational proposal statuses below.

Proposal file review commands:

```bash
memzoi proposal-files list
memzoi proposal-files show <proposal-id>
memzoi proposal-files validate
memzoi proposal-files apply <proposal-id>
memzoi proposal-files reject <proposal-id> --reason "..."
```

`list`, `show`, and `validate` are read-only. `list` and `validate` describe the pending inbox; `show` can also inspect a resolved packet. `validate` includes target existence, active-state, scope, and freshness checks for supersede/tombstone packets. `apply` accepts a `status: proposed`, `sensitivity: repo-safe` packet, writes its canonical changes and derived SQLite rows atomically, then moves the packet to `.memzoi/proposals/resolved/applied/`. Create writes one active record; supersede preserves the target as `superseded` and creates one lineage-linked active replacement; tombstone preserves the target evidence with `status: tombstoned`. `reject` creates no canonical record and moves the packet to `.memzoi/proposals/resolved/rejected/` with an explicit reason. Resolution metadata records the outcome, actor, timestamp, reason, and affected record IDs. Repeating the same outcome is an auditable no-op; requesting the opposite outcome is refused.

Git-plane apply blocks every value except `repo-safe`, including `secret`, `sensitive`, `local-only`, `raw-transcript`, `private-personal-data`, `temporary-state`, and `unknown`; there is no override flag. Missing legacy sensitivity is treated as `unknown`. Classify or sanitize blocked proposals before repo apply, or route local/session content to the runtime plane.

## Local runtime memory

Local memory commands:

```bash
memzoi local add --type preference --title "..." --body "..."
memzoi local list
memzoi local search <query>
```

Local records are stored in the runtime project database under `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/memory.db`. They are marked as `destination: local`, `visibility: private`, and `source_kind: memzoi-local` in JSON output.

Local records are not written to `.memzoi/records/**`, are not returned by global `memzoi search`, and are not exported into repo-shared agent files. `memzoi context` is repo-only by default and includes local records only with `--include-local`. Use later proposal workflows to promote local memory into repo-shared memory.

## Session checkpoints

Checkpoint commands:

```bash
memzoi checkpoint add --task "..." --note "..."
memzoi checkpoint add --task "..." --from-file notes.md
memzoi checkpoint list
```

Checkpoints are stored in the runtime project database under `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/memory.db`. They are marked as `destination: session`, `lane: session`, `type: episode`, `visibility: private`, and `source_kind: memzoi-checkpoint` in JSON output.

Checkpoints store only explicit `--note` or `--from-file` content. They are not written to `.memzoi/records/**`, are not returned by global `memzoi search`, and are not exported into repo-shared agent files. `memzoi context` is repo-only by default and includes checkpoints only with `--include-session`. Use later session-end proposal workflows to promote durable findings into repo memory.

## Session-end promotion

Session-end promotion reads only explicit structured YAML, either from a file or from an existing checkpoint body:

```bash
memzoi session-end --from-file notes.yml
memzoi session-end --from-checkpoint <checkpoint-id>
```

The input must include a `task` and a `candidates` list:

```yaml
task: "Implement auth middleware"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Protected routes validate sessions server-side
    body: Protected routes must validate sessions server-side.
    sensitivity: repo-safe
    reason: Learned while implementing middleware.
    scope:
      kind: repo
      paths:
        - src/auth/**
    tags:
      - auth
      - security
```

Memzoi validates the whole batch and prepares repo proposal files before writing. `repo` candidates must be `repo-safe` and become pending `.memzoi/proposals/pending/*.md` proposal files only; they are not applied and do not write canonical `.memzoi/records/*.md` files. `local` candidates create private runtime records. `session` candidates create runtime checkpoint records. Runtime row writes are transactional, and created proposal files are cleaned up if a later promotion step fails. `discard` and `needs_review` candidates create no writes.

`session-end` does not inspect transcripts, chat logs, shell history, hidden agent state, or context packs. Free-text notes and checkpoints are rejected until a future extraction workflow exists.

## Scope kinds

Valid `--scope-kind`, `scope_kind`, and `scope` values:

- `personal`
- `repo`
- `project`
- `team`
- `org`
- `agent`
- `imported_untrusted`

## Visibility values

Valid visibility values:

- `public`
- `private`
- `repo`
- `team`
- `org`

Exports skip `private` records.

## Status values

Operational proposal inbox statuses:

- `pending`: proposal exists but has not been approved.
- `validated`: validation passed, but approval is still required. This state remains supported even when most flows do not create it.
- `approved`: proposal is approved for durable write, but canonical `.memzoi/records/*.md` has not been written.
- `applied`: proposal produced an active canonical record and no longer blocks rebuilds.
- `rejected`: proposal was intentionally closed without applying.
- `open`: synthetic filter meaning `pending`, `validated`, or `approved`.

Record statuses:

- `active`
- `superseded`
- `expired`
- `tombstoned`
- `redacted`

Auto-approval means `approved`, not `applied`.


## Approval policy

Effective proposal approval mode is resolved in this order:

1. Built-in default: `auto`.
2. User-global config: `${MEMZOI_HOME:-~/.memzoi}/config.toml`.
3. Repo config: `.memzoi/config.toml`.
4. CLI or MCP per-call override.

Config shape:

```toml
[workflow]
proposal_approval = "manual" # or "auto"
```

CLI overrides:

- `memzoi propose --manual` creates a `pending` proposal.
- `memzoi propose --auto-approve` forces auto-approval for one proposal.
- `memzoi propose --apply --sensitivity repo-safe` creates, approves, and applies through the CLI. It is incompatible with `--manual`.
- Omitted sensitivity is serialized as `unknown`; validation and apply both refuse canonical promotion until it is explicitly `repo-safe`.

MCP override:

- `propose_memory` accepts `approval_mode: "auto"` or `"manual"`.
- MCP rejects `apply` and `auto_apply`; MCP never writes canonical records.

## Export formats

Valid `memzoi export <format>` values:

- `okf`
- `agents-md`
- `claude-md`

## v0 limitations

- Source installs require a Rust/Cargo-capable environment; release binaries do not.
- Search is text/FTS-first, not vector or semantic recall.
- Memory is repo-local; global, personal, team, and org sync are future work.
- `memzoi rebuild` restores approved records from `.memzoi/records/`. Current proposals are DB-local; rebuild refuses to discard readable open proposals and should be unblocked with `memzoi proposals list --status open`, `memzoi proposals apply --all-approved`, or `memzoi reject <proposal-id> --reason "..."`. A corrupt unreadable DB causes rebuild to fail before deleting local or session runtime rows.
- MCP is intentionally minimal and safe-by-default. It can create proposals under the effective approval policy, but it cannot apply canonical records.
- Homebrew and package-manager installers are not available yet.
