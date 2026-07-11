# Recall v3 public-corpus reviewer guide

This guide is for the blind human review of the public `development` corpus in
issue #74. It defines how reviewers assign relevance and eligibility judgments
before candidate tuning. It does **not** cover locked-test authoring, custody,
sealing, access control, or execution; those responsibilities belong to issue
#75 and a separate custodian.

## Roles and separation

- The **review coordinator** prepares a packet containing the case query,
  path/scope inputs, evaluation time, and the records to judge. The packet must
  omit existing judgments and all retrieval results.
- **Reviewer A** and **Reviewer B** work independently. Where two reviewers are
  not possible, document why; at least one reviewer must be independent of
  candidate implementation.
- The **adjudicator** compares the submitted reviews only after both are final,
  resolves disagreements, and records a rationale. A candidate implementer may
  provide policy context but must not be the sole adjudicator or unilaterally
  relabel a result.
- The **corpus editor** applies the adjudicated judgments and runs validation.
  This may be the coordinator, but must not alter the adjudicated result without
  reopening and documenting review.

Reviewers must not see candidate rankings, scores, model names, model output,
fusion output, retrieval traces, baseline results, another reviewer's answers,
or the current corpus judgments while reviewing. Do not run the evaluation
runner until your independent submission has been accepted by the coordinator:
its report contains ranked retrieval output.

## What each reviewer receives

The coordinator provides the same immutable packet to both reviewers:

- case ID and query;
- declared slices and provenance;
- evaluation timestamp;
- path, scope kind, and scope ID, including explicit absence;
- top-k and context budget for context only; and
- the complete candidate record set with record IDs, metadata, content, and
  source citations needed to judge policy and usefulness.

Confirm the packet digest or version before starting. Stop and ask the
coordinator if a record is missing, unreadable, ambiguous, or contains data that
should not be in a public repository. Do not infer missing content.

## Judge eligibility separately

Eligibility is the policy boundary: may this record be returned for this query,
evaluation time, path, scope, and destination? Decide it from record metadata and
case inputs, not from how relevant the content is.

- Set `eligible: true` and omit `forbidden_reason` when the record is allowed.
- Set `eligible: false`, set exactly one `forbidden_reason`, and set
  `relevance: 0` when policy forbids the record.
- Do not make an irrelevant but allowed record ineligible. It is eligible with
  relevance 0.
- Do not make a highly useful but forbidden record eligible. Policy wins over
  usefulness.

Use one of these exact snake-case reasons:

| Reason | Use when |
| --- | --- |
| `stale` | The record is no longer current or authoritative for the case, without a more specific lifecycle state below. |
| `expired` | Its expiry time has passed at the declared evaluation time. |
| `scope` | Its scope kind or scope ID is outside the case's requested scope. |
| `destination` | Its memory destination is not allowed by the request boundary. |
| `private` | Its visibility or privacy boundary forbids exposure in this context. |
| `prohibited` | Policy forbids storing or returning the represented sensitive content at all. |
| `tombstoned` | The record has been explicitly deleted or tombstoned. |
| `superseded` | A replacement has made this record non-current. |
| `other` | A policy boundary forbids the record but none of the typed reasons applies; state the boundary explicitly in the rationale. |

Prefer the most specific applicable reason. If two reasons appear equally
applicable, flag the judgment for policy adjudication rather than silently
choosing one. Never use `other` merely because the decision is uncertain.

## Assign relevance from 0 to 3

For an eligible record, judge how well its authoritative content answers the
query under the supplied path and scope. Ignore keyword overlap, retrieval rank,
and which architecture might find it.

| Grade | Meaning |
| --- | --- |
| `0` | Not useful for answering the query, including a plausible but wrong hard negative. |
| `1` | Marginally useful: related context, but incomplete or requiring substantial inference. |
| `2` | Useful: answers a meaningful part of the query, though another record or inference is needed for a complete answer. |
| `3` | Directly useful: clearly and substantially answers the query on its own. |

Use the same rubric for lexical matches, paraphrases, synonyms, abbreviations,
identifiers, procedures, causal questions, and multi-relevant cases. An eligible
record with grade 0 may be marked `hard_negative: true` only when it is
deliberately similar-but-wrong and therefore tests discrimination. Each case
must ultimately contain at least one eligible record with positive relevance.

List `expected_citations` only when the record should cite those exact sources
for this case. Use the citation identifiers as written in the record. Do not
invent a citation or copy one from an existing judgment.

Every judgment needs a concise rationale that cites observable record or policy
facts. “Seems relevant” and “model should find this” are not sufficient.

## Independent submission and adjudication

For every case-record pair, submit:

```yaml
case_id: <case ID>
record_id: <record ID>
eligible: true
forbidden_reason: null
relevance: 0
expected_citations: []
hard_negative: false
rationale: <brief evidence-based explanation>
```

Use reviewer-specific files or forms. Send them directly to the coordinator;
do not place either submission in a location visible to the other reviewer.
Submissions become final before comparison. The coordinator records reviewer
identity or a stable pseudonym, independence from candidate implementation,
packet digest, submission time, and any declared conflict.

The adjudicator compares fields, not just final grades. For each disagreement,
record both original answers, the adjudicated answer, the policy or record
evidence used, and a concise rationale. Never erase the independent inputs.
Material changes to a reviewed case or record require another review and new
corpus and judgment digests.

## Agreement report

Publish agreement before and after adjudication, without rewriting the original
reviewer values. Report the total number of case-record pairs and, overall and
per required slice where sample size permits:

- exact eligibility agreement and Cohen's kappa;
- exact forbidden-reason agreement among pairs both reviewers marked
  ineligible, plus the reason confusion counts;
- exact relevance agreement, agreement within one grade, and ordinal weighted
  Cohen's kappa for grades 0–3;
- disagreement counts by field and the number and percentage changed by
  adjudication; and
- the number of cases, records, reviewers, single-reviewed judgments, and
  missing judgments.

State the weighting convention and calculation tool. Report `n/a` with the
reason when a statistic is undefined; do not replace it with zero. Publish low
agreement and thin slices as gaps to fix, rather than padding or suppressing
them.

## Privacy and benchmark integrity

The development corpus is public. Do not include secrets, credentials, tokens,
private personal data, raw chats, customer data, repository-confidential source,
or provider metadata. Use synthetic or redacted fixtures when a real failure
contains restricted material, and record truthful provenance for the resulting
case. An evaluated candidate or its paired generator must not be the sole author
of its test.

Do not ask a candidate model to label, rewrite, or resolve its own evaluation
case. Do not share review packets or submissions beyond the named reviewers,
coordinator, and adjudicator before adjudication. Candidate rankings, model
names, scores, fusion output, and retrieval traces remain hidden until both
independent submissions are final.

## Validation after adjudication

Only the corpus editor or coordinator runs these commands, from the repository
root, after the blind submissions are final and adjudicated.

Validate the public corpus with the production lexical baseline:

```bash
make eval-recall-v3
```

Generate a reviewable report and digest commitment:

```bash
cargo run --locked -q -p memzoi-cli -- eval recall-v3 \
  --corpus evals/recall/v3/corpus.yaml \
  --commitment /tmp/recall-v3-commitment.json \
  --json > /tmp/recall-v3-report.json
```

Run the complete v0.5 evaluation contract before proposing the corpus change:

```bash
make eval-v0.5-foundation
```

Review the command exit status, report, and commitment artifact. Commit the
development corpus, record fixtures, adjudicated judgments, agreement report,
and digests together. Validation success checks structure and runner behavior;
it does not replace human review or agreement reporting.

## Completion checklist

- [ ] The packet version or digest is recorded and identical for both reviewers.
- [ ] Reviewers saw no existing judgments, peer answers, rankings, scores,
      model names, model output, fusion output, or retrieval traces.
- [ ] Two independent reviews were collected where possible, with at least one
      reviewer independent of candidate implementation; exceptions are stated.
- [ ] Every case-record pair has eligibility, relevance, citation, hard-negative,
      and rationale fields.
- [ ] Every ineligible record has relevance 0 and exactly one typed forbidden
      reason; every eligible record omits the reason.
- [ ] Every case has at least one eligible record with positive relevance.
- [ ] Disagreements were adjudicated with both originals and a rationale kept.
- [ ] Overall and per-slice agreement statistics, denominators, undefined
      statistics, and single-review gaps are published.
- [ ] Provenance is recorded and no evaluated candidate is the sole test author.
- [ ] Public files contain no secrets, private personal data, raw chats, or
      confidential source material.
- [ ] `make eval-recall-v3` passes.
- [ ] The JSON report and commitment were generated and reviewed.
- [ ] `make eval-v0.5-foundation` passes.
- [ ] Any substantive correction triggers review and new corpus/judgment digests.
- [ ] Locked-test custody has not been treated as part of this guide or workflow.
