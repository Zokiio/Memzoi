# Recall v3 human governance handoff

The v0.5 evaluation code validates evidence, but it cannot manufacture the
independence required by the benchmark protocol. Candidate implementers must
not mark these gates complete alone.

## Development corpus — external prerequisite

1. Author enough cases for every ratified lexical, semantic, adversarial, path,
   scope, privacy, and lifecycle slice.
2. Record human-written, real-failure-derived, or generated provenance without
   using an evaluated candidate as its sole author.
3. Collect two blind relevance/eligibility reviews where possible. At least one
   reviewer must be independent of candidate implementation.
4. Adjudicate disagreements with rationales and publish agreement statistics.
5. Run the v3 schema and lexical baseline, review all digests, and commit the
   public corpus before selecting candidate parameters.

## Locked bundle custody — issue #75

Author locked cases independently from development queries and keep them outside
candidate-implementer access. A custodian records the sealed corpus, judgment,
metric, and runner digests. Any substantive correction creates a new version
and invalidates prior results. Never include secrets, private personal data,
raw chat, or repository-confidential source.

## Candidate freeze — issue #77

After an independently reviewed public development corpus is available and
candidate implementation is complete, run every documented profile, document
template, and architecture on development data. Retain failed and rejected
attempts. A maintainer reviews immutable manifests, selects the frozen set
without locked-test access, and records inclusion/exclusion reasons. The lexical
baseline is always included.

## Locked decision — issue #80

Only after candidate, operational, task-utility, competitor, and sealed-test
prerequisites are frozen may the custodian execute the locked run. Reruns are
limited to documented infrastructure failures. The maintainer records exactly
`no_go`, `conditional_go`, or `full_go`, cites every digest and limitation, and
states that v0.5 remains opt-in with lexical-only mode available. A full go does
not authorize default promotion.
