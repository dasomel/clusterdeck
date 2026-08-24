# ADR-0001: macOS-first Tauri Architecture

- Status: Accepted
- Date: 2026-08-24

## Context

ClusterDeck is intended to be a lightweight desktop utility for local developers and operators who repeatedly connect to VM and Kubernetes environments. The primary workflows require filesystem access, OpenSSH/SCP execution, kubeconfig processing, local configuration management, and a compact GUI.

## Decision

Use:

- Tauri 2 as the desktop application shell
- Rust as the backend/application core
- React + TypeScript as the frontend
- macOS as the initial supported platform
- OpenSSH and `kubectl` as the initial system integration layer

Rust owns security-sensitive and operating-system operations. The frontend communicates with Rust through Tauri commands and does not directly manipulate credentials or local system configuration.

## Consequences

Positive:

- Small native desktop footprint
- Strong boundary between UI and privileged/local operations
- Reuse of mature macOS/OpenSSH/Kubernetes tooling
- Natural path to menu-bar and desktop UX

Trade-offs:

- Initial platform support is intentionally limited to macOS
- App packaging, signing, and macOS permissions require platform-specific work
- Some behavior depends on locally installed OpenSSH and Kubernetes tooling

## Non-goals

This decision does not make ClusterDeck a Kubernetes management console or require implementing a complete SSH client in Rust.
