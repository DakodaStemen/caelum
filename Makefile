# One entry point for developers. `make ci` is lint + tests + build, matching the Linux steps in
# .github/workflows/ci.yml.
.DEFAULT_GOAL := help
.PHONY: help setup lint fmt test build ci

help:
	@echo "make setup   check that cargo, rustfmt and clippy are installed"
	@echo "make lint    cargo fmt --check, cargo clippy -D warnings, markdownlint"
	@echo "make fmt     apply rustfmt"
	@echo "make test    cargo test --locked"
	@echo "make build   cargo build --release --locked"
	@echo "make ci      lint + test + build"

setup:
	@command -v cargo >/dev/null || { echo "need cargo (rustup)" >&2; exit 1; }
	@cargo fmt --version >/dev/null || { echo "need rustfmt: rustup component add rustfmt" >&2; exit 1; }
	@cargo clippy --version >/dev/null || { echo "need clippy: rustup component add clippy" >&2; exit 1; }
	@command -v npx >/dev/null || echo "note: npx not found; make lint skips markdownlint"

lint:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	@if command -v npx >/dev/null; then npx -y markdownlint-cli2 "**/*.md"; else echo "skip markdownlint: npx not found"; fi

fmt:
	cargo fmt

test:
	cargo test --locked

build:
	cargo build --release --locked

ci: lint test build
