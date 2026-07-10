# Trust evaluation

Memzoi's checked-in evaluation corpus is a local and CI release gate for recall,
precheck, lifecycle suppression, privacy boundaries, citations, and provenance.
It runs from disposable state and does not open or mutate the current project's
canonical records or normal runtime index.

Run the same gate as CI:

```bash
make eval
```

This evaluates `evals/recall/v2/corpus.yaml` and compares the result with
`evals/recall/v2/baseline.json`. The command exits non-zero when the corpus or
baseline is invalid, their identities do not match, or a documented threshold
fails.

## Metrics

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
| Token usage | The deterministic `approx_words` estimate, with its unit and estimator version emitted in the report. |
| p50 and p95 latency | Nearest-rank median and 95th-percentile observed case latency from a monotonic timer. |

Quality and integrity metrics are higher-is-better; leakage and forbidden-hit
metrics are lower-is-better. Latency and runtime metadata are reported for
diagnosis but are not compared as deterministic baseline values. They affect the
exit status only when the corpus declares an explicit threshold.

## Add a case

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

## Update the baseline intentionally

After reviewing an intentional corpus, behavior, or threshold change, run:

```bash
make eval-update-baseline
```

This is the only normal workflow that writes
`evals/recall/v2/baseline.json`; `make eval` and CI remain read-only. Inspect the
baseline diff, explain any threshold change in the pull request, then rerun:

```bash
make eval
cargo test --workspace
```

The update command refuses to write when the current report fails a threshold.
Change a threshold deliberately in the corpus first when the accepted contract
itself is changing.

Do not copy a raw JSON report into the baseline. Reports contain observed
runtime metadata and latency, while the checked-in baseline is the stable,
typed comparison projection.
