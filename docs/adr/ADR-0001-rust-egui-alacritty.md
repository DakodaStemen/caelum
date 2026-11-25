# ADR-0001: Rust with egui/eframe and the Alacritty terminal core

| Field | Value |
| :---- | :---- |
| Status | Accepted |
| Date | 2026-11-03 |
| Deciders | @DakodaStemen |

## Context

Caelum is a workspace terminal for Linux, macOS and Windows. The code shows the choices already made: Rust (edition 2024, `rust-version` 1.88 in `Cargo.toml`), egui/eframe for the interface, `alacritty_terminal` (pinned to an exact version) for terminal semantics, and `portable-pty` for shell sessions.

## Decision

We will build Caelum as a single Rust binary. The UI uses egui/eframe (glow backend, X11 and Wayland on Linux). Terminal emulation stays in `alacritty_terminal`, PTY ownership in `src/terminal.rs`, document saving and recovery in `src/files.rs`, and workspace serialization in `src/model.rs` (see CONTRIBUTING.md).

## Alternatives considered

| Alternative | Why not chosen |
| :---- | :---- |
| A web or Electron UI | Not used; the project is a native single binary with no browser engine. |
| Writing a terminal emulator from scratch | Not used; `alacritty_terminal` provides the grid and escape handling, pinned with `=0.26.0`. |

## Consequences

**Positive**

- One codebase and binary for Linux, macOS and Windows; CI builds and tests on all three.
- Memory-safe Rust; `cargo clippy -D warnings` and `cargo fmt --check` are enforced in CI.

**Negative**

- `alacritty_terminal` is pinned exactly, so upgrades are manual.
- egui is immediate mode; accessibility of the terminal canvas is not established (see README).
- Native-window behavior cannot be fully covered by headless tests; desktop tests need X11 or Xvfb.

**Follow-ups**

- None recorded.
