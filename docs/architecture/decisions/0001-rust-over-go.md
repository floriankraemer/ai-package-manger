# 0001 Rust over Go

- Status: Accepted
- Date: 2026-11-05

## Context

aip is a command-line tool that must run on Windows, macOS and Linux as one binary without a runtime.
A native desktop GUI is planned for later.
It must not be built on web technology (no Electron, no webview).
Go and Rust were the two candidates.

## Decision drivers

- Single static binary on three operating systems.
- A path to a native, non-web GUI that reuses the core logic.
- Correctness of parsing and file handling (constraints, JSON, paths).
- Long-term maintainability.

## Considered options

1. Go, with Fyne for a later GUI.
2. Rust, with Slint or Iced for a later GUI.

## Decision

Use Rust.
Keep all logic in a library crate and keep `main.rs` a thin CLI, so a GUI can call the same functions (`ops::install`, `ops::search`, ...) and receive plain values (`Report`, `SearchHit`).

## Consequences

- Good: a Rust GUI toolkit (Slint, Iced) runs in the same process as the core library, with no FFI or second language. The core compiles as a crate dependency.
- Good: enums and `Result` model the domain well (`Constraint`, `Change`, `RegistryConfig`) and the compiler checks exhaustive handling.
- Good: `semver`, `serde` and `clap` cover most needs.
- Bad: slower builds and a steeper learning curve for contributors than Go.
- Bad: Rust GUI toolkits are younger than Fyne's Go ecosystem; they may need more work for polish.
- No GUI exists yet, so this benefit is not proven.

## Pros and cons of the options

### Go with Fyne

- Good: fast builds, simple concurrency, mature cross-compilation.
- Good: Fyne is a complete native toolkit with one language for CLI and GUI.
- Bad: the core would be Go and the GUI Go too, but Fyne draws its own widgets rather than native controls.
- Bad: weaker type modelling for the variant-heavy domain code.

### Rust with Slint or Iced

- Good: see Consequences.
- Bad: see Consequences.
