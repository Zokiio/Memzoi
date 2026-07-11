# Evaluation gates

## Recall v3 candidate evaluation

Recall v3 is the candidate-neutral evaluation foundation for the v0.5 hybrid
recall decision. It is additive: the checked recall-v2 baseline remains the
release regression gate while v3 development and locked-test bundles collect
graded retrieval evidence.

Run the checked development smoke corpus with the production lexical baseline:

```bash
make eval-recall-v3
```

Emit the stable JSON report and a separately reviewable digest commitment:

```bash
cargo run --locked -q -p memzoi-cli -- eval recall-v3 \
  --corpus evals/recall/v3/corpus.yaml \
  --commitment /tmp/recall-v3-commitment.json \
  --json
```

The strict `memzoi-recall-corpus/v3` schema distinguishes `development` from
`locked_test` bundles. Every case declares provenance, query slices, path and
scope inputs, top-k, context budget, and 0-3 relevance judgments. Policy
eligibility is a separate boolean with a required typed forbidden reason for
ineligible records; an ineligible record cannot receive positive relevance.
Unknown fields, unsafe fixture paths, duplicate IDs, missing records, invalid
grades, and inconsistent eligibility judgments are rejected.

The runner gives every adapter the same eligible records and request budgets,
then applies the eligibility boundary again before the shared scoring path.
Reports include NDCG@10, recall@k, MRR, hard-negative and categorized forbidden
behavior, citation integrity, normalized fallback parity, latency, optional
resource observations, and aggregate and per-slice views. Candidate comparisons
use a deterministic 10,000-sample paired bootstrap against the lexical baseline
with a fixed seed and a 95% interval.

Corpus bytes and record fixtures, judgments, metric definitions, runner
identity, and candidate manifests receive separate BLAKE3 digests. The
commitment artifact binds those identities without exposing a locked bundle.
Final locked-test execution must use an unchanged commitment; changing a case,
judgment, metric, runner, candidate manifest, or fixture changes its digest.

All production-baseline execution happens under temporary project and runtime
roots and requires no network. Candidate adapters receive typed input and return
ranked IDs, citations, separated numeric signals, fallback reason, and structured
resource counters. They must not install or download models or mutate project
memory; model-backed and fusion adapters are added by the separately gated
candidate implementation issue.

Memzoi has two checked-in, file-native evaluation suites. Recall v2 gates
retrieval, precheck, lifecycle suppression, privacy boundaries, citations, and
provenance. Capture v1 gates candidate quality, evidence, classification,
prohibited-data leakage, deterministic planning, stale-source rejection, and
human review burden. Both suites run in disposable isolated state and do not
open or mutate the current project's canonical records or normal runtime index.

Run the same gate as CI:

```bash
make eval
```

This evaluates `evals/recall/v2/corpus.yaml` and compares the result with
`evals/recall/v2/baseline.json`, then evaluates
`evals/capture/v1/corpus.yaml` against `evals/capture/v1/baseline.json`. The
command exits non-zero when a corpus or baseline is invalid, a documented gate
fails, or the capture report differs from its accepted deterministic baseline.

Run one suite while iterating:

```bash
make eval-recall
make eval-capture
```

## Recall metrics

The JSON report's versioned metric definitions are the machine-readable source
of truth. In plain language:

| Metric | Definition |
| --- | --- |
| Recall at *k* | Relevant record IDs returned in the first *k* results divided by declared relevant IDs. |
| MRR | Reciprocal rank of the first relevant result, averaged across applicable cases. |
| Forbidden-hit rate | Categorized forbidden hits divided by declared forbidden opportunities. |
| Precheck precision | Expected warnings returned divided by all warnings returned. |
| Precheck recall | Expected warnings returned divided by declared expected warnings. |
| Stale, expired, and scope leakage | Declared ineligible records that surfaced in the corresponding safety cases. Expired records are classified before other non-active lifecycle states so the categories stay disjoint. |
| Citation integrity | Returned results with structurally consistent citations that identify the same record and expected evidence. |
| Provenance integrity | Returned results whose storage plane matches their destination, whose source metadata survives intact, and whose proposal lineage remains separate from evidence. |
| Token usage | The deterministic `approx_words` estimate, with its unit and estimator version emitted in the report. `max_estimated_usage` limits the maximum per-case estimate, not the corpus total. |
| p50 and p95 latency | Nearest-rank median and 95th-percentile observed case latency from a monotonic timer. |

Quality and integrity metrics are higher-is-better; leakage and forbidden-hit
metrics are lower-is-better. Latency and runtime metadata are reported for
diagnosis but are not compared as deterministic baseline values. They affect the
exit status only when the corpus declares an explicit threshold.

## Capture metrics and hard gates

Capture reports aggregate and per-profile results for every required extractor.
The JSON report is `memzoi-capture-report/v1`; its versioned `definitions`
object is the machine-readable source of truth.

| Metric | Definition |
| --- | --- |
| Candidate precision | Expected candidates matched divided by all emitted candidates. |
| Candidate recall | Expected candidates matched divided by all declared expected candidates. |
| Evidence validity | Evidence items whose source, exact byte/line span, heading path, section kind, and content hash match the named fixture. |
| Destination, sensitivity, and action accuracy | Expected classification or routing fields matched divided by checked candidates. |
| Forbidden-hit rate | Declared prohibited candidates emitted divided by declared prohibited opportunities. |
| Unsupported-outcome accuracy | Unsupported and blocked inputs that produced the declared no-candidate status and diagnostic outcome. |
| Review burden | Proposed, accepted, rejected, edited, deferred, duplicate, conflict, and needs-review counts. No candidate content is copied into this metric. |
| Payload and latency | Observed plan byte size and monotonic-clock p50/p95 latency. These are diagnostic unless the corpus declares a limit. |

The capture suite also has non-negotiable zero-violation hard gates for
determinism, planning writes, invalid evidence, unnamed evidence sources or
undeclared policy reads, prohibited-content echoes, stale-identity acceptance,
and skipped required profiles. Every source snapshot's policy inputs must match
the case's profile- and locator-specific declarations. Thresholds cannot waive
these gates. Every case executes in its own temporary project, with network
access unnecessary.

## Add a recall case

1. Add the smallest OKF Markdown fixture under `evals/recall/v2/records/`.
   Proposal round-trip cases may also need a fixture under
   `evals/recall/v2/proposals/`. Never add secrets or private personal data.
2. List the fixture and add a uniquely named case in
   `evals/recall/v2/corpus.yaml`. Declare relevant and forbidden IDs plus the
   scope, lifecycle, precheck, citation, or provenance expectations needed by
   that case. The corpus is strict: unknown fields and missing fixture IDs are
   errors.
3. Run `make eval` and inspect the per-case result and aggregate threshold
   checks. A new or intentionally changed corpus requires an explicit baseline
   update before the comparison can pass.

Keep each case focused on one contract boundary. Prefer a target plus a close
distractor over a broad fixture dump, and use the fixed corpus evaluation clock
for expiry-boundary cases.

## Add a capture case

1. Add the smallest synthetic source fixture under
   `evals/capture/v1/fixtures/`. Canonical inventory fixtures belong under
   `evals/capture/v1/records/`. Never add real credentials, private personal
   data, raw chats, or hidden agent state; use an obvious synthetic canary when
   testing redaction.
2. Add one strict case to `evals/capture/v1/corpus.yaml`. Name the extractor
   profile and explicit source request, then declare the expected plan status,
   data class, diagnostics, candidates, exact evidence spans, classification,
   routing action, forbidden candidates, review outcomes, or stale-source
   replacement needed by that boundary.
3. Run `make eval-capture`. Inspect the per-case assertions, aggregate and
   per-profile metrics, hard gates, observations, and baseline comparison.
4. If the reviewed behavior change is intentional and every gate passes, run
   `make eval-update-capture-baseline`, inspect the exact baseline diff, and
   rerun `make eval-capture`.

Keep fixtures deterministic and independently understandable. A case should
exercise one trust boundary rather than reproduce a real repository or user
session.

## Update the baseline intentionally

After reviewing an intentional corpus, behavior, or threshold change, run:

```bash
make eval-update-baseline
```

This explicitly updates both baselines. Prefer the per-suite targets when only
one contract changed:

```bash
make eval-update-recall-baseline
make eval-update-capture-baseline
```

These are the only normal workflows that write
`evals/recall/v2/baseline.json` or `evals/capture/v1/baseline.json`; `make eval`
and CI remain read-only. Inspect every baseline diff, explain any threshold or
review-burden change in the pull request, then rerun:

```bash
make eval
cargo test --workspace
```

An update command refuses to write when the current report fails a threshold or
hard gate. Change a threshold deliberately in the corpus first when the
accepted contract itself is changing. Recall baseline metric drift is reported
for review while incompatible identity fails; capture requires exact equality
with its deterministic baseline so any unaccepted drift fails CI.

Do not copy a raw JSON report into either baseline. Reports contain observed
runtime metadata, latency, and payload observations, while checked-in baselines
are stable typed comparison projections.
