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

build:
	cargo build --workspace

test:
	cargo test --workspace

eval: eval-recall eval-capture eval-v0.5-foundation

.PHONY: eval-recall-v3
eval-recall-v3:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 --corpus evals/recall/v3/corpus.yaml

.PHONY: eval-recall-v3-candidate eval-recall-v3-candidate-matrix
eval-recall-v3-candidate:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 --corpus evals/recall/v3/corpus.yaml --candidate evals/recall/v3/candidates/exact-union.json --require-ready-candidates

eval-recall-v3-candidate-matrix:
	cargo run --locked -q -p memzoi-cli -- eval recall-v3 --corpus evals/recall/v3/corpus.yaml --candidate evals/recall/v3/candidates/semantic-only.json --candidate evals/recall/v3/candidates/lexical-rerank.json --candidate evals/recall/v3/candidates/lexical-union.json --require-ready-candidates

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
