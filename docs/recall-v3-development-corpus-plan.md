# Recall v3 development-corpus plan

This document is the pre-authoring plan for issue #74. It is not a reviewed
corpus, a candidate result, or release evidence. It records the coverage audit,
the proposed public case matrix, and the decisions that must be made before
blind human review begins.

The checked-in smoke corpus currently has three cases and four records. It
covers exact terminology, citation/path behavior, one paraphrase with zero
lexical overlap, and one scope distractor. It does not yet provide the breadth
or independent judgments required by #74.

## Proposed public case matrix

Cases may carry more than one slice when each label is independently true. The
initial authoring target is 34 cases. A statistical reviewer must ratify the
minimum useful count per slice before the corpus is frozen; thin slices must be
reported as gaps rather than padded with near-duplicates.

| Family | Proposed cases | Required contrast |
| --- | ---: | --- |
| Exact terminology | 2 | Direct phrase target and a lexically similar wrong record |
| Identifiers | 2 | Exact decision/record identifier and a near-identifier negative |
| Filenames | 2 | Basename/full-path target and same-name wrong-directory negative |
| Error codes | 2 | Exact code/remediation and adjacent-code wrong remediation |
| Paraphrase | 2 | Natural-language restatement and semantic near miss |
| Zero lexical overlap | 2 | Relevant answer with verified zero query-token overlap |
| Synonyms | 2 | Correct operation and synonym-sharing wrong operation |
| Abbreviations | 2 | Correct expansion and plausible alternate expansion |
| Procedural | 2 | Correct ordered procedure and same-goal wrong-order negative |
| Causal | 2 | Supported root cause and correlated-but-not-causal negative |
| Temporal conflict | 2 | Current record against expired or superseded predecessor |
| Negation | 2 | Correct prohibition and polarity-reversed near duplicate |
| Ambiguous short query | 2 | Path/scope context disambiguates multiple plausible records |
| Multi-relevant | 2 | At least two graded positive records plus a partial answer |
| Similar-but-wrong | 2 | Explicit lexical and semantic hard-negative cases |
| Policy boundary | 4 | Scope, path, lifecycle, privacy, destination, and prohibited coverage |

## Proposed record inventory

The first authoring pass should use roughly 30 small records grouped as follows:

- exact lexical, decision-ID, record-ID, filename, and error-code targets;
- atomic release, cache revalidation, recovery-objective, TTL, certificate
  rotation, and SQLite-lock procedures or explanations;
- current and superseded delivery policies;
- separate build and search records with `crates/**` and `apps/web/**` paths;
- release build, activation, and rollback records for multi-relevant and
  similar-but-wrong cases;
- team-alpha and team-beta scope twins;
- live and boundary-expired records using the fixed evaluation clock;
- active, superseded, and tombstoned lifecycle twins; and
- public policy plus an obviously synthetic privacy canary containing no real
  private data.

Every fixture uses a stable, unique `source_ref`. Path-sensitive positives must
carry an `applies_to` value that matches the case path. Similar-but-wrong active
records remain eligible with relevance 0 and `hard_negative: true`; ordinary
irrelevant records remain eligible with relevance 0. Policy-forbidden records
have relevance 0 and the most specific typed forbidden reason.

## Authorship and provenance

Each authored case must truthfully use one of the schema provenance kinds:

- `human_written` only after a named human authors or substantively rewrites it;
- `real_failure_derived` only with a public, sanitized source reference; or
- `generated` with a non-empty `authoring_model` and a human review reference.

Agent-drafted cases remain `generated`. They must not be relabeled as human
written merely because a human approves the pull request. No evaluated
candidate or paired generator may be the sole author or judge of its test.

## Foundation gaps to resolve before bulk authoring

The audit found four issues that would otherwise make the corpus misleading or
unreasonably expensive to review:

1. Every case currently judges every globally staged record. The 34-case,
   30-record plan would create 1,020 case-record judgments per reviewer. The
   runner needs reviewable per-case record pools, while still requiring complete
   judgments for every record available to that case.
2. Staged OKF files project to repository memory. Local/session destination
   distractors therefore need typed runtime fixtures; assigning a destination
   label to a repository record would not test the real boundary.
3. Prohibited content must never be persisted as an ordinary memory record. A
   synthetic, content-free canary or write-gate fixture needs an explicit
   contract instead of a fabricated stored record.
4. The current quality thresholds let the small lexical smoke corpus pass. A
   deliberately semantic development corpus may lower lexical aggregate quality.
   Development diagnostics and locked release thresholds must be separated or
   ratified before changing those values.

The current forbidden-reason enum also has no dedicated path value. The policy
owner must decide whether path mismatch is `scope`, `other`, or a new typed
reason before reviewers label those cases.

## Human decisions required

Before generating the review packets, a maintainer and statistical reviewer
must record:

- minimum case counts and acceptable uncertainty for each required slice;
- the per-case record-pool contract;
- authentic destination, prohibited, privacy, and path-mismatch semantics;
- development diagnostic thresholds versus locked release thresholds;
- the public sources permitted for real-failure-derived cases; and
- reviewer, adjudicator, and coordinator assignments.

## Execution sequence

1. Resolve the foundation gaps above without changing locked-test custody.
2. Author the public records and cases with truthful provenance.
3. Produce immutable reviewer packets without judgments or candidate output.
4. Collect independent Reviewer A and Reviewer B submissions using the
   [reviewer guide](recall-v3-reviewer-guide.md).
5. Adjudicate disagreements and publish agreement statistics without erasing
   either original submission.
6. Apply adjudicated judgments, run the production evaluator, and commit the
   corpus, fixtures, review artifacts, report, and commitment digests together.
7. Only then begin candidate tuning and configuration freeze under issue #77.
