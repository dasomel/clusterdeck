# ClusterDeck CI

## Purpose

GitHub Actions is the baseline verification gate for changes to ClusterDeck.

The initial CI runs on macOS because ClusterDeck is currently a macOS-first Tauri application.

## Checks

Pull requests and pushes to `main` run:

1. Rust formatting (`cargo fmt --check`)
2. Rust linting (`cargo clippy -D warnings`)
3. Rust tests
4. Frontend TypeScript/Vite build
5. Tauri environment/configuration validation

## Dependency installation

The repository does not yet commit a frontend lockfile while the initial dependency set is being stabilized. CI therefore uses `pnpm install --no-frozen-lockfile` during the bootstrap stage.

Once the dependency graph is stabilized, a lockfile must be committed and CI changed to `pnpm install --frozen-lockfile`.

## Release builds

Release packaging, code signing, notarization, and DMG publication are intentionally separate from PR verification. They will be introduced after the local application build is stable and the macOS signing/notarization policy is defined.

## Security

CI must not require production credentials, SSH private keys, kubeconfigs, or real infrastructure access. Tests must use placeholders, mocks, or local fixtures.
