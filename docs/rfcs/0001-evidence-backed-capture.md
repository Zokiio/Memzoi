# RFC 0001: Evidence-backed capture and extractor boundary

- Status: Accepted with profile-scoped implementation
- Accepted: 2026-07-10
- Direction endorsed: 2026-07-10
- Target: v0.4 — Evidence-backed Capture
- Tracking issue: [#45](https://github.com/Zokiio/Memzoi/issues/45)

## Summary

Memzoi will add a versioned `memzoi/capture-plan-v2` contract before the
existing classified-import and proposal workflows. A capture planner reads
only sources explicitly named by the caller, forms typed candidates with
candidate-scoped evidence, classifies their destination and sensitivity, and
returns a reviewable plan without writing canonical, proposal, local, session,
index, event, or configuration state.

Every plan is classified as `repo_safe`, `private`, or `blocked` from its
sources, evidence, and candidates. Private plans cannot be persisted anywhere
under the repository root or returned through an unauthorised or logging MCP
path. Blocked plans contain redacted diagnostics only.

Routing reviewed selections is a separate, explicit CLI operation. A
versioned review artifact records accept, reject, edit, and defer decisions against
one immutable plan. Apply revalidates that review, the saved plan, and its
source snapshots without rerunning a model or provider. Shipped in-process
deterministic profiles are replayed over those exact snapshots to authenticate
candidate completeness and derived fields before the existing destination
policy and guarded routing primitives run. A repo candidate
may create a pending, repo-safe OKF proposal; it can never create a canonical
record. Canonical repo memory still requires review and the existing explicit
proposal-file apply operation. MCP exposes planning only.

The first supported extractor is deterministic Markdown. Model-backed
extractors use the same strict contract through a provider-neutral,
out-of-process adapter. Credentials are supplied by the host environment and
never appear in a request, plan, repo configuration, log, proposal, or memory
record.

## Context

Memzoi 0.3.1 already has the downstream governance boundary needed by capture:

- `memzoi/import-v2` accepts already-formed, explicitly classified candidates.
- `memzoi/import-plan-v2` is deterministic and planning is intended to be
  mutation-free.
- destination policy maps `repo` to a file-backed proposal, `local` and
  `session` to private runtime routes, and `discard` and `needs_review` to no
  write;
- import apply recomputes its plan before writing and rejects a stale plan ID;
- repo proposals must be `repo-safe`, are reviewed as Markdown, and require a
  separate explicit apply before becoming canonical records; and
- proposal lineage (`proposal_id`) is distinct from evidence provenance
  (`source` and `source_ref`).

Import deliberately does not extract candidates from source material. Capture
fills that gap without weakening the review boundary.

## Goals

1. Form small, typed memory candidates from exact evidence in named project
   sources.
2. Give CLI and MCP one versioned, deterministic plan contract.
3. Make provenance, extractor identity, destination, sensitivity, confidence,
   and their reasons reviewable per candidate.
4. Detect changed plan bytes, stale sources, and changed routing inputs before
   any write, without claiming that an unkeyed digest authenticates a reviewer.
5. Prevent capture plans themselves from becoming a repo-sharing or logging
   bypass for private or prohibited source material.
6. Support deterministic and model-backed extractors without coupling core to
   a model provider or storing credentials.
7. Reuse the existing import, destination-policy, proposal-review, and
   canonical-apply boundaries.
8. Make safety, quality, latency, and review burden measurable.

## Non-goals

- Ambient repository scans, implicit globs, unbounded directory recursion, or
  discovery of sources outside an explicitly named file, directory, supplied
  payload, Git blob, or Git range.
- Raw chat, shell history, hidden agent state, prompts, context packs, or
  checkpoints as extraction sources.
- Automatic canonical promotion, background capture, or MCP apply.
- A new memory destination, a hosted service, provider-specific SDKs in core,
  or credentials in project files.
- Archive unpacking, OCR, image/audio extraction, lossy text decoding, or
  arbitrary URL fetching in `capture-plan-v2`.
- Cryptographic reviewer authentication. `plan_id` and `review_id` provide
  content identity and stale-input detection, not signatures or identity
  proof.
- A model extractor as a fallback when deterministic extraction fails.

## Terminology

- **Source descriptor**: an explicit, typed locator supplied by the caller.
- **Source snapshot**: the exact bytes resolved from a descriptor plus their
  media type, length, and content hash.
- **Evidence**: a validated, non-empty byte and line span inside one snapshot.
- **Claim**: the normalized proposed memory, exact evidence, and extractor
  identity before routing policy is applied.
- **Candidate**: proposed typed memory content, evidence, extraction metadata,
  confidence, and effective classification.
- **Capture plan**: an immutable review artifact containing snapshots,
  candidates, blockers, and a content-derived identity. It is not memory.
- **Capture review**: a versioned artifact that binds per-candidate accept,
  reject, edit, or defer decisions to one capture plan.
- **Route apply**: the explicit operation that validates a reviewed capture
  plan and review artifact and hands selected candidates to guarded routing
  primitives.
- **Canonical apply**: the existing, separate operation that applies an
  approved repo proposal to `.memzoi/records/`.

## Proposed decision

### Boundary and flow

The endorsed high-level flow is:

```text
explicit capture request
  -> guarded source snapshots
  -> extractor request/response
  -> host validation and classification
  -> capture-plan-v2 (no writes)
  -> human review -> capture-review-v2
  -> explicit CLI route apply
       repo    -> pending OKF proposal -> review -> explicit canonical apply
       local   -> private runtime record
       session -> private checkpoint
       discard / needs_review -> no write
```

Capture is a distinct stage before import. It does not extend
`memzoi/import-v2` to read source files, and it does not make the extractor a
writer. This keeps already-structured import stable and makes the new trust
boundary visible.

Planning may read existing Memzoi configuration and the minimum canonical,
pending-proposal, and runtime metadata needed for policy and duplicate or
conflict decisions. Those stores must never be mined as capture evidence.
Core capture planning consumes an immutable inventory snapshot; it must not
call normal `MemoryService::open`, which can create runtime directories, open
SQLite read/write, enable WAL, or migrate schema. The snapshot loader reads
canonical and proposal files directly and, when runtime metadata is needed,
opens an existing compatible database read-only without migrations. Missing,
incompatible, or unreadable runtime state emits a stable warning and forces
affected local/session actions to `needs_review`; it does not create state.

### Explicit source contract

The caller supplies `memzoi/capture-request-v2`. There are no default sources.
An empty source list is invalid. Source order is preserved because it is an
extractor input.

`source_id` values are unique ASCII identifiers matching
`[a-z0-9][a-z0-9._-]{0,63}`. Display names and paths reject control characters
and pass prohibited-data checks before they can appear in output. If a locator
itself is sensitive, blocked output uses only the source ordinal and a stable
redacted code.

`capture-plan-v2` recognizes these tagged locators:

- `project_path`: one POSIX project-relative regular file;
- `project_directory`: one explicit project-relative directory with a bounded,
  adapter-owned enumeration policy; the plan records the exact sorted files
  and hashes considered;
- `supplied_bytes`: caller-supplied bytes with an explicit safe display name,
  media type, byte length, and content hash; CLI stdin is one transport for
  this locator, not authority to inspect ambient input;
- `git_blob`: one POSIX path at an explicit full commit object ID; and
- `git_range`: an explicit repository identity plus full base/head object IDs
  and diff mode, producing a deterministic ordered set of files and hunks.

The Markdown tracer implements `project_path` first. The instruction adapter
uses that locator. The ADR adapter may use `project_path` or
`project_directory`; directory enumeration is non-recursive by default,
ignore-aware, bounded, sorted, and unable to escape the named root. The
Git-change adapter may use a diff `project_path`, `supplied_bytes` from stdin,
or `git_range`, including multi-file rename, delete, and merge cases, without
changing candidate semantics. Recognizing a locator does not require an
implementation to support it; unsupported kinds fail closed.

Remote URLs, chat text, implicit directories, globs, and symbolic links are
not v1 source locators. A `supplied_bytes` plan stores hashes and allowed
evidence spans, not the whole payload; route apply requires the exact bytes to
be supplied again and rejects a hash mismatch before writing. Current OKF
proposal `url` and `ref` provenance remain valid downstream fields, but they
do not grant capture permission.

Example request:

```yaml
schema: memzoi/capture-request-v2
sources:
  - source_id: auth-adr
    locator:
      kind: project_path
      path: docs/adr/0012-auth.md
    media_type: text/markdown
extractor:
  profile: markdown-deterministic
```

Normalized directory, stdin-diff, and Git-range requests use the same source
union; the transport reads stdin before calling core and supplies the computed
length/hash rather than granting ambient stdin access:

```yaml
sources:
  - source_id: accepted-adrs
    locator:
      kind: project_directory
      path: docs/adr
      recursive: false
      ignore_policy: git-v1
      include: ["*.md"]
  - source_id: reviewed-diff
    locator:
      kind: supplied_bytes
      display_name: reviewed.diff
      media_type: text/x-diff
      byte_length: 1842
      source_content_hash: blake3:2ca1...
  - source_id: release-range
    locator:
      kind: git_range
      repository: .
      base: sha1:1111111111111111111111111111111111111111
      head: sha1:2222222222222222222222222222222222222222
      merge_parent: first_parent
      rename_detection: true
```

The request chooses an allow-listed extractor profile. It cannot contain an
executable path, endpoint, API key, credential environment variable, arbitrary
prompt, or provider secret.

### Source and evidence identity

Hashes use BLAKE3 over exact bytes and are rendered as `blake3:<lowercase-hex>`.
No newline or Unicode normalization occurs before `source_content_hash` is
calculated.

Each evidence item is candidate-scoped and contains:

- the source ID and typed locator;
- the whole-source raw content hash;
- a zero-based, half-open byte range `[byte_start, byte_end)`;
- a one-based, inclusive line range derived from that byte range;
- `evidence_content_hash`, calculated over the exact bytes in the range; and
- the exact UTF-8 evidence text for review, with no surrounding context.

Evidence ranges must be non-empty, lie on UTF-8 boundaries, fit the snapshot,
and match both hashes and the derived line range. The host constructs or
reconstructs evidence text from the snapshot; it never trusts a model-supplied
quote. Every emitted candidate has at least one evidence item. Evidence items
are sorted by source order and byte position before identity calculation.

Line 1 begins at byte 0. LF advances the line once; CRLF is one terminator and
the CR remains part of the preceding raw line; a lone CR does not advance the
line. `line_start` contains `byte_start`, and `line_end` contains
`byte_end - 1`, so a span ending immediately after a newline ends on the line
whose terminator it includes. A final empty line has no evidence byte of its
own. These rules are versioned and tested without normalizing source bytes.

Adapters add a typed semantic location alongside the raw span: Markdown and
instruction sources carry a heading path/section kind; ADR sources carry the
parsed field and ADR status; Git evidence carries algorithm-prefixed object
IDs, repository identity, old/new path, hunk identity, side, and old/new line
ranges. Supplied diffs retain raw-byte spans. `git_range` uses a versioned diff
renderer and records merge parent selection, rename detection settings, and
diff-format version so the same full object IDs reproduce the same snapshot.

The plan does not contain whole source bodies. Evidence text and candidate
content pass the same post-extraction prohibited-data scan before they may be
returned.

### Candidate schema

Each candidate contains the following required semantic fields:

| Field | Decision |
| --- | --- |
| `claim_id` | Content-derived identity of the normalized memory draft, exact evidence, and extractor identity. |
| `candidate_id` | Content-derived identity of the claim plus confidence, effective destination, sensitivity, and action. |
| `memory.type` | Existing `MemoryType` value. |
| `memory.lane` | Existing `MemoryLane` value. |
| `memory.title`, `memory.body` | Non-empty normalized proposed memory. |
| `memory.scope`, `memory.tags` | Existing scope semantics; paths use the same project-relative validation. |
| `evidence[]` | One or more exact evidence items defined above. |
| `extraction` | Extractor kind, ID, version, and configuration/template hash; model identity when applicable. |
| `confidence` | Finite number in the closed interval `0.0..=1.0`; it never bypasses policy or review. |
| `classification.destination` | One of `repo`, `local`, `session`, `discard`, or `needs_review`. |
| `classification.destination_reason` | Non-empty explanation that does not quote prohibited content. |
| `classification.sensitivity` | Existing OKF sensitivity value. |
| `classification.sensitivity_reason` | Non-empty explanation that does not quote prohibited content. |
| `classification.policy` | Effective write route and review requirement from core destination policy. |
| `action` | Preview of the existing import outcome, including duplicate, blocked, no-write, runtime, or proposal route. |

Core trims outer whitespace from titles, bodies, reasons, tags, IDs, and paths
using the same typed-draft rules as existing import, preserves internal Unicode
and line endings, sorts only fields whose schema declares set semantics, and
rejects duplicates after normalization. `candidate_id` is a domain-separated
BLAKE3 digest of `claim_id`, confidence, effective destination, sensitivity,
and action. `claim_id` is a separately domain-separated digest of canonical
memory, evidence, and extraction identity. A policy change can therefore route
the same claim differently without pretending extraction produced a different
claim. Reviewer edits that change the memory draft produce a new reviewed
claim identity; destination-only edits retain the claim identity and produce a
new reviewed candidate identity. `plan_id` covers both identities and all
decision-affecting routing state.

Plan `status` is `ready` or `blocked`. Candidate actions are a closed tagged
union: `create_proposal` (reserved proposal ID and relative packet path),
`create_runtime` (destination/write route), `duplicate` (typed matching IDs),
`conflict` (safe matching IDs and `needs_review`), `no_write` (stable reason
code), or `blocked` (stable redacted code). Unsupported source/media cases
produce a blocked plan. Sensitive/unknown and low-confidence cases produce
candidate-local `needs_review`/`no_write` results unless a source-level global
blocker applies. The versioned examples and tests for every union variant are
a required #48/#49 deliverable before those schemas ship.

Extractor output is advisory. Core validates types and evidence, runs
deterministic safeguards, and computes the effective classification. An
extractor cannot weaken a safeguard result or choose a route inconsistent with
the sensitivity policy.

Effective sensitivity rules for capture are:

- `repo-safe` may target `repo`, which still means a reviewed proposal;
- `local-only` cannot target `repo`;
- `temporary-state` may target `session` or `discard`;
- `sensitive` and `unknown` are forced to `needs_review` and write nothing; and
- `secret`, `raw-transcript`, and `private-personal-data` block the batch and
  are not echoed in candidate, diagnostic, human, JSON, provider, proposal, or
  memory output.

The host may only preserve a proposed route or make it safer. A classification
mismatch becomes `needs_review` with a stable diagnostic code; it is never
silently upgraded to `repo-safe`.

### Plan data classification and storage

The plan is a review artifact that may contain exact source text. It is not
safe merely because planning performs no writes. Core assigns one
`data_class` after preflight and again after post-extraction safeguards:

| Plan contents | `data_class` |
| --- | --- |
| Only repo-safe sources, locators, evidence, and candidates | `repo_safe` |
| Any local/session source or candidate, `sensitive`/`unknown` classification, private locator, or private evidence | `private` |
| Secret, raw transcript, private personal data, unsafe source, or global safeguard violation | `blocked` |

Classification is enforced, not advisory:

- `repo_safe` plans may be printed or explicitly saved to an allowed review
  path, never under `.memzoi`, the private runtime directory, or generated
  exports.
- `private` plans default to stdout or the private runtime directory and cannot
  be saved under the repository root, `.memzoi/`, a generated export, or an
  integration file. Path checks use resolved containment, not string prefixes.
- MCP may return a private plan only to an explicitly authorised local client.
  The server must not persist the request, response, evidence, or candidate
  content in logs, traces, caches, or metrics labels.
- `blocked` plans contain only the redacted blocked-plan envelope and safe
  diagnostics defined under safeguards. They never contain candidate or
  evidence text.

Changing a source, edit, or classification so that the effective data class
changes produces a different plan or review identity. Route apply repeats this
classification and fails closed before writing.

### Example `capture-plan-v2`

This example is abbreviated only by replacing hashes with illustrative values;
an implementation returns full hashes.

```yaml
schema: memzoi/capture-plan-v2
plan_id: capture_4a26...
status: ready
data_class: repo_safe
sources:
  - source_id: auth-adr
    locator:
      kind: project_path
      path: docs/adr/0012-auth.md
    media_type: text/markdown
    byte_length: 842
    source_content_hash: blake3:91bf...
safeguards:
  policy_version: memzoi/capture-safeguards-v1
  configuration_hash: blake3:6d73...
preconditions:
  policy_version: memzoi/destination-policy-v1
  candidates:
    candidate_7098...:
      duplicate_match_set_hash: blake3:ee20...
      conflict_match_set_hash: blake3:7f31...
      reserved_proposal_id: capture-authentication-uses-signed-sessions
      relevant_record_hashes: []
extractor:
  kind: deterministic
  id: memzoi-markdown
  version: 1.0.0
  configuration_hash: blake3:2718...
candidates:
  - claim_id: claim_61c2...
    candidate_id: candidate_7098...
    memory:
      type: decision
      lane: semantic
      title: Authentication uses signed sessions
      body: Authentication uses server-verified signed sessions.
      scope:
        kind: repo
        id: null
        paths: [src/auth]
      tags: [authentication]
    evidence:
      - source_id: auth-adr
        locator:
          kind: project_path
          path: docs/adr/0012-auth.md
        source_content_hash: blake3:91bf...
        span:
          byte_start: 214
          byte_end: 278
          line_start: 12
          line_end: 13
        evidence_content_hash: blake3:152c...
        text: Authentication uses signed sessions verified by the server.
    extraction:
      kind: deterministic
      id: memzoi-markdown
      version: 1.0.0
      configuration_hash: blake3:2718...
    confidence: 0.96
    classification:
      destination: repo
      destination_reason: Durable project architecture decision.
      sensitivity: repo-safe
      sensitivity_reason: Project design contains no prohibited data.
      policy:
        write_route: file_backed_proposal
        review: proposal_review
    action:
      kind: create_proposal
      proposal_id: capture-authentication-uses-signed-sessions
      path: .memzoi/proposals/pending/capture-authentication-uses-signed-sessions.md
summary:
  sources: 1
  candidates: 1
  blocked: 0
blockers: []
```

Human output is a rendering of this contract, not a separate semantic plan.
CLI- or MCP-specific envelope fields such as mode, actor, transport request ID,
or generation time are outside the plan and outside its identity.

### Review selections and edits

Route apply never treats every planned action as implicitly approved. A human
records one strict `memzoi/capture-review-v2` artifact:

```yaml
schema: memzoi/capture-review-v2
review_id: review_8b13...
plan_id: capture_4a26...
reviewed_by: maintainer:zoki
reviewed_at: 2026-07-10T18:00:00Z
decisions:
  - candidate_id: candidate_7098...
    outcome: accept
  - candidate_id: candidate_9912...
    outcome: reject
    reason_code: not-durable
  - candidate_id: candidate_1184...
    outcome: defer
    reason_code: insufficient-context
  - candidate_id: candidate_3011...
    outcome: edit
    memory:
      type: decision
      lane: semantic
      title: Revised reviewed title
      body: Revised reviewed body.
      scope:
        kind: repo
        id: null
        paths: [src/auth]
      tags: [authentication]
```

Every plan candidate appears exactly once. An empty review or duplicate,
unknown, or omitted candidate ID is invalid. `accept` preserves the candidate;
`reject` writes nothing; `defer` writes nothing and records that the reviewer
has not made a terminal decision; `edit` may replace only the typed memory
draft and an explicit requested destination. Evidence, source snapshots, and
extraction identity cannot be edited. A later review may replace a deferred
decision by repeating the complete decision set, naming the prior review ID,
and changing only deferred entries. It receives a new review ID and must pass
the same stale checks when applied. The required v0.4 profile supports one
such predecessor hop. Deeper review chains fail closed because the v0.4 CLI
and apply boundary carry only the immediate predecessor; a future extension
must carry and validate the complete ancestor chain before enabling more hops.

Core reruns shape, prohibited-data, destination, sensitivity, duplicate, and
conflict validation over edits and computes new reviewed claim and candidate
identities as applicable. Human authority depends on why the original
candidate required review:

| Condition | Human authority |
| --- | --- |
| Ambiguous destination, sensitivity, or low confidence | May edit or sanitize the draft and explicitly request `repo`, `local`, `session`, or `discard`; core reclassifies and reroutes it. A `repo` request succeeds only when the retained evidence and locator are also repo-safe. |
| Possible duplicate or conflict | Cannot silently force creation; the reviewer must choose an explicit lifecycle resolution or regenerate after resolving the conflict. |
| Secret, raw transcript, private personal data, unsafe path, malformed source, or global safeguard violation | Hard blocker; cannot be overridden by a review. |

Thus `needs_review` is a resolvable no-write state when the ambiguity is within
human authority, not a permanent route. Review cannot override a global
blocker, silently accept a possible conflict, or upgrade content to
`repo-safe` without passing the same deterministic policy.

`review_id` is a domain-separated BLAKE3 digest over the plan ID, ordered
decisions, reviewed candidate identities, reviewer, and review time. Like
`plan_id`, it is content identity rather than authentication. The reviewed ID
must be pinned by the explicit apply request or review channel. The proposal
audit block retains the review ID, actor, time, decision, and any safe edit
reason. Mixed selections are transactional: all selected writes commit or none
do. A review containing only rejects and deferrals succeeds with zero writes
and a resolved review result; deferrals remain eligible for a later review and
are not reported as rejections.

### Plan identity and stale checks

`plan_id` is:

```text
"capture_" + hex(
  BLAKE3(
    "memzoi/capture-plan-v2\0" ||
    RFC8785_JCS(identity_payload)
  )
)
```

`identity_payload` is the plan without `plan_id`. It includes every persisted
human-visible semantic field and decision-affecting input or result: schema
version, summary counts, ordered
source descriptors and hashes, effective limits and safeguard versions,
extractor/model identity and configuration hashes, routing policy and
candidate-specific preconditions, normalized claims, candidates and evidence,
effective classifications, actions, and stable blocker codes with safe
locations. Human diagnostic prose is derived from identity-covered codes at
render time and is never accepted as unvalidated plan input.

Actor identity, timestamps, transport IDs, diagnostic prose, credential names
and values, and provider request IDs are excluded. Actor and time do not change
what a reviewed plan means. Credentials must not be persisted at all.

Core owns canonicalization and fingerprinting; CLI and MCP call the same core
function. Objects use RFC 8785 JSON Canonicalization Scheme, arrays use the
semantic ordering specified by this RFC, and non-finite confidence values are
invalid.

The digest detects accidental edits, binds a pinned review to exact content,
and makes changed content a different plan. Because it is public and unkeyed,
anyone can edit a plan and compute another internally consistent ID; it does
not prove which plan a human reviewed.

Route apply accepts the complete plan, a complete review artifact, and pinned
expected plan/review IDs. Before the first write it atomically performs these
checks:

1. Parse the exact supported schema and reject unknown fields.
2. Recompute `plan_id` and `review_id` and compare them with the pinned expected
   IDs.
3. Re-resolve every named source under the same path and media safeguards;
   require exact re-supplied bytes for `supplied_bytes`.
4. Recompute source hashes, byte lengths, evidence ranges, evidence hashes, and
   evidence text.
5. Resolve the same non-secret extractor profile definition and recompute its
   configuration/template hash. Replay an allow-listed in-process deterministic
   adapter over the already resolved bytes and require exact candidate and
   diagnostic equality. A future non-deterministic profile instead requires a
   trusted issuance attestation; apply never invokes its model or provider. A
   missing or changed profile makes the plan stale, and provider credentials
   are never required at apply.
6. Recompute safeguard configuration, destination policy, and each candidate's
   targeted routing preconditions: duplicate and conflict match sets, matched
   record hashes/statuses, and reserved proposal identity.
7. Validate every review decision and edited candidate, rebuild selected
   routing actions, and require semantic equality with the reviewed actions.
8. Only then invoke the guarded destination-specific write primitives.

Any mismatch rejects the whole batch as stale with stable, redacted codes and
zero writes. Source changes, policy changes, newly conflicting memory, or plan
edits therefore require a new plan and review. Review edits retain
the plan ID but produce new reviewed-candidate and review IDs. A changed plan
actor or wall clock does not change the plan.

The planner must not use one hash of the complete memory, proposal, or runtime
inventory. Each candidate records only state capable of changing its action:

```yaml
preconditions:
  policy_version: memzoi/destination-policy-v1
  candidates:
    candidate_7098...:
      duplicate_match_set_hash: blake3:...
      conflict_match_set_hash: blake3:...
      reserved_proposal_id: capture-authentication-uses-signed-sessions
      relevant_record_hashes:
        - record_id: mem_...
          content_hash: blake3:...
          status: active
          updated_at: 2026-07-09T12:00:00Z
```

A plan becomes stale when a relevant match appears or disappears, a matched
record changes, the reserved proposal identity is taken, or an applicable
policy/safeguard changes. An unrelated record elsewhere in the project does
not invalidate it. The read-only snapshot loader may use broader indexes to
find relevant records, but only the deterministic match sets and target state
become apply preconditions.

A model, remote provider, or other non-deterministic extractor is **not rerun
during apply**. This avoids nondeterminism and a second disclosure and ensures
the human reviewed the candidate that will be routed. Current deterministic,
in-process adapters are replayed solely to authenticate the complete plan from
exact pinned bytes; their output must be byte-equivalent. A future
non-deterministic profile must provide a trusted issuance attestation instead.
Apply always resolves and hashes the non-secret allow-listed profile
definition. A review edit is validated and fingerprinted through
`capture-review-v2`; it does not masquerade as extractor output.

### Deterministic extractor first

The first production extractor parses explicit UTF-8 Markdown using versioned,
deterministic rules. Given identical source snapshots, effective configuration,
and routing inputs, it must produce a byte-equivalent semantic plan and the
same ID.

It must not call a model, network, shell, Git discovery command, or fallback
extractor. Failure produces a blocked plan. The exact Markdown candidate rules
belong to #48 and are evaluated by the v0.4 corpus; they do not alter this
boundary.

Instruction-file, ADR, and Git-change extractors are adapters over the same
source, evidence, candidate, and plan contracts. They may specialize
deterministic parsing, but cannot relax safeguards.

### Profile-scoped implementation

Acceptance fixes the core contract without requiring every described source or
extractor profile in the first tracer:

| Scope | Required capability |
| --- | --- |
| Core contract | `capture-plan-v2`, `capture-review-v2`, exact evidence, plan/review identity, data classification, targeted stale checks, safeguards, and governed routing. |
| Required v0.4 profile | One explicit `project_path`, UTF-8 Markdown, and the deterministic Markdown extractor. |
| Extension profiles | `project_directory`, instruction and ADR directories, supplied Git diff/bytes, `git_blob`, `git_range`, and model-backed extraction. |

The extension profiles remain specified so they cannot weaken the boundary
when implemented. They require their own implementation support, corpus cases,
and release gates. #48 and #49 are not blocked on implementing them.

### Provider-neutral model boundary

Core exposes one logical extractor interface. A model-backed implementation is
a one-shot, out-of-process adapter using strict JSON on stdin/stdout:

```json
{
  "schema": "memzoi/extractor-request-v1",
  "request_id": "request_...",
  "sources": [
    {
      "source_id": "auth-adr",
      "media_type": "text/markdown",
      "source_content_hash": "blake3:91bf...",
      "content": "...bounded, preflighted UTF-8 source bytes..."
    }
  ],
  "output_schema": "memzoi/extractor-response-v1"
}
```

```json
{
  "schema": "memzoi/extractor-response-v1",
  "request_id": "request_...",
  "extractor": {
    "id": "example-model-adapter",
    "version": "2.1.0",
    "configuration_hash": "blake3:...",
    "template_hash": "blake3:...",
    "model": {
      "provider": "example",
      "name": "model-name",
      "version": "provider-reported-version"
    }
  },
  "candidates": [
    {
      "candidate_key": "response-local-1",
      "type": "decision",
      "lane": "semantic",
      "title": "...",
      "body": "...",
      "evidence_refs": [
        {"source_id": "auth-adr", "byte_start": 214, "byte_end": 278}
      ],
      "confidence": 0.88,
      "destination": "repo",
      "destination_reason": "...",
      "sensitivity": "repo-safe",
      "sensitivity_reason": "..."
    }
  ]
}
```

The protocol gives the adapter bounded source content rather than source paths
or a Memzoi mutation API. That is a protocol boundary, not an operating-system
sandbox: an ordinary child process may still have filesystem, process, and
network authority. Therefore an allow-listed adapter executable is trusted
host code. Supporting an untrusted adapter requires an actual platform sandbox
with explicit filesystem, process, and egress policy; a scrubbed environment
and non-repository working directory alone are insufficient. The host may pass
only credentials allow-listed in local host configuration or a secret manager.
Project configuration, CLI/MCP arguments, requests, and plans may select only
a preconfigured profile; they cannot provide credentials or arbitrary commands.

Core contains no provider SDK. The adapter declares its own version, exact
configured model identity/version, template hash, and non-secret configuration
hash. These are required candidate provenance and plan-identity inputs. Raw
provider responses, chain-of-thought, prompts, tokens, and provider request IDs
are not retained.

Source content and remote-model output are untrusted data. Model requests use a
fixed versioned instruction template and grant the remote model no tools. Core
does not honor response attempts to add sources, request reads, invoke tools,
change safeguards, or write; it rejects unknown fields, mismatched
request/source IDs, invalid enums, unverifiable spans, and extra stdout after
the single response object. Core then rebuilds evidence from the trusted
snapshot and reapplies all deterministic policy. Stdout and stderr are bounded
separately; stderr is treated as potentially sensitive and is redacted or
discarded rather than stored. Timeout or cancellation terminates the adapter's
process tree and discards partial output.

Selecting a model profile is an explicit disclosure decision because a remote
provider may retain data under its own policy. Deterministic extraction remains
the default. No model profile ships as an automatic fallback. MCP may invoke a
remote-model profile only when the server administrator enabled that exact
profile and its egress policy; an MCP request cannot enable remote disclosure.

## Safeguards

All evidence sources are untrusted. Preflight completes for the entire batch
before any extractor runs. Source-level path, size, prohibited-data, or media
failures produce a redacted blocked plan and make the batch non-routable.
Candidate-local duplicate, conflict, low-confidence, `unknown`, or `sensitive`
outcomes remain visible but write nothing unless a valid review decision and
policy make them routeable. Mixed reviewed writes are transactional.

### Path and file handling

- Paths must be non-empty, UTF-8, POSIX project-relative paths with no absolute
  prefix, drive prefix, backslash, NUL, `.` or `..` component.
- Every path component is checked without following symlinks. `project_path`
  accepts only regular files; `project_directory` accepts only the explicitly
  named directory and regular files selected by its bounded enumeration.
  Symlinks, devices, sockets, FIFOs, and other special files are rejected.
- The resolved file must remain beneath the already-discovered project root.
- For every selected file the planner opens one handle, checks metadata, reads
  and hashes from that handle, and verifies stable metadata after reading. A
  change during enumeration or read blocks the batch.
- Caller-supplied bytes are bounded before allocation and receive the same
  media, secret, evidence, and output validation as file-backed bytes.
- Git locators use explicit full object IDs and read objects, not ambient
  working-tree discovery. Missing, abbreviated, or changing refs are rejected.
- `.memzoi/**`, the runtime home, generated exports/projections, and known
  Memzoi-managed instruction blocks are never evidence sources. Instruction
  adapters fingerprint and exclude their generated marker ranges to prevent
  feedback loops.
- Directory adapters may read an explicitly versioned ignore-policy input as
  policy, not evidence. ADR directory capture uses applicable worktree
  `.gitignore` files. Git-range capture reads applicable `.gitignore` blobs only
  from the explicitly named head tree; project and supplied Git diffs do not
  consult ambient worktree ignore files. Every such policy path/hash and the
  ignore-engine version appears in the plan identity. Policy bytes receive the
  same prohibited-content preflight before any derived hash can enter a plan.
  “Explicit sources only” means zero unnamed evidence or policy reads; it does
  not hide these enumerated policy/state inputs.

### Size and resource handling

- Effective per-source, aggregate-source, evidence-text, candidate-count, and
  output-size limits are mandatory and included in the safeguard configuration
  hash.
- A source over a limit is rejected; it is never silently truncated.
- Size is checked before allocation and while streaming so misleading metadata
  cannot bypass the limit.
- v1 never decompresses or recursively expands content.
- Timeouts and process-output limits terminate a model adapter and block the
  batch. Partial responses are discarded.

The proposed `memzoi/capture-safeguards-v1` profile is explicit in every plan:

| Resource | Default | Hard ceiling |
| --- | ---: | ---: |
| One Markdown/instruction/ADR source | 1 MiB | 8 MiB |
| Aggregate source bytes | 4 MiB | 32 MiB |
| One supplied diff payload | 2 MiB | 16 MiB |
| Directory files / relative depth | 128 / 1 | 1,024 / 8 |
| Candidates | 100 | 1,000 |
| One evidence item / all evidence text | 16 KiB / 256 KiB | 64 KiB / 4 MiB |
| Serialized plan or review JSON | 2 MiB | 16 MiB |
| Adapter stdout / stderr | 2 MiB / 64 KiB | 16 MiB / 1 MiB |
| Adapter wall time / termination grace | 60 s / 2 s | 300 s / 10 s |

Profiles may lower defaults. Raising an effective value up to a hard ceiling is
an explicit local-host configuration choice; exceeding a ceiling requires a
new safeguard profile/version. Cancellation uses the same process-tree
termination and zero-partial-output rule as timeout. Effective values, not only
their configuration hash, are human-visible and identity-covered. #47/#55 may
tighten defaults based on corpus evidence. Any effective-limit change makes an
existing plan stale.

### Secret and prohibited-content handling

- A deterministic secret/prohibited-data scan runs on raw bytes before any
  extractor or provider call and on all candidate/evidence/diagnostic text
  after extraction.
- A match blocks the entire batch. No candidate content from the batch is
  returned or written.
- Diagnostics contain only a stable code, source ID, and safe location. They
  never contain the matched value, a reversible encoding, surrounding text, or
  a source hash derived solely from the secret.
- Logs, errors, traces, metrics labels, provider requests, and test snapshots
  follow the same no-echo rule.
- A model's `secret`, `raw-transcript`, or `private-personal-data`
  classification is itself a block signal even if the deterministic scanner did
  not identify the content.

The blocked-plan schema contains the plan schema/version, `status: blocked`,
safe source ordinals or validated IDs, safeguard profile/version, stable codes,
and safe locations such as line number when the locator itself is not
sensitive. It omits candidate text, evidence text, sensitive locators, and
source/evidence hashes for blocked spans. Scanner rule versions are
fingerprinted. False-positive tuning may change the rules, but cannot introduce
an override that writes content detected by the effective profile.

No finite detector proves that arbitrary input contains no secret or private
data. The hard release claim is zero prohibited-data leakage across the
versioned corpus and the documented detector classes, plus the absolute rule
that detected values are never echoed or sent onward. A remote provider may
already have received data missed by preflight before a model classifies it as
private; that classification blocks output but cannot undo disclosure, which
is why remote-model use requires explicit profile and egress approval.

### Prompt-injection handling

Prompt-injection text is treated as data, not as an exceptional instruction
channel. Safety does not depend on reliably detecting every injection:

- extractors receive only named, bounded snapshots;
- the remote model has no tools, and the adapter protocol exposes no Memzoi
  mutation operation;
- source text cannot choose profiles, prompts, destinations, credentials, or
  additional sources;
- strict response validation and evidence reconstruction prevent forged
  locators or unsupported fields;
- deterministic policy can only preserve or tighten model classifications; and
- planning mutates no Memzoi state; an explicitly requested plan artifact is
  the only allowed planning write, and routing remains an explicit reviewed
  CLI action.

An injection heuristic may emit a redacted warning. It never grants authority.
If a model response attempts protocol or source-set changes, the entire response
is rejected. Deterministic instruction-file extraction remains available for
files whose legitimate content consists of agent instructions.

### Unsupported content

The v1 Markdown path accepts UTF-8 text without NUL bytes. Invalid UTF-8,
binary-looking content, a media-type mismatch, unsupported locator/media type,
or malformed adapter response produces a blocked plan with no extractor
fallback and no lossy decoding. The error must not echo source content.

## Routing and proposal compatibility

Current import is a policy precedent, not a lossless drop-in bridge: its
sources are manifest-wide, local/session writes omit capture metadata, session
routing constrains type/lane, canonical OKF apply projects only one legacy
source and confidence, and current citations have no evidence list or spans.
Route apply therefore reuses shared duplicate, destination, sensitivity,
transaction, and proposal-write primitives but does not squeeze capture through
the existing `ImportCandidateInput` unchanged.

#49 must add one typed routed-capture representation and preserve, for repo,
local, and session destinations, candidate-scoped evidence, extractor/profile
identity, confidence, sensitivity, scope, tags, plan ID, review ID, and review
decision. Runtime schemas keep the same provenance for local/session cited
recall; session routing must not silently rewrite a reviewed capture draft.

For `repo` candidates, #49 adds optional capture provenance to the pending OKF
proposal packet. That block contains `capture-plan-v2`, `capture-review-v2`,
claim/candidate/reviewed-candidate IDs, exact repo-safe review evidence,
extraction identity, confidence, classifications, and the review decision. The
existing `sources` projection remains populated for older readers. A resolved
proposal retains this complete capture block. Proposal identity remains review
lineage and is not substituted for evidence identity.

Canonical apply remains unchanged in authority: only the existing explicit
proposal-file apply may create the repo record, and it rechecks `repo-safe`.
Evidence is intentionally stored at three different levels:

| Surface | Evidence representation |
| --- | --- |
| Capture plan and review | Exact excerpt text, typed locator, source hash, byte/line span, excerpt hash, and semantic location. |
| Pending and resolved proposal | Exact repo-safe review evidence plus claim, candidate, plan, review, and extractor identities. |
| Canonical record | Compact references: locator, source revision/hash, span, evidence hash, and an optional bounded short excerpt. |

For a Git-tracked path, capture records the resolved blob or commit object ID
when available as well as the path and content hash. This makes historical
evidence reproducible after the working tree changes.

To satisfy #49, the additive OKF record schema gains the compact optional
versioned evidence list; proposal parsing/rendering, resolved packets, rebuild,
and the derived runtime index preserve the appropriate tier. Existing
`source`/`source_ref` remains a legacy primary-evidence projection, while
`proposal_id` remains separate review lineage. Derived storage gains typed
evidence rows (or an equivalently queried versioned structure), and recall
citations expose source/span/hash plus extractor, plan, and review identities.
Older records without the block remain valid. This extension does not bypass
the proposal packet or canonical apply.

MCP may accept a capture request and return `capture-plan-v2`. It must not
accept route/apply flags, write a plan file, create proposals or runtime rows,
approve proposals, or apply canonical records. It may return a `private` plan
only through an explicitly authorised local-client profile whose request and
response bodies are excluded from persistence and logging. It returns a
redacted envelope for `blocked` plans. CLI route apply is introduced by #49 as
the explicit mutation boundary.

## Threat model

| Threat | Required behavior |
| --- | --- |
| Path traversal or symlink escape | Reject before reading; no fallback path resolution. |
| File changes between plan and apply | Raw hash/length mismatch makes the whole plan stale. |
| File changes while planning | Stable-handle/metadata check blocks the batch. |
| Oversized, binary, compressed, or unsupported input | Fail closed without truncation, expansion, or lossy decode. |
| Secret or prohibited personal/transcript data | Pre/post scan, no provider call after a preflight hit, redacted diagnostics, zero writes. |
| Prompt injection | Treat as powerless model data; fixed template, tool-free remote model, strict schema, host policy, explicit review. |
| Malicious or compromised adapter | Adapter is trusted host code unless actually OS-sandboxed; core still verifies every returned field and span and grants no Memzoi mutation protocol. |
| Forged provenance | Reconstruct evidence from trusted snapshots and verify whole-source and span hashes. |
| Model nondeterminism | Apply the reviewed plan without rerunning the model. |
| Plan/review edits | Mismatched pinned IDs fail; a recomputed unkeyed ID creates different content requiring fresh review but does not authenticate its author. |
| Actor/time replay differences | Excluded from identity because they do not change plan meaning. |
| Changed duplicate or proposal state | Candidate-specific match sets, target hashes, and proposal reservations reject stale affected plans before writes without coupling them to unrelated inventory changes. |
| Private plan persistence or logging | Enforce `data_class` at every CLI/MCP save, return, log, trace, export, and integration boundary. |
| Partial-batch confusion | Source/global blockers make the batch non-routable; candidate-local no-write outcomes remain explicit; selected writes are transactional. |
| Canonical-write bypass | Capture can create at most a pending repo proposal; canonical apply remains separate. |
| Credential disclosure | No credential fields; trusted profile controls environment/secret injection; bounded stderr and detected values never enter output or logs. |
| Provider retention or egress | Model use is explicit and profile-controlled; deterministic extraction is default. |
| Ambient or raw-chat ingestion | Only named, media/profile-constrained evidence sources are read; `supplied_bytes` is accepted only by supported non-chat adapters such as explicit Git diff. |

## Metrics and release gates

The following are hard invariants for v0.4:

- zero Memzoi state mutations during planning, including migrations, events,
  indexes, proposals, runtime rows, and canonical files; an explicitly
  requested plan artifact is allowed;
- zero unnamed evidence-source reads and zero ambient scans; enumerated
  identity-covered policy/inventory inputs are allowed;
- 100% validity for emitted source hashes, evidence spans, span hashes, and
  line ranges;
- zero accepted stale plan/review IDs or source/configuration mismatches;
- zero prohibited-data or credential leakage across the versioned corpus and
  documented detector classes in human/JSON/MCP output, logs, provider
  requests, proposals, runtime rows, and canonical records;
- zero persistence of `private` plans beneath the repository root, in
  generated exports or integration files, or in MCP server logs;
- zero canonical repo writes from capture routing;
- deterministic plans are byte-equivalent for identical inputs; and
- every repo candidate remains behind repo-safe proposal review and explicit
  canonical apply.

The eval report must also measure candidate precision/recall, evidence validity,
destination and sensitivity accuracy, duplicate/conflict handling, extractor
failures, latency, payload size, candidate counts, and human proposed,
accepted, rejected, edited, deferred, duplicate, and needs-review outcomes.

Numeric quality, latency, and review-burden thresholds are deliberately not
invented in this RFC. #44 establishes the file-native corpus/reporting
foundation, #47 provides safety and quality metrics in CI, and #55 records
the observed baseline and v0.4 go/no-go thresholds. Mutation-free planning,
detected-value non-echo, provenance, stale-input rejection, corpus leakage, and
canonical-boundary invariants above are not deferrable numeric targets.

A model-backed extractor cannot become the default until it clears the same
corpus and hard gates and materially improves accepted candidate quality or
review burden over deterministic extraction.

## Schema cutovers

- Before v1.0, capture, import, session-end, OKF record, and proposal formats
  are hard cutovers. Current readers reject older schemas; they do not infer
  defaults, accept aliases, dual-write, or migrate artifacts automatically.
- Capture-generated repo proposals carry the current versioned provenance
  block. Every reader validates that block directly.
- The v2 request, extractor, plan, review, and OKF evidence schemas reject
  unknown fields within their versioned blocks. A breaking field, changed
  span/hash semantics, relaxed source authority, or incompatible locator
  requires another schema version.
- CLI and MCP share core parsing, normalization, safeguard, and fingerprint
  code. Transport envelopes do not redefine the plan.
- A plan created under a different safeguard, extractor, destination policy,
  or relevant candidate precondition is stale and must be regenerated.
- Capture plans are review artifacts, not canonical memory or disposable
  indexes. Planning prints them; it does not silently save them. A caller may
  explicitly save a `repo_safe` plan to an allowed path for review and pass
  that exact artifact to CLI route apply. A `private` plan may be saved only
  under the private runtime directory; a `blocked` plan contains no reviewable
  content.
- Exact Markdown candidate rules, CLI command spelling, and concrete encoding
  of the required capture/review/evidence blocks are implementation details for
  #48/#49.
  They must conform to this contract and are verified by #47/#55.

## Alternatives considered

### Make import read files

Rejected. `import-v1` has a useful, narrow promise: callers supply already
formed candidates. Mixing file IO and extraction into it would make its current
plan identity and trust boundary ambiguous.

### Scan the repository and suggest memories automatically

Rejected. Ambient scans expand authority, create privacy surprises, and make
provenance and stale checks harder. Explicit source enumeration is part of the
product contract.

### Start with a model extractor in core

Rejected. A deterministic Markdown tracer provides a reproducible baseline.
Provider SDKs would couple credentials, network policy, and release cadence to
memory semantics.

### Rerun extraction during apply

Rejected. Model output may differ, a second provider call repeats disclosure,
and the applied candidate may not be what the human reviewed. Apply validates
the saved plan and current sources instead.

### Hash only the source, without evidence spans

Rejected. A source hash proves which file version was read but not which claim
supports a candidate. Candidate-scoped byte/line spans are required.

### Include actor and timestamp in `plan_id`

Rejected. They create false staleness without changing the reviewed decision.
They may exist in a transport or audit event after a write, outside plan
identity.

### Trust extractor sensitivity and destination

Rejected. Extractors consume untrusted content and model extractors are
probabilistic. Core computes the effective, never-less-safe classification and
reuses destination policy.

### Partially apply sources from a globally blocked batch

Rejected for v1. A source-level path, size, prohibited-data, or protocol
blocker makes the batch non-routable to avoid evidence attribution mistakes.
Candidate-local duplicate, conflict, reject, and needs-review outcomes do not
globally block independently selected candidates; their mixed writes remain
transactional. The caller can split unrelated explicit sources into separate
plans when isolation is preferred.

### Write canonical records directly from a reviewed capture plan

Rejected. The accepted plan authorizes routing, not canonical truth. Repo
candidates still become reviewable proposal files and require explicit
canonical apply.

### Put credentials or provider commands in capture requests

Rejected. Requests and plans are reviewable and may be shared. They select only
an allow-listed local profile; the host owns execution and secrets.

## Deferred implementation choices

The following do not change the accepted core contract:

- #48 selects and versions the initial deterministic Markdown candidate rules
  and may tighten the proposed safeguard defaults through tests.
- #49 selects the CLI command spelling and exact optional OKF capture
  provenance encoding while retaining the route/apply boundaries here.
- #51 maps planning to MCP without adding mutations.
- #44/#47/#55 define the corpus, metric implementation, and evidence-based
  numeric quality, latency, and review-burden release thresholds.
- Shipping any particular model adapter is optional and requires a separate
  provider/privacy review plus the same eval gates.
- Directory, supplied-bytes, Git blob/range, and specialised instruction/ADR
  profiles are extension profiles. Each may ship independently after its own
  implementation and evaluation gate.

## Maintainer decision

**Accepted with profile-scoped implementation — 2026-07-10.**

The capture-plan, review, evidence, no-write, stale-validation,
classification, and routing boundaries in this RFC are normative. The v0.4
required profile is deterministic capture from one explicit UTF-8 Markdown
`project_path`. Directory, supplied-bytes, Git blob/range, specialised
instruction/ADR, and model-backed profiles remain specified extensions and
require their own implementation and evaluation gates.

Acceptance specifically includes:

- enforced plan data classification;
- separate claim and routing identities;
- human-resolvable `needs_review` cases with non-overridable hard blockers;
- candidate-specific stale preconditions;
- tiered evidence storage;
- the `defer` review outcome; and
- the existing boundary in which route apply can create a pending repo
  proposal but never canonical memory.

#48 and #49 may treat the core contract and required v0.4 profile as stable.
Extension profiles do not block their implementation.
