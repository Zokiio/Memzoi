# Recall v3 code-based evaluation workflow

Recall v3 is maintained by one developer and is validated with deterministic,
reproducible code and fixtures. External user testing and independent human
review are welcome later, but are not prerequisites for this work.

## Development corpus

1. Add deterministic cases for lexical, semantic, adversarial, path, scope,
   privacy, and lifecycle behavior.
2. Record case provenance and rationale in the checked corpus.
3. Run every candidate under the same corpus, eligibility boundary, top-k,
   context budget, runner, and hardware procedure.
4. Keep completed, rejected, and failed candidate attempts in the development
   record. The lexical baseline is always included.

## Local locked bundle — issue #75

A future locked corpus lives in an ignored local path such as
`.research/recall-v3/locked/`; it must never contain secrets, private personal
data, raw chat, or repository-confidential material. Before an evaluation, run
the CLI's locked-commitment preflight to write the corpus, judgments, metric,
runner, and candidate-manifest identities. Verify that commitment immediately
before the locked evaluation.

The commitment prevents accidental input or configuration drift. An ignored
directory is not access control: a solo maintainer remains able to read its own
local files.

## Candidate freeze — issue #77

Freeze explicit candidate manifests after the development run. Every manifest
binds its offline model metadata, deterministic document template, exact vector
artifact digest, profile/generation, and retrieval parameters. Any artifact or
configuration change produces a different candidate identity and requires a
new run.

## Locked decision — issue #80

Once the automated development, operational, competitor-harness, and locked
commitment checks pass, run the locked suite once per frozen candidate. Record
`no_go`, `conditional_go`, or `full_go` with the resulting digests and known
limitations. Lexical-only mode remains available; a full go does not imply
default promotion.
