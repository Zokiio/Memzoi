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
| `memzoi capture` | Plan evidence-backed capture from one explicit Markdown, instruction, ADR, or Git-change source; record a complete review; and route reviewed candidates. |
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
| `memzoi eval recall` | Evaluate a versioned file-native trust corpus in disposable isolated state. |
| `memzoi eval capture` | Evaluate capture quality, safety, and review burden in disposable isolated state. |
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
| `capture plan` | `--source <project-relative.md>` or `--request-file <capture-request.{json,yaml}>`, `--source-bytes <path\|->` for `supplied_bytes`, `--source-id`, `--output`, `--json` |
| `capture review` | `--plan-file`, `--decisions-file`, `--prior-review-file`, `--source-bytes <path\|->` when replaying `supplied_bytes`, `--reviewed-by`, `--reviewed-at`, `--output`, `--json` |
| `capture apply` | `--plan-file`, `--review-file`, `--prior-review-file`, `--source-bytes <path\|->` when replaying `supplied_bytes`, `--plan-id`, `--review-id`, `--actor`, `--json` |
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
| `eval recall` | `--corpus <path>`, `--baseline <path>`, `--update-baseline`, `--json` |
| `eval capture` | `--corpus <path>`, `--baseline <path>`, `--update-baseline`, `--json` |
| `quickstart` | `--apply-sample`, `--json` |
| `update` | `--check`, `--ref`, `--json` |
| `mcp config` | `--project-root` |
| `integrate list` | `--json` |
| `integrate prompt` | `--profile` |
| `integrate instructions` | `--profile`, `--file`, `--json` |

## Recall evaluation

Run the checked-in trust corpus without opening or mutating the current
project's canonical records, proposal inbox, runtime database, exports, or event
log:

```bash
memzoi eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json
memzoi eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json --json
```

`--baseline` is optional. `--update-baseline` requires it and is the only mode
that writes the selected baseline. A threshold-failing run is never written:

```bash
memzoi eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json --update-baseline
```

The explicit corpus is strict YAML with version
`memzoi-recall-corpus/v2`. It references OKF Markdown, proposal, and private
runtime fixtures relative to the corpus, fixes the evaluation clock, declares
aggregate thresholds, and defines tagged cases. Unknown fields are rejected.
The following abridged excerpt shows the search and precheck shapes; a complete
v2 trust corpus must also declare proposal/runtime fixtures, context and
write-gate cases, and forbidden opportunities for every required safety
category:

```yaml
version: memzoi-recall-corpus/v2
name: project-trust-v2
evaluated_at: 2026-07-10T12:00:00Z
records_root: records
records:
  - package-manager.md
  - package-manager-warning.md
  - unrelated-package-manager.md
thresholds:
  min_mean_recall_at_k: 1.0
  min_mean_mrr: 1.0
  min_precheck_precision: 1.0
  min_precheck_recall: 1.0
  max_stale_leakage_rate: 0.0
  max_expired_leakage_rate: 0.0
  max_scope_leakage_rate: 0.0
  max_forbidden_hit_rate: 0.0
  min_citation_integrity: 1.0
  min_provenance_integrity: 1.0
  min_case_pass_rate: 1.0
  max_estimated_usage: 500 # Per-case maximum; corpus total is reported separately.
  # max_p95_latency_ms: 50
cases:
  - surface: search
    id: package-manager-decision
    query: package manager
    relevant_ids: [package-manager]
    forbidden:
      scope: [unrelated-package-manager]
    scope_kind: repo
    type: decision
    lane: semantic
    path: package.json
    k: 5
  - surface: precheck
    id: package-manager-precheck
    path: package.json
    scope_kind: repo
    relevant_ids: [package-manager-warning]
```

Case `surface` selects one strict shape:

- `search` accepts a query, top-k limit, relevant IDs, categorized forbidden
  IDs, optional scope/type/lane/path filters, and an optional proposal fixture.
- `precheck` accepts path/action/command inputs, scope, and expected warning IDs.
- `context` accepts task/path/budget inputs, local/session opt-ins, and expected
  included or forbidden destinations.
- `write_gate` declares a prohibited candidate, expected policy issue code, and
  a record ID that must remain absent.

JSON output uses `memzoi-recall-report/v2`. Its `definitions` object explains
the versioned formulas, while `runtime` reports the Memzoi/SQLite environment,
timer, isolated-state guarantee, and estimator. `metrics` contains:

- search case count, mean recall at k, and mean MRR;
- micro precheck precision/recall with true-positive, false-positive, and
  false-negative counts;
- stale, expired, scope, prohibited, destination, and total forbidden leakage
  as hits, opportunities, and rates;
- citation and provenance integrity as valid/checked ratios;
- deterministic `approx_words` usage totals and distribution;
- nearest-rank p50/p95 monotonic-clock latency; and
- the overall case pass ratio.

Empty precision/recall/integrity denominators resolve to `1.0`; empty leakage
denominators resolve to `0.0`. Threshold comparisons use the underlying values,
not their display rounding.

The typed `memzoi-recall-baseline/v1` artifact contains only deterministic
metrics and per-case outcomes. Runtime metadata and observed latency are not
exact-compared. A baseline comparison has status `match`, `changed`, or
`incompatible`: deterministic changes are reported for review but remain
informational, while an incompatible corpus/schema identity fails the report.
Corpus thresholds remain the regression gate. A valid corpus prints its full
report before a threshold or baseline failure returns non-zero; corpus or
fixture validation errors return non-zero without a report.

## Capture evaluation

Run the checked-in capture quality gate from isolated temporary projects:

```bash
memzoi eval capture \
  --corpus evals/capture/v1/corpus.yaml \
  --baseline evals/capture/v1/baseline.json

memzoi eval capture \
  --corpus evals/capture/v1/corpus.yaml \
  --baseline evals/capture/v1/baseline.json \
  --json
```

The strict `memzoi-capture-corpus/v1` YAML names every required extractor
profile, explicit source fixture, expected candidate and exact evidence span,
classification, routing action, forbidden candidate, review outcome, and
optional stale-source check. Unknown fields, escaping fixture paths, duplicate
IDs, invalid expectations, and unaccounted profiles are rejected.

The `memzoi-capture-report/v1` report contains aggregate and per-profile
candidate precision/recall, evidence validity, destination/sensitivity/action
accuracy, forbidden-hit rate, unsupported-outcome accuracy, review-burden
counts, payload observations, and p50/p95 latency. Its non-waivable hard gates
require deterministic no-write planning, valid evidence from named sources, no
unnamed evidence or undeclared policy reads, no prohibited-content echo,
stale-source identity rejection, and execution of every required profile.

`--baseline` is optional. When present, capture requires an exact match with the
typed `memzoi-capture-baseline/v1` deterministic projection; unlike observed
latency and payload metadata, any changed deterministic metric, profile
fingerprint, hard gate, or case outcome fails. Update an accepted change only
after every gate passes:

```bash
memzoi eval capture \
  --corpus evals/capture/v1/corpus.yaml \
  --baseline evals/capture/v1/baseline.json \
  --update-baseline
```

See the [evaluation contributor guide](https://github.com/Zokiio/Memzoi/blob/main/docs/evaluation.md)
for metric definitions and fixture guidance.

## Evidence-backed capture

Capture turns one explicitly named project source into evidence-linked memory
candidates without ambient repository scanning or inference from chat, shell
history, or hidden agent state. The legacy `--source` shorthand selects the
`markdown-deterministic` profile; `--request-file` accepts the complete strict
JSON or YAML request needed by instruction, ADR, and Git-change profiles. Its
three CLI stages keep extraction, human judgment, and writes separate:

```bash
memzoi capture plan \
  --source notes/session-findings.md \
  --source-id session-findings \
  --output capture-plan.json \
  --json

memzoi capture review \
  --plan-file capture-plan.json \
  --decisions-file capture-decisions.json \
  --reviewed-by zoki \
  --reviewed-at 2026-07-10T12:00:00Z \
  --output capture-review.json \
  --json

memzoi capture apply \
  --plan-file capture-plan.json \
  --review-file capture-review.json \
  --plan-id capture_... \
  --review-id review_... \
  --actor zoki \
  --json
```

`plan` and `review` do not write memory state. `--output` optionally writes the
complete JSON artifact, while `--json` prints it; without `--json`, the command
prints a human-readable view. Request, plan, decision, and review artifacts
must be regular, nonsymlink UTF-8 files no larger than 2 MiB. Artifact output
is installed without replacing an existing path. The artifact's data class
also constrains where it may be saved, as described below.

### Deterministic Markdown profile

The `markdown-deterministic` profile accepts exactly one regular UTF-8 `.md` file named by a POSIX
project-relative path. Absolute paths, traversal components, backslashes, `.memzoi`, symbolic
links, non-Markdown files, and files larger than 1 MiB are rejected. The source is read only from
the current project; capture never searches for additional inputs. The profile also caps a plan at
100 candidates, 4,096 Markdown headings, 16 KiB per evidence item, 256 KiB of total evidence, a
bounded 10,000-file/32 MiB duplicate inventory, and a serialized plan just under 2 MiB.
The extractor profile is `markdown-deterministic`. Plans identify the concrete extractor as
`id: memzoi-markdown`, together with its version and configuration hash.

Capture file access and private artifact saving currently require Unix handle-relative,
no-symlink primitives. Windows builds fail these capture operations closed; the rest of the CLI
remains available there.

The extractor recognizes ATX headings outside fenced code blocks. A heading prefix determines
the type, lane, destination, and sensitivity of the section that follows it:

| Heading prefix | Type and lane | Planned route |
| --- | --- | --- |
| `Fact:` | `fact`, `semantic` | Repo-safe pending proposal |
| `Decision:` | `decision`, `semantic` | Repo-safe pending proposal |
| `Procedure:` | `procedure`, `procedural` | Repo-safe pending proposal |
| `Warning:` | `warning`, `semantic` | Repo-safe pending proposal |
| `Failed attempt:` | `failed_attempt`, `episodic` | Repo-safe pending proposal |
| `Risk:` | `risk`, `semantic` | Repo-safe pending proposal |
| `Preference:` | `preference`, `semantic` | Local-only runtime record |
| `Episode:` | `episode`, `session` | Temporary session runtime record |

For example:

```markdown
## Decision: Verify downloaded release archives

Verify the SHA-256 checksum before extracting a release archive.
```

Each candidate contains the exact source locator, source and evidence hashes, byte and line
spans, heading kind, extractor identity, and deterministic claim/candidate identities. Planning
also compares candidates with canonical records, pending proposals, active runtime memory, and
earlier candidates in the same source. Exact matches become no-write duplicates; same-scope,
same-title disagreements become conflicts requiring lifecycle resolution. A document without a
recognized typed heading becomes `needs_review` with unknown sensitivity rather than being
silently routed. In a mixed document, nonempty preamble text and untyped sections produce
identity-covered `unsupported_markdown_content` diagnostics with their source ID and starting
line, so typed extraction cannot silently hide unsupported regions.

The same source bytes and relevant memory inventory produce the same plan. The `plan_id` pins the
request, source snapshot, extracted candidates, duplicate/conflict match sets, reserved proposal
IDs, policy/configuration versions, and preconditions. Planning opens existing runtime inventory
read-only and does not create or change `.memzoi/`, SQLite, proposal, export, or event state.
If runtime inventory is missing or cannot be read safely, affected local/session candidates become
`needs_review` no-write actions with a stable warning. Unaffected repo-only candidates retain the
same identity they would have against an empty runtime inventory.

### Instruction, ADR, and Git-change profiles

Extension profiles use a complete `memzoi/capture-request-v1` artifact. For
example, this request captures one explicitly named agent instruction file:

```yaml
schema: memzoi/capture-request-v1
sources:
  - source_id: agent-rules
    locator:
      kind: project_path
      path: AGENTS.md
    media_type: text/markdown
extractor:
  profile: instruction-deterministic
```

```bash
memzoi capture plan --request-file capture-request.yaml --output capture-plan.json --json
```

The profiles and accepted source shapes are closed sets:

| Profile | Accepted explicit source | Extraction and routing boundary |
| --- | --- | --- |
| `instruction-deterministic` | One `project_path` whose basename is `AGENTS.md` or `CLAUDE.md` | Preamble and nonempty sections become scoped procedures by default; typed headings retain their typed memory mapping. A nested instruction file scopes candidates to its parent directory. Memzoi-generated marker blocks and whole generated projections are excluded. Temporary, session, WIP, scratch, personal, private, or local-only markers in headings, preambles, or section bodies become `needs_review` with unknown sensitivity. |
| `adr-deterministic` | One Markdown `project_path`, or one `project_directory` with `ignore_policy: git-v1` and `include: ["*.md"]` | Recognizes ADR context, decision, consequences, risk, and supersession fields. Accepted/adopted/approved ADR fields may route repo-safe, except supersession always requires lifecycle review. Draft, rejected, superseded, deprecated, or unknown status remains `needs_review`. |
| `git-change-deterministic` | One `.diff`/`.patch` `project_path` plus explicit Git context, one `supplied_bytes` descriptor plus explicit Git context and transport bytes, or one immutable `git_range` | Parses strict unified Git diffs and extracts typed added `Decision`, `Procedure`, `Warning`, `Risk`, and `Failed attempt` sections. Typed deleted guidance is preserved as an old-side `needs_review` candidate and cannot route directly to repo memory. Evidence records revisions, blobs, old/new paths, change kind, hunk identity, side, and line coordinates. Unsupported additions and rename-only changes produce diagnostics instead of speculative memory. |

ADR directory capture sorts a bounded set of Markdown members, follows the
repository's `.gitignore` policy, never enters `.git` or `.memzoi`, and snapshots
both member content and ignore-policy inputs. Its locator shape is:

```yaml
locator:
  kind: project_directory
  path: docs/adr
  recursive: true
  ignore_policy: git-v1
  include: ["*.md"]
```

Git-change sources never infer revision identity from ambient `HEAD`. Git range
rendering requires Git 2.43 or newer and is capped at 512 changed files and
4,096 diff hunks. A
project diff names `git.repository`, `git.base`, and `git.head`; a `git_range`
instead carries `repository`, full base/head object IDs, `merge_parent`
(`base_to_head` or `first_parent`), `rename_detection`, and
`diff_format: git-unified-v1` inside the locator. The range loader resolves and
pins commit objects, runs a bounded deterministic local Git diff with quoted
paths and attributes pinned, and does not change the worktree, index, refs, or
configuration. The bounded local repository configuration is prohibited-scanned,
rejects external includes, and is identity-covered; inherited Git tracing and
configuration environments are cleared. Applicable `.gitignore` files are read
only from the explicitly named head tree and are likewise prohibited-scanned and
identity-covered; project and supplied diff sources do not consult ambient
worktree ignore files. Combined and binary diffs, non-regular evidence modes,
unsafe paths, and unsupported diff forms fail closed.

A `supplied_bytes` request additionally pins a safe display name,
`media_type: text/x-diff`, exact byte length, and
`blake3:<64-lowercase-hex>` source content hash. The bytes are transported
separately and are never read from ambient stdin. Pass the same exact bytes at
all three trust boundaries:

```bash
memzoi capture plan \
  --request-file supplied-diff-request.yaml \
  --source-bytes reviewed.diff \
  --output capture-plan.json \
  --json

memzoi capture review \
  --plan-file capture-plan.json \
  --decisions-file capture-decisions.json \
  --source-bytes reviewed.diff \
  --reviewed-by zoki \
  --reviewed-at 2026-07-11T12:00:00Z \
  --output capture-review.json \
  --json

memzoi capture apply \
  --plan-file capture-plan.json \
  --review-file capture-review.json \
  --source-bytes reviewed.diff \
  --plan-id capture_... \
  --review-id review_... \
  --actor zoki \
  --json
```

Use `--source-bytes -` only to select stdin explicitly. Missing, extra,
changed, oversized, symlinked, or non-regular transport bytes fail before a
review or write. Project-path, directory, and Git-range requests reject
`--source-bytes`.

### Data classes and review

Every plan and review has one conservative `data_class`:

- `repo_safe` means every routeable candidate is explicitly repo-safe and repo-bound. The
  artifact may be saved to a normal review location, but never under `.memzoi`, the private
  runtime directory, or generated exports.
- `private` means the artifact contains or derives from local/session/private or unresolved
  material. CLI output may be printed, but `--output` is accepted only under the project's private
  runtime directory, never under the project root or generated exports.
- `blocked` means a prohibited credential, known secret token, private key, private-personal-data,
  or raw-transcript pattern was found. The redacted plan omits source snapshots, candidates, and
  evidence text, reports only safe diagnostics, cannot be reviewed, and may only be emitted to
  standard output.

The strict review-input artifact must decide every candidate exactly once. This JSON example
accepts one candidate:

```json
{
  "schema": "memzoi/capture-review-input-v1",
  "plan_id": "capture_...",
  "decisions": [
    {
      "candidate_id": "candidate_...",
      "outcome": "accept"
    }
  ]
}
```

Outcomes are `accept`, `reject`, `edit`, and `defer`. Accept keeps a routeable extracted
candidate. Reject and defer produce no write. Edit requires a complete replacement memory draft
and may request a destination; policy is reapplied to the edited candidate. Duplicates cannot be
accepted as new memory, conflicts require separate lifecycle resolution, and a no-write candidate
must be edited, rejected, or deferred. `reviewed_by` must be non-empty and `reviewed_at` must be an
explicit RFC 3339 time. The resulting `review_id` pins the plan, reviewer, time, complete decision
set, and any reviewed candidate edits.

A later review may replace deferred decisions only. Set `prior_review_id` in the next
`capture-review-input-v1` artifact and pass the complete predecessor with
`--prior-review-file <capture-review.json>`. Core verifies the prior review identity, requires the
same plan, preserves every terminal decision byte-for-byte after normalization, and binds the new
review ID to its predecessor. Applying that later review also requires the immediate predecessor
through `capture apply --prior-review-file`; apply repeats the lineage validation at the locked
transaction boundary. The v0.4 profile supports one predecessor hop. A review whose predecessor
already names an earlier review is rejected until a future interface can carry and validate the
complete ancestor chain.

Review recomputes the plan before creating an artifact. Apply validates the supplied plan and
review identities, reconstructs the review, and recomputes current source/inventory preconditions
again before writing and after acquiring the repo lifecycle lock when needed. A changed source,
new duplicate/conflict, consumed proposal ID, altered artifact, or mismatched expected ID is a
stale zero-write error.

### Apply routing and provenance

Only accepted or edited routeable candidates are considered during `capture apply`:

- A `repo`/`repo-safe` candidate creates a pending OKF packet under
  `.memzoi/proposals/pending/`. Capture never writes it directly to `.memzoi/records/`; validate,
  review, and explicitly apply that packet with `memzoi proposal-files apply <proposal-id>`.
- A `local` candidate creates a private local runtime record.
- A `session` candidate creates a private session runtime record.
- Rejected, deferred, duplicate, conflicting, blocked, and unresolved candidates write nothing.

Proposal-file and runtime writes are one crash-recoverable guarded operation. A content-free,
fsynced journal and a SQLite commit marker let the next service open roll back an interrupted
uncommitted batch or finish a committed proposal install without exposing private bodies in the
journal. The result uses schema
`memzoi/capture-apply-result-v1` and lists each proposal file or runtime record written.

Capture provenance records the plan/review, original and reviewed candidate identities, extractor,
evidence locator/spans/hashes, confidence, destination, sensitivity, and review outcome. Pending
proposal packets retain the review evidence. When a proposal is applied, canonical OKF keeps a
compact form without copied evidence text; its evidence identity and lineage remain available to
rebuild, recall citations, and later audits. Private runtime records retain the same provenance
through runtime preservation and rebuild.

MCP exposes only the original read-only Markdown/project-path planner as
`plan_capture_v1`; instruction, ADR, directory, supplied-byte, and Git-range
requests remain CLI-only and are rejected at the MCP boundary. MCP deliberately
exposes no capture review or apply tool and denies `private` results by default. See
[MCP and agent integration](./mcp-and-agent-integration.md#plan_capture_v1-contract).

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
    sensitivity: repo-safe          # repo-safe | local-only | sensitive | secret |
                                    # raw-transcript | private-personal-data |
                                    # temporary-state | unknown; omitted => unknown
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
Only a `repo` candidate with `sensitivity: repo-safe` can create a pending proposal.
Omitted sensitivity normalizes to `unknown`; any other repo sensitivity produces a
structured `blocked`/no-write result. Scope paths have the same project-relative
validation, and tags cannot be empty.

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
`title`, `body`, explicit `sensitivity`, `scope`, `tags`, a trimmed-body BLAKE3
`content_hash`, `duplicates`, and `action`.

Blocked non-repo-safe candidates use classification-only placeholders for title, body,
reason, tags, and scope metadata; their original content is represented only by the
`content_hash`. Because manifest `sources` are document-wide rather than candidate-scoped,
the plan omits all source locators when any repo candidate is blocked. It also blocks every
other repo candidate in that manifest with guidance to split the manifest before retrying;
this prevents a partial repo write from consuming ambiguous provenance. Local and session
candidates may still create private runtime records, while blocked repo candidates create no
proposal or canonical file.

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

`list`, `show`, and `validate` are read-only. They share the same contained inventory as apply, reject, replay, and doctor: symlinked proposal roots are refused without reading outside content, packet/file identities must be globally unique, and a resolved identity cannot return to pending under another filename. `list` and `validate` describe the pending inbox; `show` can also inspect a resolved packet. `validate` includes target existence, active-state, scope, and freshness checks for repo-safe supersede/tombstone packets, while non-repo-safe packets are invalid with classification-only remediation. Sensitivity is preflighted before the rest of a packet is parsed, so a malformed packet already classified as non-repo-safe is represented by a generic, structurally parseable receipt rather than echoing malformed fields.

`apply` accepts a `status: proposed`, `sensitivity: repo-safe` packet, holds the repo lifecycle lock, writes its canonical changes and derived SQLite rows with rollback for reported failures, then moves the packet to `.memzoi/proposals/resolved/applied/`. Create writes one active record; supersede preserves the target as `superseded` and creates one lineage-linked active replacement; tombstone preserves the target evidence with `status: tombstoned`. `reject` holds the same lock, creates no canonical record, and moves the packet to `.memzoi/proposals/resolved/rejected/` with an explicit reason. A rejected non-repo-safe packet is archived as a create-shaped hash receipt: its original title, body, source, scope, authorship, action target, lineage, proposal ID, and file ID are not copied into Git-visible history or command output. The receipt uses deterministic `redacted-identity-…` identities, and replay can match either original alias by hashing the lookup without printing it. Repeating an applied outcome checks create/replacement bytes plus lifecycle status, scope, and lineage while treating current canonical target bytes as file-native source of truth; it repairs relational and full-text SQLite drift transactionally. Repeating a rejection is an auditable no-op, and requesting the opposite outcome is refused. Session-end and import proposal writers hold the same lifecycle lock while reserving identities and installing pending files. Reported rollback or cleanup failures are surfaced. The multi-file filesystem and SQLite operation is not crash-atomic across process termination or power loss; `memzoi doctor` warns about index drift and hidden transaction artifacts without printing unsafe artifact identities.

Git-plane apply blocks every value except `repo-safe`, including `secret`, `sensitive`, `local-only`, `raw-transcript`, `private-personal-data`, `temporary-state`, and `unknown`; there is no override flag. Missing legacy sensitivity is treated as `unknown`. Classify or sanitize blocked proposals before repo apply, or route local/session content to the runtime plane.

With `--json`, sensitivity-blocked `apply`, `supersede`, and `proposal-files apply`
commands exit nonzero after emitting a content-free error object on stdout. The envelope
uses `ok: false` and an `error` object containing `code: repo_sensitivity_required`, the
operation, classification, message, and next step; proposal bodies and other rejected fields
are not included.

## Local runtime memory

Local memory commands:

```bash
memzoi local add --type preference --title "..." --body "..."
memzoi local list
memzoi local search <query>
```

Local records are stored in the repository-shared runtime database under `${MEMZOI_HOME:-~/.memzoi}/projects/<repository-key>/shared.db`. They are visible from every linked worktree and are marked as `destination: local`, `visibility: private`, and `source_kind: memzoi-local` in JSON output.

Local records are not written to `.memzoi/records/**`, are not returned by global `memzoi search`, and are not exported into repo-shared agent files. `memzoi context` is repo-only by default and includes local records only with `--include-local`. Use later proposal workflows to promote local memory into repo-shared memory.

## Session checkpoints

Checkpoint commands:

```bash
memzoi checkpoint add --task "..." --note "..."
memzoi checkpoint add --task "..." --from-file notes.md
memzoi checkpoint list
```

Checkpoints are stored in the repository-shared runtime database under `${MEMZOI_HOME:-~/.memzoi}/projects/<repository-key>/shared.db`. They are visible from every linked worktree and are marked as `destination: session`, `lane: session`, `type: episode`, `visibility: private`, and `source_kind: memzoi-checkpoint` in JSON output.

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

Memzoi validates the whole batch and prepares repo proposal files before writing. `repo`
candidates must be `repo-safe` and become pending `.memzoi/proposals/pending/*.md`
proposal files only; omitted sensitivity normalizes to `unknown`. If any repo candidate is
not repo-safe, the command returns structured blocked results, redacts that candidate's
title from output, and performs no writes for the entire batch. Otherwise, `local`
candidates create private runtime records and `session` candidates create runtime
checkpoint records. Runtime row writes are transactional, and created proposal files are
cleaned up if a later promotion step fails. `discard` and `needs_review` candidates create
no writes.

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

DB proposal transitions are monotonic: repeated approval or rejection of the
same current state is idempotent, while terminal `applied` and `rejected`
proposals cannot be reopened.

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
