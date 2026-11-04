.PHONY: help verify format fmt lint test build install dev release clean evidence research-check

help:
	@echo "verify   - run the full CI-equivalent gate (format+lint+test+build)"
	@echo "evidence - run verify stages and append sanitized JSONL timing/result records (docs/research-evidence.md)"
	@echo "research-check - validate research/evidence against the OpenForge v1 schema"
	@echo "format   - check Rust formatting (no changes written)"
	@echo "fmt      - apply Rust formatting"
	@echo "lint     - cargo clippy -D warnings"
	@echo "test     - cargo test (all targets/features)"
	@echo "build    - pnpm build (tsc + vite build; frontend assets only, no app binary)"
	@echo "install  - pnpm install"
	@echo "dev      - pnpm tauri dev (launch the desktop app)"
	@echo "release  - pnpm tauri build (produce the .app/.dmg bundle)"
	@echo "clean    - remove Rust and frontend build artifacts"

verify: format lint test build

format:
	cd src-tauri && cargo fmt --all -- --check

fmt:
	cd src-tauri && cargo fmt --all

lint:
	cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings

test:
	cd src-tauri && cargo test --all-targets --all-features

build:
	pnpm build

install:
	pnpm install

dev:
	pnpm tauri dev

release:
	pnpm tauri build

clean:
	cd src-tauri && cargo clean
	rm -rf dist

evidence:
	./scripts/collect-evidence.sh

research-check:
	python3 scripts/research/check-research-evidence.py
