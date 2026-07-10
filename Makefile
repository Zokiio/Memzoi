.PHONY: help build test eval eval-update-baseline lint bench install smoke onboarding-smoke

help:
	@printf '%s\n' 'Targets:'
	@printf '%s\n' '  build             Build all workspace crates'
	@printf '%s\n' '  test              Run workspace tests'
	@printf '%s\n' '  eval              Run the checked-in trust evaluation gate'
	@printf '%s\n' '  eval-update-baseline  Explicitly update the trust evaluation baseline'
	@printf '%s\n' '  lint              Run clippy with warnings denied'
	@printf '%s\n' '  bench             Run core benchmarks'
	@printf '%s\n' '  install           Install memzoi and memzoi-mcp locally'
	@printf '%s\n' '  smoke             Run developer smoke checks'
	@printf '%s\n' '  onboarding-smoke  Run first-run onboarding smoke checks'

build:
	cargo build --workspace

test:
	cargo test --workspace

eval:
	cargo run --locked -q -p memzoi-cli -- eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json

eval-update-baseline:
	cargo run --locked -q -p memzoi-cli -- eval recall --corpus evals/recall/v2/corpus.yaml --baseline evals/recall/v2/baseline.json --update-baseline

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
