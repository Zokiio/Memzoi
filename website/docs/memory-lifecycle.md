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
| **Runtime** | Fast local recall, private preferences, task continuity, checkpoints, proposals, and derived operational state. | `${MEMZOI_HOME:-~/.memzoi}/projects/<repository-key>/shared.db` is the local authority for runtime memory and proposal state shared by linked worktrees. Each `worktrees/<worktree-key>/index.db` is a disposable projection. Runtime state is not Git truth. |

Transitional proposal commands remain present in the current implementation and
may still create a pending file-backed proposal at
`.memzoi/proposals/pending/<proposal-id>.md`. They are not a pre-1.0
compatibility commitment and may be changed or removed without an adapter.
That packet is a review artifact, not a canonical record. New direct repository
materialization does not require or create a proposal packet: an explicitly
authorized structured candidate becomes an ordinary working-tree change under
`.memzoi/records/`.

Runtime rows are not a second canonical source for repo memory. Rebuild reads
the Git records and recreates only the current worktree's derived index while
preserving repository-shared local/session rows and proposals in `shared.db`.
It does not promote runtime rows into Git memory. See
[Exports and files](./exports-and-files.md) for file layout and commit guidance.

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
| `repo` | `git` | `file_backed_proposal` for transitional commands currently present; `materialization` for an explicit structured candidate | `proposal_review` for transitional commands; a pinned explicit decision for direct materialization | Repo-shared durable knowledge. The transitional route writes a pending proposal; direct materialization writes one reviewed canonical working-tree change. |
| `local` | `runtime` | `runtime_local` | `no_review` | Private local runtime memory; never a repo record by this route. |
| `session` | `runtime` | `runtime_session` | `no_review` | Task continuity/checkpoint state; never a repo record by this route. |
| `discard` | none | `no_write` | `no_review` | Do not retain the candidate. |
| `needs_review` | none | `no_write` | `human_decision` | Do not write it yet; a human must decide the sharing boundary. |

`MemoryDestination::policy()` describes the current destination-routed flow and
therefore still reports `file_backed_proposal` / `proposal_review` for `repo`.
That current behavior is not a pre-1.0 compatibility guarantee and does not
grant direct write authority. Direct materialization is a separate, explicitly
authorized structured-candidate contract described in
[Git-native materialization and Git review](#git-native-materialization-and-git-review).

`team` and `cloud` are future-only labels. They are not accepted
`MemoryDestination` values, do not have a current plane or write route, and
must not be presented as available destinations. There is no team runtime
plane, hosted storage, or sync implementation in this MVP.

## Git-plane responsibilities and exclusions

Git-plane memory must be human-readable, reviewable, scoped to the repository,
and safe to share with repository collaborators. A direct candidate reaches the
Git plane only after immutable planning, an explicit decision, a current-target
check, and the shared repository-write safety gate. It must have repository
scope and visibility, `sensitivity: repo-safe`, and
`content_class: general_repo_knowledge`. Transitional proposal adapters keep
their currently documented review path while present, without creating a
forward-compatibility commitment.

The following categories are excluded from canonical repo records:

- `secrets` (including credentials);
- `raw_chat_transcripts`;
- `private_personal_data`;
- `temporary_task_state`; and
- `local_only_state`.

Proposal sensitivity expresses these boundaries as `repo-safe`, `local-only`,
`sensitive`, `secret`, `raw-transcript`, `private-personal-data`,
`temporary-state`, or `unknown`. Current-format proposal files require an
explicit sensitivity and content class. Interactive proposal inputs may
represent an omitted sensitivity as `unknown`; only an explicit `repo-safe`
classification can pass canonical apply.

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

## Retention and current assertions

Every current-format record carries required typed `retention` facts. Retention is
the lane-specific temporal decision only; it returns `current` or `query_only`
plus an effective boundary and reason. The complete ordinary-use decision is
the **current assertion**:

```text
active lifecycle status
AND retention is current
AND applicability is valid
AND no conflict suppression
AND no quarantine or safety suppression
```

Search, context, handoff, precheck, exports, local/checkpoint reads, duplicate
analysis, and conflict analysis all use that shared boundary before ranking or
limiting. A temporally current record can therefore still be excluded by
another current-assertion dimension. `include_inactive` does not grant history
access.

The current retention policy applies these boundaries:

- session: the earliest of closure, 24 hours after the latest continuation or
  start, seven days after the original start, and explicit expiry;
- episodic: 30 days after occurrence, shortened by explicit expiry; an exact
  owner-authorized private lifecycle action may extend automatic recall, but
  never later than 90 days after occurrence and without changing validity or
  physical retention; and
- semantic and procedural: no age TTL, while explicit expiry still applies.

The exact boundary is `query_only`. All timestamps are RFC 3339 instants with
`Z` or an explicit numeric UTC offset. Invalid facts fail with an error naming
the record; they are never treated as an ordinary non-current result.

Retention evaluation is not an implicit lifecycle write. Memzoi leaves the
record and canonical `.memzoi/records/*.md` file unchanged during recall. Use
`memzoi expiry <record-id>` (or MCP `inspect_memory_expiry`) for ordinary
repository-record expiry inspection. Only explicit local CLI
`memzoi lifecycle inspect record <record-id>` may retrieve quarantined or
superseded private history. Renewal and status transitions remain explicit
lifecycle actions.

## Command boundary

The following is the authoritative boundary for current CLI behavior. A
command's JSON output, event, or database row does not change what it writes.

| Boundary | Commands | What is written (or not written) |
| --- | --- | --- |
| **Git-native materialization** | `memzoi materialize plan`; `memzoi materialize decide` | Parse and validate a strict structured candidate, then produce a deterministic plan or explicit decision. These commands do not write `.memzoi/records/`, runtime state, proposal packets, or Git state. An optional caller-selected artifact path is outside `.memzoi`. |
|  | `memzoi materialize apply` | Revalidates the candidate, plan, decision, exact supplied identities, current target revision, Git visibility, and the shared safety gate; then atomically creates or updates one `.memzoi/records/*.md` working-tree file. It does not stage, commit, push, open a pull request, merge, switch branches, or change Git configuration. |
| **Canonical Git record writers — transitional current behavior** | `memzoi apply <proposal-id>`; `memzoi proposals apply --all-approved` | Apply approved, explicitly `repo-safe` DB proposals and write canonical `.memzoi/records/*.md`. These commands are not a pre-1.0 compatibility commitment. |
|  | `memzoi propose --apply --sensitivity repo-safe` | Create, validate, approve, and then explicitly apply one proposal. The flag supplies an `auto` per-call approval override and writes a canonical record only because `--apply` was requested; `--manual --apply` is invalid. Auto-approval cannot bypass sensitivity. |
|  | `memzoi proposal-files apply <proposal-id>` | Explicitly apply one valid repo-safe OKF proposal, update the runtime search index in the same operation, and move the packet from `pending/` to `resolved/applied/`. |
|  | `memzoi supersede <record-id> --sensitivity repo-safe`; `memzoi tombstone <record-id>` | Explicitly update an active, non-private repo record and its derived row as one staged transaction. Supersede replacements must remain in the target's scope and require an explicit repo-safe classification; local/session, private, and inactive targets are rejected before canonical writes. |
|  | `memzoi quickstart --apply-sample` | Explicitly creates the quickstart sample as a canonical repo record (and also generates an export). |
| **Pending file proposal writers — transitional current behavior** | `memzoi session-end --from-file <path>`; `memzoi session-end --from-checkpoint <id>` with a `repo` candidate | Write `.memzoi/proposals/pending/*.md` review packets. They do **not** write `.memzoi/records/*.md`; review and an explicit proposal-file apply are separate steps. These commands may be changed or removed before 1.0 without an adapter. |
|  | `memzoi capture apply ...` with an accepted or edited `repo`/`repo-safe` candidate | Write a pending evidence-backed proposal packet after validating pinned plan/review identities and current preconditions. Capture apply never writes the candidate directly to `.memzoi/records/*.md`. |
| **DB proposal-state writers (not file/canonical writers)** | `memzoi propose`; `memzoi approve <proposal-id>`; `memzoi reject <proposal-id>` | Create or change proposal state in the runtime database. `propose` without `--apply` never writes a canonical record; approval alone never writes one. |
| **Runtime local/session writers** | `memzoi local add`; `memzoi checkpoint add`; `memzoi session-end ...` with `local` or `session` candidates | Write private runtime rows under the project runtime directory. Session candidates become checkpoints and require `type: episode` plus `lane: session`; neither route writes a Git record. |
|  | `memzoi capture apply ...` with an accepted or edited `local`/`session` candidate | Write a private runtime record with capture evidence and review provenance. It is never routed through a repo proposal by this command. |
| **Private lifecycle evidence and inspection** | `memzoi lifecycle plan`; `memzoi lifecycle inspect record`; `memzoi lifecycle inspect grant` | Strictly read-only. Private plan output is content-free and may be installed only at a caller-selected path outside the Git worktree and all Memzoi-managed runtime directories. Explicit record inspection is the only route that may return quarantined or superseded private history. |
| **Private lifecycle authority** | `memzoi lifecycle authorize` | Validate the exact request, optional source plan, policy, scope, lane, and version bindings, then create only one `owner_action_grant` row. It performs no private-record or lifecycle change. |
|  | `memzoi lifecycle revoke` | Update only `owner_action_grant`; it never rewrites private records, lifecycle state, relations, receipts, or audit events. |
| **Private lifecycle execution** | `memzoi lifecycle apply` | Atomically consume exactly one active grant and apply its complete action group. This is the only command that mutates private records, lifecycle state or relations, ordinary-read eligibility, lifecycle application receipts, or lifecycle audit events. Any failed revalidation rolls back the complete group and leaves the grant unconsumed. |
| **Derived private recall suppression** | `memzoi lifecycle maintenance enable`; `disable`; `inspect`; `reconcile` | Manage a separate standing owner opt-in that maintains a rebuildable, record-token-matched suppression projection for accepted high-confidence unresolved contradictions. It never selects a winner or mutates canonical/private record content. |
| **No-write outcomes** | `discard` or `needs_review` candidates in `memzoi session-end` | Write neither a canonical record, pending proposal file, nor runtime memory row. `discard` is skipped; `needs_review` is blocked until a human decides. |
|  | `memzoi capture plan`; `memzoi capture review`; MCP `plan_capture` | Do not write memory state. CLI `--output` may write a classified plan/review artifact to an allowed caller-selected path outside `.memzoi`, private runtime state, and generated exports; MCP never writes an artifact. |
|  | `memzoi maintenance plan`; MCP `plan_maintenance` | Read canonical repository records directly and emit an immutable report/candidate artifact. They do not open a writable service or mutate records, proposals, indexes, overlays, private runtime, WAL/event logs, or Git. CLI `--output` is the sole optional write and must be outside the Git worktree and all managed runtime roots; MCP never writes an artifact. |
|  | Rejected, deferred, duplicate, conflicting, blocked, or unresolved capture candidates | Write no proposal, canonical record, or runtime record. A stale source, inventory, policy, plan ID, or review ID also fails before writes. |
| **Operational runtime state** | `memzoi init`; `memzoi rebuild`; `memzoi export`; event recording used by normal operations | Initialize/update bundle directories including `.memzoi/` and `.memzoi/records/`, runtime SQLite/configuration, derived indexes, event rows, and generated files under the runtime project directory. These are operational or derived state, not canonical memory records. `rebuild` reads canonical Git records; it does not write them. |
| **Non-memory integration-file writes** | `memzoi integrate instructions [--file <path>]` | Update or create an agent instruction file such as `AGENTS.md` or `CLAUDE.md` (or the explicit file). This is an integration-file write, not a canonical memory or proposal write. `memzoi integrate prompt` and `integrate list` print information only. |

`memzoi export` writes generated projections (for example,
`AGENTS.memory.md`, `CLAUDE.memory.md`, or an `okf` export) under runtime
exports. Those projections are not canonical records. If a generated file is
copied into a repository instruction file, that copy is an explicit
integration/documentation change, not an implicit memory write.


## Immutable maintenance evidence

`memzoi maintenance plan` evaluates the repository plane from a stable
canonical snapshot. The plan binds its evaluation and expiry times, selected
targets and comparison neighbourhood, record revisions, detector and policy
versions, evidence digests, findings, action candidates, and stale
preconditions. Replaying the same request against the same snapshot, policy,
and time produces the same semantics and identities.

A plan is a decision artifact, not permission to change memory. Exact duplicate
candidates remain distinct from contradictions; unresolved contradictions name
the full set and choose no winner. Staleness and expiry are report-only, and a
renewal candidate never extends or otherwise changes its predecessor's
retention.

The current contract is `maintenance-plan/2`; the stable artifact identifiers
remain `memzoi/maintenance-request` and `memzoi/maintenance-plan`. Repository
CLI and MCP planning remain repository-only and read-only. The separate local
`memzoi lifecycle plan` command uses the same current contract over private runtime
snapshots, with opaque IDs and random version tokens instead of bodies or
titles. Neither detector selects a duplicate keeper or contradiction winner;
only an exact owner request may make that choice.

Memzoi is pre-1.0 and current-schema-only. Maintenance artifacts and SQLite
databases that do not match the current schema are rejected before any write.
There are no compatibility
readers, schema aliases, fallbacks, or automatic migrations; manually upgrade
or remove incompatible runtime databases and artifacts before retrying.

## Standing private maintenance authority

`memzoi lifecycle maintenance enable` creates a standing opt-in distinct from
the one-shot, time-bounded `owner_action_grant`. Enablement becomes visible only
when its first projection is `current`; disablement atomically revokes the opt-in
and clears derived suppression. Reconciliation uses base eligibility (lifecycle,
retention, validity, quarantine, applicability, and safety) and ordinary recall
uses effective eligibility (base plus the current conflict projection), so the
overlay cannot erase its own contradiction evidence.
Private contradiction applicability includes the persisted path bindings:
disjoint path scopes are not contradictory, nested scopes overlap, and an empty
path set is global.

The projection states are `disabled`, `current`, `dirty`, and `blocked`. A
successful zero-edge evaluation publishes a current empty projection. Stale
snapshots are retried rather than published. Detector, policy, bounds, or
validation failure retains prior edges, records a content-free blocked reason,
and fails automatic private recall closed until an explicit reconciliation
publishes a current projection. Explicit record inspection reports both base and
effective eligibility plus content-free conflict participation.
Every current projection persists the source plan's `not_after`. At that
boundary inspection reports `stale`, immutable recall fails closed, and an
audited private read reconciles under the lifecycle lock before returning.

## Exact owner-authorized private lifecycle

Private lifecycle changes follow one explicit authority chain:

```text
plan = evidence
request = exact owner intent
grant = temporary one-shot authority
apply = revalidation + atomic execution
result = idempotent receipt
```

`memzoi lifecycle plan` is optional evidence. It is deterministic for a fixed
snapshot and evaluation time, contains no private body or title, and never
chooses a duplicate keeper or contradiction winner. The owner supplies a
strict `memzoi/private-lifecycle-request` with a caller-controlled
`operation_id`, recomputable `request_id`, direct or selected-plan source, one
to 64 tagged actions, and exact random version tokens for every target and
evidence record. A request may touch at most 256 distinct mutation targets,
and a record may not participate in overlapping actions.

`authorize` captures its own system-clock instant. An optional requested
expiry may only shorten authority; a grant expires at the earliest of 24 hours
after authorization, the requested expiry, and the source plan's `not_after`.
An identical active grant is reused. The stored `owner_action_grant` row—not
printed JSON—is authoritative for active, revoked, consumed, and expired
state. `revoke` is idempotent for already-revoked or consumed grants.

`apply` holds the lifecycle lock and uses one shared SQLite transaction. It
checks operation replay or conflict, the authoritative grant, exact request and
plan binding, policy and lane rules, the complete comparison neighbourhood,
and every target/evidence version before changing anything. A successful
transaction applies the whole action group, writes one content-free event and
one replayable receipt, consumes the grant, and advances the shared lifecycle
generation. Any stale, unauthorized, expired, revoked, overlapping, or
partially invalid request performs zero lifecycle writes and leaves the grant
unconsumed. Reusing an `operation_id` with the same `request_id` and recorded
`grant_id` returns the recorded result. Supplying another grant is a zero-write
replay mismatch; reusing the operation with another request is a zero-write
operation-ID conflict.

Private lifecycle artifact I/O is deliberately fail-closed on unsupported
platforms in this release. `lifecycle authorize`, `lifecycle apply`, and
`lifecycle plan --output` currently require Unix regular-file, no-symlink, and
atomic no-clobber primitives. On non-Unix systems, `lifecycle plan` without
`--output`, both `lifecycle inspect` commands, and `lifecycle revoke` remain
available.

The action rules preserve independent clocks and owner choice:

- automatic episodic recall may extend only through 90 days after occurrence;
  validity extension requires a later existing finite boundary and rejects
  session records; `retain_until` may only increase;
- indefinite pinning accepts episodic, semantic, and procedural records but
  not session records; unpinning preserves any finite retention boundary;
- renewal uses distinct fresh accepted evidence to promote that evidence and
  supersede an expired semantic/procedural predecessor, without creating a
  third record;
- correction creates a classification-preserving replacement; supersession
  requires compatible existing records;
- consolidation and contradiction resolution revalidate the complete member
  set and preserve the owner's explicit keeper or winner; and
- quarantine preserves content and history while excluding the record from
  ordinary reads; release removes only quarantine.

`shared.db` is authoritative and every worktree mirror records the lifecycle
generation it reflects. Mirror-backed reads compare generations before serving
results, refresh on mismatch, and fail with a mirror-refresh-required error if
convergence cannot be completed. Exact apply replay also retries convergence,
so a committed quarantine or eligibility change is never served from a stale
mirror.

Lifecycle audit events are deliberately content-free: they may contain only
random grant/application IDs, operation IDs, action kinds, opaque target IDs,
and timestamps. They never contain private bodies or titles, evidence text or
IDs, provenance payloads, request digests, content hashes, or content-derived
versions. Private planning, authority, inspection, and execution are local
CLI/core only; MCP exposes repository-only `plan_maintenance` and no private
lifecycle tool.


## Git-native materialization and Git review

Direct materialization consumes one complete
`memzoi/repository-materialization-candidate` JSON file. The candidate is
the caller's explicit structured input; it carries the typed canonical content,
classification, provenance, action, and current-record precondition. It is not
an inbox packet, and planning does not retain it under `.memzoi`.

```bash
# Both artifacts are explicit, user-owned files outside .memzoi.
memzoi materialize plan \
  --candidate-file materialization-candidate.json \
  --output materialization-plan.json

memzoi materialize decide \
  --candidate-file materialization-candidate.json \
  --plan-file materialization-plan.json \
  --decision-at 2026-07-16T12:00:00Z \
  --output materialization-decision.json

# Review the candidate, plan, and decision before authorizing a filesystem write.
${EDITOR:-vi} materialization-candidate.json
${EDITOR:-vi} materialization-plan.json
${EDITOR:-vi} materialization-decision.json

memzoi materialize apply \
  --candidate-file materialization-candidate.json \
  --plan-file materialization-plan.json \
  --decision-file materialization-decision.json \
  --candidate-id "$(jq -r .candidate_id materialization-candidate.json)" \
  --plan-id "$(jq -r .plan_id materialization-plan.json)" \
  --decision-id "$(jq -r .decision_id materialization-decision.json)"
```

`apply` creates an **unstaged** canonical file or updates an existing canonical
file. It reports each repository-relative path, the action, record ID,
semantic revision, and an action-aware narrow review command. For a tracked
path, inspect the ordinary working-tree diff:

```bash
git diff -- .memzoi/records/<record-id>.md
```

For an untracked create, use a no-index diff so the new file is displayed:

```bash
git diff --no-index -- /dev/null .memzoi/records/<record-id>.md
```

`git diff --no-index` exits with status 1 when it finds a difference; that is
the expected review result. Git—not Memzoi—remains responsible for acceptance:

```bash
# Revise the ordinary working-tree change, then rebuild its disposable index.
${EDITOR:-vi} .memzoi/records/<record-id>.md
memzoi rebuild

# Discard an untracked create, or restore a tracked path from HEAD.
rm .memzoi/records/<record-id>.md
git restore --worktree -- .memzoi/records/<record-id>.md
memzoi rebuild

# Stage, commit, push, and open/review a pull request using normal project tools.
git add -- .memzoi/records/<record-id>.md
git commit -m "docs(memory): record <record-id>"
git push
gh pr create

# Roll back a committed memory change through Git, then rebuild the local index.
git revert <commit>
memzoi rebuild
```

Choose either `rm` for an untracked create or `git restore` for an existing
tracked path; do not run both for the same path. Manual editing or deletion
does not retain a hidden Memzoi approval: admission validates the current
working-tree bytes on the next rebuild/read. An invalid, unsafe, ignored
untracked, or stale-attested record is excluded with diagnostics instead of
being silently repaired or restored.

## Transitional proposal approval

These commands document current executable behavior. They are not a pre-1.0
compatibility route: existing artifacts must satisfy the current schema, and
the commands or their formats may be changed or removed without an adapter.

The effective DB-proposal approval policy is resolved from the built-in default
(`auto`), then the user-global config, repo config, and a per-call CLI override.
Configure it as:

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
`reject` closes a proposal without creating a canonical record. Terminal DB
proposal states (`applied` and `rejected`) cannot be reopened; repeated
same-state approval or rejection requests are idempotent.

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
packet is sensitivity-preflighted before full parsing and replaced there by a
create-shaped hash receipt, even when its other fields are malformed. Its
original content, scope, authorship, target, lineage, proposal ID, and file ID
do not enter Git history or command output. The receipt frontmatter and filename
use deterministic hash-only identities, while replay by either original alias
hashes the lookup and finds the receipt without echoing that alias.
Repeating an apply verifies create/replacement bytes plus lifecycle status,
scope, and lineage while treating current target bytes as canonical truth. It
repairs missing or stale relational rows and full-text index drift
transactionally. Repeating a rejection returns the stored resolution without
writing again; attempting the opposite outcome is refused.

List, show, validate, apply, reject, replay, and doctor share one contained
proposal inventory. The inventory refuses symlinked proposal roots, enforces
unique file and packet identities across pending and resolved directories, and
treats an applied or rejected identity as terminal. Session-end and import
proposal creation reserve those same global identities while holding the repo
lifecycle lock, so they cannot recreate an already resolved packet under a new
filename.

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
process termination or power loss, run `memzoi doctor`. It warns about full-text
index drift and hidden transaction artifacts without exposing unsafe artifact
identities. Reported cleanup and rollback failures remain errors rather than
being presented as successful resolutions; inspect the Git-safe roots before
retrying or repairing from canonical Git truth.

Runtime promotion follows the same boundary: local/session rows are not
directly promoted to canonical files. To promote an explicit durable finding,
provide a structured candidate to `memzoi session-end` with destination `repo`,
then review and explicitly apply the resulting pending proposal. There is no
automatic promotion, automatic classification, automatic scanning, or
automatic write from runtime state. A `needs_review` candidate stops before
any write and requires a human decision; a `discard` candidate is intentionally
lost.

MCP clients can recall repository memory and create read-only capture and
repository-maintenance plans, but they cannot create proposal state or apply
canonical records. Proposal creation, review, and apply remain local CLI/core
workflows for durable Git writes. See
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
memzoi checkpoint continue <checkpoint-id>
memzoi checkpoint close <checkpoint-id>
memzoi checkpoint add --task "..." --note "..." --successor-of <checkpoint-id>
memzoi checkpoint list
```

These rows remain in the runtime project database, are not included in global
repo search or exports by default, and are included in context/handoff only
with explicit `--include-local` or `--include-session`. Their recall and
precheck citations carry `provenance: runtime`; Git records carry
`provenance: git`. A runtime row never becomes shared repo truth merely
because it was recalled, exported, or included in a context pack.

A continuation is accepted only while the checkpoint is open and current;
the exact retention boundary is too late. Closure is terminal and idempotent.
A closed or expired checkpoint can be named as `--successor-of` to create a
new session generation with explicit predecessor lineage. Machine-oriented
JSON calls require caller-controlled `--operation-id` and observed
`--expected-version`; an identical retry replays its recorded outcome before
version checks, while changed parameters with the same operation ID fail.

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
`memzoi/import`, a required source-event `origin_key`, explicit candidates, and provenance sources. It does not
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
action. If any repo candidate is non-repo-safe, every repo candidate in that manifest
is blocked with guidance to split the manifest before retrying. No proposal file or
canonical repo record is written from that repo subset. Local and session candidates
may still write private runtime records. The document-wide source locators are omitted
from the plan because they cannot be safely attributed to a partial destination subset.

After reviewing an imported repo proposal, use the separate explicit proposal
review/apply workflow described in [Transitional proposal approval](#transitional-proposal-approval),
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
