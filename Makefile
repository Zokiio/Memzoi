.PHONY: help build test eval eval-recall eval-capture eval-update-baseline eval-update-recall-baseline eval-update-capture-baseline lint bench install smoke onboarding-smoke capture-smoke

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  build             Build all workspace crates'
	@printf '%s\n' '  test              Run workspace tests'
	@printf '%s\n' '  eval              Run all checked-in evaluation gates'
	@printf '%s\n' '  eval-recall       Run the checked-in recall evaluation gate'
	@printf '%s\n' '  eval-capture      Run the checked-in capture evaluation gate'
	@printf '%s\n' '  eval-update-baseline  Explicitly update both evaluation baselines'
	@printf '%s\n' '  eval-update-recall-baseline  Explicitly update only the recall baseline'
	@printf '%s\n' '  eval-update-capture-baseline  Explicitly update only the capture baseline'
	@printf '%s\n' '  lint              Run clippy with warnings denied'
	@printf '%s\n' '  bench             Run core benchmarks'
	@printf '%s\n' '  install           Install memzoi and memzoi-mcp locally'
	@printf '%s\n' '  smoke             Run developer smoke checks'
	@printf '%s\n' '  onboarding-smoke  Run first-run onboarding smoke checks'
	@printf '%s\n' '  capture-smoke     Run CLI and MCP capture through built binaries'
	@printf '%s\n' '  recall-v3-development-run  Build and evaluate the complete offline 18-candidate matrix'
	@printf '%s\n' '  recall-v3-development-freeze  Verify evidence and freeze development finalists'
	@printf '%s\n' '  recall-v3-development-publish  Publish verified evidence without model/vector artifacts'

build:
	cargo build --workspace

test:
	cargo test --workspace

eval: eval-recall eval-capture eval-v0.5-foundation

.PHONY: eval-recall-v3
eval-recall-v3:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 --corpus evals/recall/v3/corpus.yaml

.PHONY: eval-recall-v3-candidate eval-recall-v3-candidate-matrix recall-v3-model-install recall-v3-model-inspect recall-v3-development-run recall-v3-development-freeze recall-v3-development-publish
eval-recall-v3-candidate:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 --corpus evals/recall/v3/corpus.yaml --candidate evals/recall/v3/candidates/exact-union.json --require-ready-candidates

eval-recall-v3-candidate-matrix:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 --corpus evals/recall/v3/corpus.yaml --candidate evals/recall/v3/candidates/semantic-only.json --candidate evals/recall/v3/candidates/lexical-rerank.json --candidate evals/recall/v3/candidates/lexical-union.json --require-ready-candidates

# Explicit network step. Model files remain under the ignored research root.
recall-v3-model-install:
	@set -e; for profile in evals/recall/v3/profiles/*.json; do \
		cargo run --locked -q -p memzoi-cli -- eval recall-v3 model install --profile "$$profile" --model-root .research/recall-v3/models; \
	done

# Offline integrity check for all explicitly installed profiles.
recall-v3-model-inspect:
	@set -e; for profile in evals/recall/v3/profiles/*.json; do \
		cargo run --locked -q -p memzoi-cli -- eval recall-v3 model inspect --profile "$$profile" --model-root .research/recall-v3/models; \
	done

recall-v3-development-run:
	@test -n "$(RECALL_V3_ATTEMPTED_AT)" || (echo 'RECALL_V3_ATTEMPTED_AT=<RFC3339> is required' >&2; exit 2)
	cargo run --locked -q -p memzoi-cli --features recall-models -- eval recall-v3 development run --matrix evals/recall/v3/development-matrix.json --corpus evals/recall/v3/corpus.yaml --model-root .research/recall-v3/models --output .research/recall-v3/observed --attempted-at "$(RECALL_V3_ATTEMPTED_AT)"

recall-v3-development-freeze:
	@test -n "$(RECALL_V3_FROZEN_AT)" || (echo 'RECALL_V3_FROZEN_AT=<RFC3339> is required' >&2; exit 2)
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 development freeze --log .research/recall-v3/observed/development-log.json --output .research/recall-v3/observed/frozen-candidates.json --frozen-at "$(RECALL_V3_FROZEN_AT)"

recall-v3-development-publish:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 development publish --run .research/recall-v3/observed --output "$${RECALL_V3_PUBLISH_OUTPUT:-evals/recall/v3/observed}"

.PHONY: eval-recall-operational
eval-recall-operational:
	cargo run --locked -q -p memzoi-cli -- eval recall-operational --evidence evals/recall/v3/operational/evidence.json

.PHONY: eval-recall-competitors
eval-recall-competitors:
	cargo run --locked -q -p memzoi-cli -- eval recall-competitors --evidence evals/recall/v3/competitors/fixture-evidence.json

.PHONY: eval-v0.5-foundation
eval-v0.5-foundation: eval-recall-v3 eval-recall-v3-candidate-matrix eval-recall-operational eval-recall-competitors

eval-recall:
	cargo run --locked -q -p memzoi-cli -- eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json

eval-capture:
	cargo run --locked -q -p memzoi-cli -- eval capture --corpus evals/capture/v1/corpus.yaml --baseline evals/capture/v1/baseline.json

eval-update-baseline: eval-update-recall-baseline eval-update-capture-baseline

eval-update-recall-baseline:
	cargo run --locked -q -p memzoi-cli -- eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json --update-baseline

eval-update-capture-baseline:
	cargo run --locked -q -p memzoi-cli -- eval capture --corpus evals/capture/v1/corpus.yaml --baseline evals/capture/v1/baseline.json --update-baseline

lint:
	cargo clippy --workspace --all-targets -- -D warnings

bench:
	cargo bench -p memzoi-core --bench memory_bench

install:
	./scripts/install.sh

smoke:
	./scripts/dev-smoke.sh

onboarding-smoke:
	./scripts/onboarding-smoke.sh

capture-smoke:
	cargo build --locked -p memzoi-cli -p memzoi-mcp
	./scripts/capture-smoke.sh
