.PHONY: verify format lint test build

verify: format lint test build

format:
	cd src-tauri && cargo fmt --all -- --check

lint:
	cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings

test:
	cd src-tauri && cargo test --all-targets --all-features

build:
	pnpm build
