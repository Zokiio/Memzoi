---
title: Memory Lifecycle
---

# Memory Lifecycle

This page is the normative, human-readable statement of Memzoi's two-plane memory
policy. The executable contract is exposed by `MemoryDestination::ALL`,
`MemoryDestination::policy()`, `MemoryPlane`, and `TWO_PLANE_MEMORY_POLICY` in
`memzoi-core`; this page explains what that contract means for users and agents.

## The two planes

Memzoi deliberately separates shared Git truth from local runtime continuity:

| Plane | Responsibility | Canonical location and authority |
| --- | --- | --- |
| **Git** | Reviewed, durable, repo-shared project knowledge: facts, decisions, procedures, warnings, risks, and failed attempts that belong in the repository. | `.memzoi/records/*.md` is the canonical source. These compact Markdown records are diffable and are restored into runtime indexes by `memzoi rebuild`. |
| **Runtime** | Fast local recall, private preferences, task continuity, checkpoints, and derived operational state. | `${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/` contains the project runtime state, including `memory.db` and generated exports. It is noncanonical and must not be treated as Git truth. |

The Git plane may also contain a pending file-backed proposal at
`.memzoi/proposals/pending/<proposal-id>.md` when it is intentionally being
reviewed in Git and is `sensitivity: repo-safe`. A pending proposal is a review
packet, not a canonical record. Applying it explicitly creates the compact
record under `.memzoi/records/`; the pending file is not itself the durable
memory source.

Runtime rows are not a second canonical source for repo memory. Rebuild reads
the Git records, recreates derived runtime state, and preserves compatible
local/session runtime rows; it does not promote runtime rows or make SQLite
canonical. See [Exports and files](./exports-and-files.md) for file layout and
commit guidance.

## Destination, plane, lane, and provenance

**Destination** is the pre-write answer to “where may this candidate go?”
**Lane** is the knowledge-use/retention shape (`session`, `semantic`,
`episodic`, or `procedural`). A lane never grants permission to write to a
plane. **Provenance** reports the plane from which recalled or prechecked
memory came (`git` or `runtime`); it is intentionally distinct from
destination. Recall and context output should therefore be read as separate
`provenance=...` and `destination=...` fields. See
[Recall and precheck](./recall-and-precheck.md) for output details.

The current destination set is exactly the five values in
`MemoryDestination::ALL`:

| Destination | Plane | Write route | Review requirement | Meaning |
| --- | --- | --- | --- | --- |
| `repo` | `git` | `file_backed_proposal` | `proposal_review` | Repo-shared durable knowledge; write a pending file-backed proposal before a canonical record. |
| `local` | `runtime` | `runtime_local` | `no_review` | Private local runtime memory; never a repo record by this route. |
| `session` | `runtime` | `runtime_session` | `no_review` | Task continuity/checkpoint state; never a repo record by this route. |
| `discard` | none | `no_write` | `no_review` | Do not retain the candidate. |
| `needs_review` | none | `no_write` | `human_decision` | Do not write it yet; a human must decide the sharing boundary. |

`team` and `cloud` are future-only labels. They are not accepted
`MemoryDestination` values, do not have a current plane or write route, and
must not be presented as available destinations. There is no team runtime
plane, hosted storage, or sync implementation in this MVP.

## Git-plane responsibilities and exclusions

Git-plane memory must be human-readable, reviewable, scoped to the repository,
and safe to share with repository collaborators. A repo candidate is eligible
for the Git plane only after the proposal review boundary and, for OKF proposal
files, with `sensitivity: repo-safe`.

The following categories are excluded from canonical repo records:

- `secrets` (including credentials);
- `raw_chat_transcripts`;
- `private_personal_data`;
- `temporary_task_state`; and
- `local_only_state`.

Proposal sensitivity expresses these boundaries as `repo-safe`, `local-only`,
`sensitive`, `secret`, `raw-transcript`, `private-personal-data`,
`temporary-state`, or `unknown`. Omitted legacy DB/file values resolve to
`unknown`; only `repo-safe` can pass canonical apply.

Do not put these categories in `.memzoi/records/*.md` or a repo-shared pending proposal.
A blocked sensitivity is not made safe by auto-approval. Classify or sanitize
the candidate, or use `needs_review`; do not add an override that bypasses the
boundary.

Memzoi does **not** ingest raw transcripts. It does not inspect shell history,
chat logs, hidden agent state, or context packs. `memzoi checkpoint add` stores
only the explicit `--note` or `--from-file` content supplied by the caller as
runtime continuity. `memzoi session-end` accepts explicit structured input
(`task` plus `candidates`) from `--from-file` or a checkpoint; free-text notes
and free-text checkpoint bodies are not an extraction source. See
[OKF profile](./okf-profile.md) for the file-native proposal/record details.

## Authoritative read-time expiry

An active record with `expires`/`expires_at` stops participating in normal
memory reads when the evaluation clock reaches its expiry. The boundary is
inclusive: a record is expired when `now >= expires_at`. This rule is shared by
repo, local, and session search; context and handoff packs (including path-only
candidates); precheck; local lists; checkpoint list/show; and generated OKF,
`AGENTS.memory.md`, and `CLAUDE.memory.md` exports.

Expiry values use one of two exact forms:

- `YYYY-MM-DD` means `00:00:00Z` at the start of that date; or
- an RFC 3339 timestamp with `Z` or an explicit numeric UTC offset.

Offsets describe an instant, so `2026-07-10T14:00:00+02:00` expires at the
same moment as `2026-07-10T12:00:00Z`. Timestamps without a timezone and
invalid calendar values are rejected rather than treated as unexpired.
Production services evaluate against `SystemClock`; embedders and tests can
inject one clock through `MemoryService::open_with_clock` or
`MemoryService::open_paths_with_clock`, ensuring every surface in one service
uses the same instant.

Expiry is a read-time eligibility decision, not an implicit lifecycle write.
Memzoi leaves the indexed status and canonical `.memzoi/records/*.md` file
unchanged during search, rebuild, context, precheck, and export. Use
`memzoi expiry <record-id>` (or MCP `inspect_memory_expiry`) to retrieve the
record by ID, see the normalized effective instant, and explain why normal
reads excluded it. Any renewal or status transition remains a separately
reviewed canonical lifecycle action.

## Command boundary

The following is the authoritative boundary for current CLI behavior. A
command's JSON output, event, or database row does not change what it writes.

| Boundary | Commands | What is written (or not written) |
| --- | --- | --- |
| **Canonical Git record writers** | `memzoi apply <proposal-id>`; `memzoi proposals apply --all-approved` | Apply approved, explicitly `repo-safe` DB proposals and write canonical `.memzoi/records/*.md`. |
|  | `memzoi propose --apply --sensitivity repo-safe` | Create, validate, approve, and then explicitly apply one proposal. The flag supplies an `auto` per-call approval override and writes a canonical record only because `--apply` was requested; `--manual --apply` is invalid. Auto-approval cannot bypass sensitivity. |
|  | `memzoi proposal-files apply <proposal-id>` | Explicitly apply one valid repo-safe OKF proposal, update the runtime search index in the same operation, and move the packet from `pending/` to `resolved/applied/`. |
|  | `memzoi supersede <record-id> --sensitivity repo-safe`; `memzoi tombstone <record-id>` | Explicitly update an active, non-private repo record and its derived row as one staged transaction. Supersede replacements must remain in the target's scope and require an explicit repo-safe classification; local/session, private, and inactive targets are rejected before canonical writes. |
|  | `memzoi quickstart --apply-sample` | Explicitly creates the quickstart sample as a canonical repo record (and also generates an export). |
| **Pending file proposal writers** | `memzoi session-end --from-file <path>`; `memzoi session-end --from-checkpoint <id>` with a `repo` candidate | Write `.memzoi/proposals/pending/*.md` review packets. They do **not** write `.memzoi/records/*.md`; review and an explicit proposal-file apply are separate steps. |
| **DB proposal-state writers (not file/canonical writers)** | `memzoi propose`; `memzoi approve <proposal-id>`; `memzoi reject <proposal-id>` | Create or change proposal state in the runtime database. `propose` without `--apply` never writes a canonical record; approval alone never writes one. |
| **Runtime local/session writers** | `memzoi local add`; `memzoi checkpoint add`; `memzoi session-end ...` with `local` or `session` candidates | Write private runtime rows under the project runtime directory. Session candidates become checkpoints and require `type: episode` plus `lane: session`; neither route writes a Git record. |
| **No-write outcomes** | `discard` or `needs_review` candidates in `memzoi session-end` | Write neither a canonical record, pending proposal file, nor runtime memory row. `discard` is skipped; `needs_review` is blocked until a human decides. |
| **Operational runtime state** | `memzoi init`; `memzoi rebuild`; `memzoi export`; event recording used by normal operations | Initialize/update bundle directories including `.memzoi/` and `.memzoi/records/`, runtime SQLite/configuration, derived indexes, event rows, and generated files under the runtime project directory. These are operational or derived state, not canonical memory records. `rebuild` reads canonical Git records; it does not write them. |
| **Non-memory integration-file writes** | `memzoi integrate instructions [--file <path>]` | Update or create an agent instruction file such as `AGENTS.md` or `CLAUDE.md` (or the explicit file). This is an integration-file write, not a canonical memory or proposal write. `memzoi integrate prompt` and `integrate list` print information only. |

`memzoi export` writes generated projections (for example,
`AGENTS.memory.md`, `CLAUDE.memory.md`, or an `okf` export) under runtime
exports. Those projections are not canonical records. If a generated file is
copied into a repository instruction file, that copy is an explicit
integration/documentation change, not an implicit memory write.

## Approval, review, and promotion

The effective DB-proposal approval policy is resolved from the built-in
default (`auto`), then the user-global config, repo config, and a per-call CLI
override. Configure it as:

```toml
[workflow]
proposal_approval = "manual" # or "auto"
```

**Auto-approval is not application.** `auto` validates and approves a valid,
repo-safe
proposal; it does not write `.memzoi/records/*.md` by itself. Application is a
separate explicit operation (`apply`, `proposals apply --all-approved`,
`proposal-files apply`, or the explicit `propose --apply` shortcut). With
`manual`, proposals remain pending until an explicit approval and apply when no
per-call `auto` override is supplied (for example, a plain `propose` call).
`reject` closes a proposal without creating a canonical record.

The Git review rule is:

1. Classify a candidate as `repo` only when it is durable, repo-safe knowledge.
2. Create or receive the file-backed proposal (`.memzoi/proposals/pending/`).
3. Validate and review the proposal, including its sensitivity and scope.
4. Explicitly apply it to create/update `.memzoi/records/*.md`; successful
   apply also updates derived search state and archives the packet under
   `.memzoi/proposals/resolved/applied/`.

Apply keeps evidence provenance (`source`/`source_ref`) separate from review
lineage (`proposal_id`). Recall cites the original evidence; event and resolved
packet output identifies the proposal that authorized the canonical change.

To close a packet without a canonical write, run
`memzoi proposal-files reject <proposal-id> --reason "..."`. Rejection moves
a repo-safe packet to `.memzoi/proposals/resolved/rejected/` with the reviewer,
timestamp, outcome, and reason embedded in its frontmatter. A non-repo-safe
packet is replaced there by a create-shaped hash receipt so its original
content, scope, authorship, target, and lineage do not enter Git history.
Repeating an apply verifies the stored resolution against canonical bytes and
lineage, repairing only missing or stale disposable SQLite rows. Repeating a
rejection returns the stored resolution without writing again; attempting the
opposite outcome is refused.

File-backed `supersede` and `tombstone` actions must name exactly one target and
include a reason. Apply accepts only a repo-safe packet whose target still
exists, is active, matches the proposal's scope kind and scope ID, and was not
updated after `proposal.proposed_at`. Supersede retains the old evidence as a
`superseded` record and creates an active replacement with explicit lineage.
Tombstone retains the target evidence as a `tombstoned` record while the
resolved packet preserves the reason. Any validation, file, or index failure
reported by the command rolls back the target, replacement set, index, and
pending packet. A repo lifecycle lock prevents concurrent Memzoi writers, and
captured canonical hashes reject targets changed after validation. This is not
a claim of crash-atomicity across SQLite and multiple filesystem renames: after
process termination or power loss, run `memzoi doctor` and inspect hidden
transaction artifacts before retrying or repairing from canonical Git truth.

Runtime promotion follows the same boundary: local/session rows are not
directly promoted to canonical files. To promote an explicit durable finding,
provide a structured candidate to `memzoi session-end` with destination `repo`,
then review and explicitly apply the resulting pending proposal. There is no
automatic promotion, automatic classification, automatic scanning, or
automatic write from runtime state. A `needs_review` candidate stops before
any write and requires a human decision; a `discard` candidate is intentionally
lost.

MCP clients can propose and recall memory but cannot apply canonical records.
Use the CLI-side review/apply workflow for durable Git writes. See
[MCP and agent integration](./mcp-and-agent-integration.md) for that boundary.

## Runtime local and session continuity

Use the local namespace for explicit private runtime memory:

```bash
memzoi local add --type preference --title "..." --body "..."
memzoi local list
memzoi local search <query>
```

Use checkpoints for explicit task continuity:

```bash
memzoi checkpoint add --task "..." --note "..."
memzoi checkpoint add --task "..." --from-file notes.md
memzoi checkpoint list
```

These rows remain in the runtime project database, are not included in global
repo search or exports by default, and are included in context/handoff only
with explicit `--include-local` or `--include-session`. Their recall and
precheck citations carry `provenance: runtime`; Git records carry
`provenance: git`. A runtime row never becomes shared repo truth merely
because it was recalled, exported, or included in a context pack.

## Explicit session-end routing

`session-end` is a routing operation over explicit structured candidates, not
an extractor:

```yaml
task: "Implement auth middleware"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Protected routes validate sessions server-side
    body: Protected routes must validate sessions server-side.
    sensitivity: repo-safe
```

For each candidate, the current behavior is deterministic:

- `repo` writes a pending OKF proposal file and never directly writes a
  canonical record;
- `local` writes a private runtime-local row;
- `session` writes a runtime checkpoint (with `type: episode` and
  `lane: session`);
- `discard` writes nothing and reports `skipped`; and
- `needs_review` writes nothing and reports `blocked`.

Omitted sensitivity normalizes to `unknown`. If any `repo` candidate is not
`repo-safe`, its content is replaced by a classification-only blocked result and
the whole session-end batch performs no writes.

Promotion is transactional across the session-end operation: if a runtime
write or proposal-file write fails, created proposal files are cleaned up. A
successful `repo` route still requires the separate review/apply step above.

## Import planning

Import is a strict, compact manifest workflow, not an extractor or automatic
classifier. The caller supplies a manifest with the exact version
`memzoi/import-v1`, explicit candidates, and provenance sources. It does not
parse `AGENTS.md`, `CLAUDE.md`, Cursor files, ADRs, chats, or other ambient
project state; it does not infer candidates from those sources.

Review the mutation-free, deterministic plan before applying it:

```bash
memzoi import plan --from-file <manifest.yml> [--actor cli] [--json]
memzoi import apply --from-file <manifest.yml> --plan-id <import_…> [--actor cli] [--json]
```

`plan` reports the plan identity and candidate outcomes without writing. `apply`
recomputes that identity before writing, creates valid pending file-backed proposals
for `repo` candidates, and writes `local`/`session` candidates into private runtime
state. The destination outcomes are:

- `repo`: create a pending proposal for later review; it is not canonical yet;
- `local`: create a private active local runtime record;
- `session`: create a private session checkpoint;
- `discard`: no write; and
- `needs_review`: blocked, with no write until a human decides.

Only `repo-safe` repo candidates receive `create_proposal`; omitted sensitivity
normalizes to `unknown`, and other classifications receive a redacted `blocked`
action. Import plans are explicitly per-candidate, so applying a reviewed plan may
write its allowed candidates while the blocked candidate remains a no-write. If a
manifest contains any blocked repo candidate, its document-wide source locators are
omitted from the plan and from allowed proposal files because they cannot be safely
attributed to only the allowed subset.

After reviewing an imported repo proposal, use the separate explicit proposal
review/apply workflow described in [Approval, review, and promotion](#approval-review-and-promotion),
including `memzoi proposal-files apply <proposal-id>` to create the canonical
`.memzoi/records/*.md` record and update derived runtime search state in the
same operation. A plan may contain local or private candidates and must not be
blindly committed. Duplicate checks compare trimmed-body BLAKE3 values against
canonical records, pending proposals, active runtime memory, and earlier input
candidates. Treat manifests and plan output as potentially sensitive; the
Git-plane sharing and exclusion rules remain those in [The two planes](#the-two-planes)
and [Git-plane responsibilities and exclusions](#git-plane-responsibilities-and-exclusions).

## MVP scope and non-goals

The current MVP includes the two explicit planes, five current destinations,
file-backed repo proposals, canonical record apply/lifecycle commands, local
runtime records, session checkpoints, structured session-end routing, and
recall/precheck provenance reporting.

The two-plane policy does **not** include:

- `team` or `cloud` destinations, hosted storage, or runtime sync;
- automatic classification, scanning, extraction, promotion, or writes;
- raw transcript/chat-log ingestion;
- making SQLite or any runtime state canonical for repo memory;
- vector recall as part of this policy; or
- an MCP capability to apply canonical records.

Do not add a new destination, plane, route, or repository exclusion in docs
without changing the executable policy contract first.
