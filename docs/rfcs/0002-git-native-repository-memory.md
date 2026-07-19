# RFC 0002: Git-native repository memory

- Status: Accepted
- Accepted: 2026-07-13
- Target: v0.6 — Memory Quality and Lifecycle
- Tracking issue: [#99](https://github.com/Zokiio/Memzoi/issues/99)
- Supersedes: Repository-write and canonical-apply portions of
  [RFC 0001](0001-evidence-backed-capture.md)

## Summary

Memzoi will use ordinary Git-visible files as the normal review mechanism for
shared repository memory.

After mutation-free planning, explicit authorization, stale-input validation,
duplicate and conflict analysis, and one shared repository-write safety
decision, an explicit CLI operation may materialize a repository-safe
candidate as an unstaged change beneath `.memzoi/records/`. A separate proposal
inbox is no longer required for the normal repository route.

An admitted working-tree record is locally active. Staging has no Memzoi
semantic effect: local recall always uses the working-tree revision, including
when it differs from the Git index. Memzoi may report whether the exact revision
is committed at `HEAD` or present at a configured repository baseline ref, but
it does not infer human or organizational acceptance from Git state.

Memzoi-originated writes and existing working-tree files cross different trust
boundaries. Every Memzoi repository write must pass the common write gate.
Every file, including a manually or externally authored file, must separately
pass read-side repository-record admission before it participates in indexing,
recall, context, precheck, or export. Admission is not proof that an external
writer previously obtained Memzoi authorization.

Memzoi never stages, commits, pushes, opens or merges pull requests, switches
branches, changes Git configuration, or implicitly promotes personal or
session memory into the repository. MCP receives no Git-native
repository-materialization authority. Earlier pre-1.0 MCP proposal behavior
has been removed; the current MCP surface is repository-only and read-only.

Pre-1.0 proposal packets must be manually upgraded to a current supported
format or removed. They are not required for normal repository writes and
never become active memory themselves.

## Context

The existing repository route uses two review systems:

```text
candidate
  -> pending proposal
  -> Memzoi review and canonical apply
  -> working-tree record
  -> Git review
```

The proposal boundary is explicit, but it duplicates the Git workflow where
developers already inspect, edit, reject, commit, and review project changes.
The desired normal route is:

```text
explicit evidence or structured candidate
  -> mutation-free plan
  -> explicit decision
  -> destination, provenance, stale-input, and safety validation
  -> unstaged .memzoi/records change
  -> ordinary Git review
```

This RFC changes repository-write authority and acceptance vocabulary. It does
not weaken explicit evidence, classification, provenance, stale-source
protection, duplicate and conflict handling, private-memory isolation, or
recovery.

The current implementation also makes two constraints explicit:

- canonical records have a stable record ID and a separate content hash; and
- repository search and precheck use derived SQLite/FTS state.

The new contract must therefore distinguish logical identity from revision
identity and must define how each read surface proves that its projection
matches the current checkout.

## Goals

1. Make ordinary Git-visible changes the primary review mechanism for
   repository memory.
2. Remove the required Memzoi-specific inbox from the normal repository route.
3. Define local activity independently from staging and social approval.
4. Keep planning mutation-free and materialization explicitly authorized.
5. Put every Memzoi repository-writing route behind one versioned, fail-closed
   safety boundary.
6. Admit all repository records through a separate read-side validation
   boundary.
7. Preserve stable record identity while binding evidence and authorization to
   exact revisions.
8. Ensure recall and precheck never silently use a stale checkout projection.
9. Preserve evidence, provenance, lifecycle rationale, duplicate and conflict
   decisions, and stale-input protection in Git-rebuildable form.
10. Keep local, personal, session, unknown-sensitivity, and prohibited content
    out of Memzoi-originated repository writes.
11. Require pre-1.0 records and proposal artifacts to meet the current schema
    or be removed; never auto-classify or silently migrate them.
12. Keep runtime indexes disposable and deny Memzoi Git-operation authority.

## Non-goals

- Automatically staging, committing, pushing, opening pull requests, merging,
  rebasing, switching branches, or changing Git configuration.
- Treating staging, a commit, or baseline reachability as proof of human or
  organizational approval.
- Preventing editors, scripts, users, or other programs from writing arbitrary
  bytes beneath the repository root.
- Treating a file as safe merely because it is under `.memzoi/records/`,
  untracked, unstaged, or ignored.
- Giving every agent-facing surface repository-write authority.
- Replacing explicit evidence, review, import planning, or classification with
  Git review alone.
- Automatically promoting local, personal, or session memory into repository
  memory.
- Making SQLite, FTS, vector indexes, or any other runtime projection
  authoritative.
- Automatically repairing arbitrary malformed or semantically inconsistent
  manual edits.
- Providing automatic legacy migration or fallback admission for pre-1.0
  artifacts.
- Defining cryptographic author identity or approval signatures.

## Terminology and identities

- **Plan**: A deterministic, mutation-free description of intended actions and
  their preconditions.
- **Candidate**: A typed proposed memory with destination, scope, sensitivity,
  provenance, evidence, and action metadata.
- **Decision**: The explicit, immutable authorization to materialize selected
  candidate revisions under pinned metadata and repository preconditions.
- **Materialization**: A Memzoi operation that creates or changes a canonical
  repository record in the working tree after a decision passes the write gate.
- **Repository-write gate**: The common fail-closed authorization, safety, and
  policy decision applied before Memzoi writes candidate content anywhere
  beneath the repository root.
- **Repository-record admission**: Read-side validation applied before a file
  participates in any repository-memory read surface.
- **Canonical record**: A valid typed record beneath `.memzoi/records/`.
  Canonical describes the file format and authority within the active checkout;
  it does not imply approval.
- **Locally active**: Admitted and therefore eligible for local recall and
  indexing in the current checkout.
- **Baseline ref**: A locally resolvable configured Git ref used only to report
  whether an exact record revision is present in an integration baseline.
- **Derived projection**: Disposable SQLite, FTS, vector, cache, or export state
  rebuilt from admitted repository records and permitted private stores.
- **Legacy proposal**: A pending or resolved proposal created under the
  pre-RFC-0002 workflow.

The identity model is normative:

| Identity | Meaning |
| --- | --- |
| `record_id` / concept ID | Stable logical identity across valid revisions |
| `revision_hash` | Identity of one exact canonical semantic revision under a versioned hash contract |
| `plan_id` | Immutable planning identity |
| `candidate_id` | Immutable candidate and classification identity |
| `decision_id` | Immutable materialization authorization identity |

A semantic edit may retain `record_id`, but it creates a new `revision_hash`.
Evidence, capture lineage, review metadata, and materialization authorization
must not attest to changed semantic content unless their versioned identities
validate against that exact revision.

## Decision

### 1. Working-tree authority is local, not social

For the active checkout, admitted records beneath `.memzoi/records/` are the
authoritative input to local repository-memory reads.

Git state has the following meaning:

| State of the exact working-tree revision | Memzoi meaning |
| --- | --- |
| Planned but not materialized | Proposed action only; not repository memory |
| Admitted untracked or unstaged revision | Locally active working-tree change |
| Admitted staged revision | Same local meaning; staging adds no approval |
| Admitted revision identical to the path at `HEAD` | Locally active and committed at `HEAD` |
| Exact revision present at the configured baseline ref | Locally active and baseline-backed |
| Working-tree deletion | Locally inactive in this checkout |
| Invalid, unsafe, or conflicted file | Excluded with bounded diagnostics |

Local recall always uses admitted working-tree bytes. It never substitutes the
Git index version merely because a path is staged. If the index and working tree
contain different revisions, Memzoi may report both states, but only the
working-tree revision is locally active.

The default branch is the recommended baseline when it can be resolved from
local configuration. A repository may configure another ref. Memzoi does not
guess when no baseline can be resolved, does not require a remote, and reports
baseline state as unknown rather than performing network discovery or inferring
acceptance.

Baseline reporting compares the exact canonical path and revision represented
by the baseline tree. Historical presence elsewhere in the commit graph does
not make a deleted or superseded record baseline-backed.

Integration into a default or project-configured baseline normally represents
the repository's shared review process, but Memzoi reports only verifiable Git
facts. It does not enforce branch protection, review counts, merge policy, or
hosting-provider configuration.

### 2. Memzoi writes and filesystem admission are separate boundaries

The repository-write gate governs every repository write performed by Memzoi.
It cannot prevent another program or a user from creating or editing files
beneath `.memzoi/records/`.

Before a repository record participates in indexing, recall, context,
precheck, handoff, or export, it must pass repository-record admission covering
at minimum:

- safe canonical path resolution and containment;
- regular-file status with no symlink traversal;
- a supported schema and bounded input size;
- stable record ID and path consistency;
- repository scope and Git-review visibility;
- lifecycle and supersession consistency;
- deterministic prohibited-content checks applicable to canonical records;
- revision and attestation consistency; and
- conflicts, parse errors, and malformed encodings.

An ignored untracked file is not ordinary Git-visible repository memory and is
not admitted merely because it sits under `.memzoi/records/`. An existing
tracked file remains reviewable even if a later ignore rule matches it.

Admission does not prove that an externally authored record previously passed
a Memzoi write authorization or repository-safe classification decision. Read
output must not claim such authorization. If deterministic admission cannot
establish safety for a record, the record is excluded rather than being treated
as repository-safe by parser default.

Manually authored records may be locally active when admitted. They are
reported as unattested unless valid versioned metadata binds their exact
revision to a decision and evidence set.

### 3. Repository reads must match the current checkout

Before a read surface returns repository memory, it must establish that its
repository projection corresponds to the current checkout inventory.

The inventory identity must be sufficient to detect materialization, external
edits, deletions, branch switches, checkouts, merges, resets, conflict states,
and relevant admission-policy changes. It must not rely only on `HEAD`, because
working-tree and index revisions may differ from `HEAD` and from each other.

If a derived projection is stale, the surface must do one of the following
before describing results as current:

1. reconcile it against admitted files;
2. rebuild it;
3. use a correct file-backed fallback; or
4. return an explicit incomplete or unavailable result.

Normal search may return bounded partial results only when the response clearly
identifies omitted or invalid records and marks completeness. It must not
silently return stale records as current.

A governance precheck must not report an unqualified `no warnings` result while
one or more repository records are invalid, unsafe, conflicted, stale, or
omitted because the projection could not be reconciled. It must reconcile,
return the applicable warning, or report that a reliable precheck is
unavailable.

A full rebuild from admitted canonical files must reproduce the same locally
active repository-memory set. Proposal packets, receipts, and runtime indexes
are not active memory.

### 4. Planning remains mutation-free

Capture, classified import, provider import, maintenance, migration, recovery,
and direct structured-input routes must produce or consume a mutation-free plan
before repository materialization.

Planning may inspect only the inventory needed for duplicate and contradiction
analysis, deterministic identity, expected-current-revision checks, evidence
validation, destination policy, classification, and safety planning.

Planning must not:

- create or modify canonical records;
- create repository proposal, journal, receipt, or recovery files;
- mutate runtime memory or its indexes;
- perform Git operations; or
- persist private or blocked plans beneath the repository root.

Saving an explicitly requested, repository-safe, snapshot-bound plan is a separate
authorized persistence operation, not a side effect of planning. A saved plan
does not become active memory and must still be revalidated at materialization.

### 5. Materialization authority is explicit and narrow

Materialization is a separate explicit operation. Initial authority is:

| Surface | Initial authority |
| --- | --- |
| Core library | Typed planning, admission, validation, gate, and materialization primitives |
| CLI planning commands | Plan-only |
| Explicit CLI materialization command | May write after full authorization and validation |
| MCP | No Git-native repository-materialization authority |
| Background process | No repository-write authority |
| Extractor or model adapter | No repository-write authority |
| Provider-import adapter | No direct repository-write authority |

Earlier pre-1.0 MCP operations that created DB-local proposal state have no
forward-compatibility guarantee and are not part of the current surface. MCP
recall, context, precheck, capture planning, and repository-maintenance
planning retain only their documented read-only, repository-only authority.

A future RFC or versioned extension may grant an MCP profile materialization
authority. That authority must be opt-in, locally authenticated by the host
boundary, revocable independently of recall, safe for logging and persistence,
and routed through the exact same core gate as CLI. Ordinary MCP access never
implies it.

No surface may combine ambient capture and repository materialization into an
implicit operation.

### 6. Every Memzoi repository write uses one shared gate

Before Memzoi writes candidate content anywhere beneath the repository root,
the common gate must verify at minimum:

- destination is exactly `repo`;
- sensitivity is explicitly repository-safe;
- scope identifies the current project;
- required provenance and evidence are present and valid;
- the action is permitted and explicitly authorized;
- deterministic prohibited-content detection passes across every emitted
  field, filename, and metadata value;
- duplicate and conflict decisions are resolved;
- source, plan, decision, and target revision preconditions remain current;
- target paths are canonical, contained, Git-review-visible, and do not follow
  symlinks;
- the stable record ID and intended revision identity satisfy their versioned
  contracts; and
- the exact implementation is the shared gate rather than a route-local copy.

Unknown, incomplete, ambiguous, local, personal, session, private, or
prohibited classification fails closed.

A failed gate produces zero repository writes, zero candidate-bearing temporary
files, proposal packets, journals, receipts, or recovery artifacts, redacted
bounded diagnostics, and no Git operation. Route-specific checks may add
constraints but may not bypass, weaken, or reimplement the common gate.

Transitional pre-1.0 handling does not permit sensitive candidate content
beneath the repository merely because an artifact is called a proposal,
journal, receipt, recovery packet, or temporary file.

### 7. Decisions, revisions, provenance, and time are pinned

A materialization decision binds:

- `plan_id`, `candidate_id`, and `decision_id`;
- stable `record_id` and intended `revision_hash`;
- action and expected target revision;
- classification and governing safety/schema versions;
- required evidence and provenance identities;
- lifecycle target and rationale when applicable; and
- decision time and any canonical timestamps derived from it.

For the same plan, pinned decision metadata, and repository preconditions,
projection produces the same canonical bytes. Retrying the same decision is
idempotent and must not generate a new timestamp, identity, or semantically
different record. A retry returns the already matching result or completes the
same validated operation.

The stable `record_id` normally survives a semantic revision. The
`revision_hash` and its attestations do not. An edit that changes meaning must
not continue to present evidence, capture lineage, or review metadata as
attesting to the old content.

The exact revision-hash projection and serialization rules are versioned and
must avoid self-referential hashing. Unknown required identity semantics fail
closed.

### 8. Git-rebuildable audit data remains compact

Every newly Memzoi-materialized revision must carry a compact versioned
materialization block in the canonical record or its versioned canonical
equivalent. It is audit metadata, not a second approval state. At minimum it
preserves:

```yaml
materialization:
  schema: memzoi/repository-materialization
  action: create
  plan_id: ...
  candidate_id: ...
  decision_id: ...
  decision_at: ...
  safety_contract: ...
```

Supersede and tombstone revisions additionally preserve the target record and
revision, and a bounded repository-safe reason. The exact additive YAML shape
may be finalized by the versioned canonical-record contract, but the following
facts may not exist only in SQLite or in an optional transitional proposal
artifact:

- action;
- target record and target revision, when applicable;
- lifecycle rationale;
- plan, candidate, and decision identities; and
- governing safety contract.

The canonical record remains small and human-readable. Full plans, raw source
bodies, private evidence, credentials, chats, prompts, or model traces are not
copied into records merely to create an audit trail.

Canonical records must meet the current schema. A record missing required
metadata, including `content_class`, is fail-closed and must be manually
upgraded after review or removed. Memzoi never invents a materialization
attestation or makes a legacy proposal canonical.

### 9. Manual edits and deletions use file-native behavior

Users may edit or delete materialized records with ordinary tools.

An edited record is locally active only after admission. A semantic edit may
retain its stable record ID but creates a new revision. When retained evidence
or decision metadata no longer validates against that revision, reads must
remove or qualify the stale attestation. If the record type requires validated
evidence, the record is excluded until repaired through an explicit plan.

A validation command must report invalid, unsafe, unattested, or inconsistent
records without modifying them. Memzoi must not silently overwrite manual
changes: later materialization requires an expected target `revision_hash` and
fails stale when the current admitted bytes do not match.

Rejecting a plan before materialization writes nothing. After materialization,
deleting or restoring an uncommitted change makes it locally inactive according
to the current checkout. Memzoi may print review or restore instructions but
does not execute them.

Durable removal of a baseline-backed record should normally use a versioned
supersede or tombstone action so rationale remains Git-rebuildable. Direct Git
deletion remains possible and must never be resurrected from a derived index.

### 10. Materialization output is action-aware

Materialization reports every changed repository-relative path, action,
governing identities, safety contract, and resulting revision hash. Review
guidance must match the actual operation:

- tracked update or deletion: a narrow ordinary Git diff instruction;
- untracked create: a rendered unified diff or an appropriate no-index review
  instruction that actually displays the new file; and
- multiple paths: one bounded summary plus exact path-specific review details.

Memzoi may render a diff itself. Rendering review output is not authority to
stage, commit, push, open a pull request, merge, or modify Git configuration.

### 11. Writes are atomic and recoverable

Single-record materialization is atomic at the file level. A failure or
interruption leaves either the previous complete file or the intended complete
file, never a partially written canonical record.

Temporary files must remain inside the validated directory, use restrictive
creation behavior, reject symlinks and path escapes, contain only content that
already passed the write gate, and be removed or deterministically recoverable.
They never become a hidden memory or audit store.

Multi-record operations require a versioned transaction and recovery contract.
Until it exists, materialization performs independently atomic record actions
or fails before writing when all-or-nothing behavior is required.

Recovery validates the operation identity, decision, destination, expected
target revision, intended revision, and current file state. Ambiguous state is
left untouched for human review. Recovery never trusts a stale derived index as
proof that a write completed and never bypasses the current write gate.

### 12. Personal and session memory do not cross through Memzoi

Memzoi must not write local, personal, session, private, unknown, or prohibited
content beneath the repository root, including ignored files, proposals,
journals, receipts, temporary files, recovery artifacts, generated projections,
logs, or test snapshots derived from real user content. `.gitignore` is not a
privacy boundary.

Private destinations continue to use their defined runtime stores. Promotion
requires a new explicit candidate, repository-appropriate provenance, fresh
classification, review, authorization, and the common write gate. It is never
an automatic move or copy.

External programs remain outside Memzoi's write authority. If they place
content in `.memzoi/records/`, read-side admission applies, and Memzoi must not
claim that the write was authorized or that admission proves original
classification.

### 13. Pre-1.0 proposals are not a supported compatibility route

Pending and resolved pre-1.0 proposals must be manually upgraded to a current
supported format or removed. Implementations may retain transitional readers,
but those readers are not a forward-compatibility contract and do not treat
proposals as active memory.

Any transitional operation that writes beneath the repository uses the same
current write gate and read-side admission contract. DB-local proposal state
does not grant repository-write authority.

New normal repository writes do not require proposal packets. Optional
repo-safe evidence or recovery artifacts may remain supporting evidence, but
they are not a hidden inbox, approval state, canonical store, active memory, or
safety bypass.

No automatic bulk promotion, fallback classification, or compatibility
guarantee applies before 1.0. Operators must review, manually upgrade, or
remove pre-1.0 artifacts before relying on them.

## End-to-end state model

```text
explicit evidence or structured candidate
  |
  v
mutation-free plan
  |
  +-- rejected / deferred / blocked ----------------------> no repo write
  |
  v
pinned materialization decision
  |
  v
shared repository-write gate
  |
  +-- failed ---------------------------------------------> zero repo writes
  |
  v
atomic unstaged .memzoi/records revision
  |
  v
repository-record admission
  |
  +-- invalid / unsafe / conflicted ----------------------> excluded + diagnostic
  |
  +-- interrupted / ambiguous ----------------------------> recover or human review
  |
  +-- restored / deleted ---------------------------------> locally inactive
  |
  v
admitted working-tree revision ---------------------------> locally active
  |
  +-- staged ---------------------------------------------> no semantic change
  |
  +-- differs from staged bytes --------------------------> working tree remains active
  |
  +-- identical to HEAD path -----------------------------> committed-at-HEAD fact
  |
  +-- present at configured baseline ref -----------------> baseline-backed fact
  |                                                          (not inferred approval)
  v
checkout inventory changes
  |
  v
reconcile / rebuild / correct fallback / explicit unavailable result
```

## Failure behavior

| Failure | Required result |
| --- | --- |
| Safety or classification failure | Redacted diagnostics and zero repository writes |
| Stale source, plan, decision, or target revision | Fail before writing; regenerate or revalidate |
| Exact duplicate | Return the deterministic match; no new timestamp or write |
| Conflict or contradiction | No write without a still-valid explicit lifecycle decision |
| Invalid or unsafe existing record | Exclude with bounded diagnostics; no implicit overwrite |
| Stale evidence after manual edit | Remove or qualify the attestation; exclude when evidence is required |
| Interrupted write | Validate and finish/remove only unambiguous operation artifacts |
| Stale derived projection | Reconcile, rebuild, use correct fallback, or report incomplete/unavailable |
| Unreliable precheck inventory | Never return an unqualified clean result |
| Missing or unresolved baseline ref | Report baseline state as unknown |
| Branch switch or checkout change | Recompute active inventory before returning current results |

## Required invariants

- Mutation-free planning produces zero memory, proposal, index, or Git writes.
- Every Memzoi repository write passes the same versioned gate.
- Private, blocked, unknown, and ambiguously scoped content produces zero
  Memzoi repository writes.
- Every repository read admits the current file independently of its origin.
- Staging never changes which revision is locally active.
- Stable record identity is distinct from exact revision identity.
- Evidence and authorization never attest to semantic bytes they do not bind.
- Precheck never reports an unqualified clean result from an incomplete or stale
  repository projection.
- Retrying one pinned decision is byte-deterministic and timestamp-idempotent.
- Lifecycle rationale for new supersede and tombstone revisions is
  Git-rebuildable.
- Legacy proposals and receipts are never active memory.
- Manual edits are never silently overwritten.
- A completed single-record write never leaves a partial canonical file.
- Runtime indexes never make absent, deleted, invalid, or stale repository
  memory appear current.
- Memzoi performs zero automatic stage, commit, push, PR, merge, checkout, or
  Git-configuration operations.
- Private-to-repository promotion is always a fresh explicit decision.

## Migration, compatibility, and versioning

### RFC 0001

RFC 0001 remains normative for explicit source authority, evidence-backed
capture, extractor boundaries, capture-plan identity, review artifacts, stale
source validation, destination and sensitivity classification, private-plan
handling, provider credential isolation, and mutation-free capture planning.

RFC 0002 supersedes the requirement that a repository candidate always follow:

```text
pending proposal -> separate canonical apply
```

An explicitly reviewed and authorized repository-safe candidate may instead
follow:

```text
reviewed plan -> repository-write gate -> unstaged canonical revision
```

The old route may be removed or changed in any pre-1.0 release. It is not a
supported compatibility contract.

### Existing state

- Canonical records missing required current metadata are fail-closed. Review
  each one and either manually upgrade it to the current schema or remove it.
- Pending and resolved pre-1.0 proposals, capture reviews, import plans,
  scripts, and integrations have no forward-compatibility guarantee. They must
  be explicitly upgraded, regenerated, or removed before use.
- Existing audit artifacts never override canonical files.

### Public contracts

Breaking changes to CLI JSON, MCP schemas, plan and decision schemas, proposal
formats, admission results, materialization results, or canonical identity
require explicit versioning. Before 1.0, a versioned breaking change does not
require a compatibility adapter: operators must upgrade or remove affected
artifacts. Unknown required semantics fail closed.

## Implementation sequence and issue mapping

1. [#101](https://github.com/Zokiio/Memzoi/issues/101) implements the shared
   repository-write gate across existing and planned routes.
2. [#100](https://github.com/Zokiio/Memzoi/issues/100) implements explicit CLI
   materialization, pinned decisions, atomic writes, action-aware review output,
   read-side admission, revision-safe manual edits, and checkout-projection
   reconciliation.
3. Transitional proposal apply, while present, remains behind the shared gate
   and can be removed in a pre-1.0 release without a compatibility adapter.
4. Capture, classified import, provider import, maintenance, migration,
   recovery, and direct proposal routes gain parity tests proving equivalent
   candidates receive equivalent write and admission decisions.
5. Any future MCP materialization profile requires a separate versioned
   decision after CLI safety, recovery, and route parity are proven.

If #100 and #101 cannot contain executable projection-consistency coverage for
materialization, external edits, deletion, and branch switches, that work must
be split into an independently tracked implementation issue before #100 is
considered complete.

The following existing design issues consume this contract rather than define
another repository-write model:

- #50: maintenance actions and lifecycle rationale;
- #57: provider surfaces and authority;
- #58: end-to-end governed repository outcomes;
- #63: contract versioning and compatibility; and
- #88: provider-import routing.

## Alternatives considered

### Keep proposals mandatory

Rejected for the normal route because it duplicates Git review. Transitional
proposal support may remain for specialized audit needs but is not a
compatibility commitment.

### Delay local activity until staging or commit

Rejected. Staging is a composition mechanism, and committed-only reads would
hide the current filesystem changes a developer is reviewing. Explicit
baseline-only modes may be added later.

### Treat baseline presence as shared approval

Rejected. Git can prove local tree and reachability facts, not social
acceptance. Memzoi reports the configured baseline fact and its uncertainty.

### Trust all parseable files or only the write gate

Rejected. External writers bypass Memzoi authorization, and successful parsing
does not establish repository-safe classification. Read-side admission is
required independently.

### Change record ID on every semantic edit

Rejected. Stable concept identity and exact revision identity serve different
purposes. Stale evidence and decisions attach to revisions, not automatically
to the stable record ID.

### Keep the runtime database canonical until integration

Rejected because it creates competing truths and makes external file and Git
operations unsafe to reason about.

### Give MCP materialization authority immediately

Rejected for the initial implementation. Any remaining DB-local proposal
behavior has no forward-compatibility guarantee and does not expand repository
authority.

## Consequences

### Positive

- Repository-memory review happens in ordinary diffs and pull requests.
- Local tools see the same admitted memory represented by the current files.
- Stable concepts survive revisions without allowing stale citations.
- Read-side safety covers external writers as far as deterministic local policy
  can establish, without overstating its guarantee.
- Derived-index consistency becomes an explicit read contract.
- Lifecycle rationale survives rebuild without mandatory proposal packets.
- The authority expansion is limited to one explicit CLI operation.

### Negative

- Local recall may include admitted, uncommitted memory.
- Tools must distinguish working-tree, `HEAD`, baseline, and attestation state.
- External writes cannot be proven to have passed original classification.
- Read surfaces need inventory reconciliation rather than trusting SQLite.
- Manual edits require revision-aware evidence handling.
- Transitional proposal support remains isolated from the normal route and can
  be removed without a compatibility adapter before 1.0.
- Atomicity, audit metadata, and cross-route parity make this more than a simple
  file writer.

### Accepted tradeoff

Memzoi prefers visible, admitted working-tree state over a hidden internal
approval state. A newly materialized record can influence local recall before
commit because materialization is explicit, the Memzoi write passed the shared
gate, the file is Git-visible, and it can be edited or removed immediately.

For externally authored records, local activity follows read-side admission,
not a claim of prior Memzoi authorization. Baseline reporting remains a Git fact
rather than a claim of collaborator approval.

## Maintainer decision

**Accepted on 2026-07-13.**

The maintainer ratifies:

1. admitted working-tree revisions are locally active;
2. staging has no Memzoi semantic effect and working-tree bytes win;
3. Memzoi reports `HEAD` and configured-baseline facts without inferring human
   acceptance;
4. Memzoi-originated writes use a shared gate while every file separately uses
   read-side admission;
5. stable record identity is distinct from exact revision identity;
6. every read surface establishes current-checkout projection consistency, and
   precheck cannot return a false clean result;
7. explicit CLI materialization may create unstaged canonical revisions;
8. MCP initially gains no repository-materialization authority, while any
   remaining DB-local proposal behavior has no forward-compatibility guarantee;
9. new lifecycle revisions preserve compact Git-rebuildable rationale;
10. retries use pinned decision time and are byte-idempotent;
11. Memzoi performs no Git acceptance operations and no implicit
    private-to-repository promotion; and
12. mandatory proposals are not required for normal repository memory and any
    transitional implementation can be removed before 1.0.

With acceptance, #99 may close, #101 implements the shared write gate first,
and #100 follows with materialization, admission, and projection consistency.
