.PHONY: help verify format fmt lint test build install dev clean

help:
	@echo "verify   - run the full CI-equivalent gate (format+lint+test+build)"
	@echo "format   - check Rust formatting (no changes written)"
	@echo "fmt      - apply Rust formatting"
	@echo "lint     - cargo clippy -D warnings"
	@echo "test     - cargo test (all targets/features)"
	@echo "build    - pnpm build (tsc + vite build)"
	@echo "install  - pnpm install"
	@echo "dev      - pnpm tauri dev (launch the desktop app)"
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

clean:
	cd src-tauri && cargo clean
	rm -rf dist
