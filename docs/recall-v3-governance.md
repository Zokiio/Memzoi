# Recall v3 code-based evaluation workflow

Status: evaluation foundation complete; semantic ship decision deferred 2026-07-17

Recall v3 is maintained by one developer and is validated with deterministic,
reproducible code and fixtures. External user testing and independent human
review are welcome later, but are not prerequisites for this work.

## Maintainer decision

v0.5 closes as a lexical-first evaluation-foundation release. The maintainer
deferred semantic and hybrid productization, the locked D56-4 ship decision in
#80, and the parent RFC in #56 because semantic retrieval and real competitor
benchmarking are not current product priorities. The checked-in synthetic
competitor fixture validates the harness contract; it is not comparative
product evidence.

This deferral is not a `no_go`, `conditional_go`, or `full_go` result. No locked
result was substituted, no semantic architecture was selected, and normal
product operation remains lexical. The foundation stays available if semantic
recall later becomes an explicit priority.

Reactivation requires a freshly scoped decision issue, dependencies aligned
with the evidence actually required, verification of the sealed inputs and
frozen candidate identities, and a locked run with no post-result tuning.

## Development corpus

1. Add deterministic cases for lexical, semantic, adversarial, path, scope,
   privacy, and lifecycle behavior.
2. Record case provenance and rationale in the checked corpus.
3. Run every candidate under the same corpus, eligibility boundary, top-k,
   context budget, runner, and hardware procedure.
4. Keep completed, rejected, and failed candidate attempts in the development
   record. The lexical baseline is always included.

## Local locked bundle — issue #75

The locked-bundle contract uses an ignored local path such as
`.research/recall-v3/locked/`; it must never contain secrets, private personal
data, raw chat, or repository-confidential material. Before any reactivated
evaluation, run the CLI's locked-commitment preflight to write the corpus,
judgments, metric, runner, and candidate-manifest identities. Verify that
commitment immediately before the locked evaluation.

The commitment prevents accidental input or configuration drift. An ignored
directory is not access control: a solo maintainer remains able to read its own
local files.

## Candidate freeze — issue #77

Freeze explicit candidate manifests after the development run. Every manifest
binds its offline model metadata, deterministic document template, exact vector
artifact digest, profile/generation, and retrieval parameters. Any artifact or
configuration change produces a different candidate identity and requires a
new run.

## Deferred locked decision — issues #56 and #80

If semantic recall is reactivated and the freshly scoped automated development,
operational, competitor-harness, and locked-commitment checks pass, run the
locked suite once per frozen candidate. Record `no_go`, `conditional_go`, or
`full_go` with the resulting digests and known limitations. Lexical-only mode
remains available; a full go does not imply default promotion.
