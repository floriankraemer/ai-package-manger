# 02 Constraints

Purpose: fixed boundaries that the design has to respect.

## Technical

| Constraint | Detail | Source |
|---|---|---|
| Rust | Edition 2024, minimum Rust 1.89. Runtime dependencies: `clap`, `semver`, `serde`, `serde_json`. | `Cargo.toml`, [ADR 0001](../decisions/0001-rust-over-go.md) |
| git CLI required | aip shells out to `git`. Without `git` on `PATH` every network or repository operation fails with a clear message. | [ADR 0003](../decisions/0003-git-cli-instead-of-libgit2.md) |
| Single binary, no server | The tool runs locally and keeps all state in files (`aip.json`, `aip.lock`, `$AIP_HOME`). | |
| Three operating systems | Windows, macOS, Linux must work. | `.github/workflows/ci.yml` |
| Synchronous code | No async runtime; git processes run one after another. | `ops.rs`, `git.rs` |

## Planned, not built

A native GUI is planned for later.
It must not be web technology (no Electron, no webview-based shell).
This is why the core is a library crate (`src/lib.rs`) with a thin CLI (`src/main.rs`), and why Rust was chosen over Go ([ADR 0001](../decisions/0001-rust-over-go.md)).
No GUI code exists today.

## Organisational and conventions

| Constraint | Detail |
|---|---|
| Composer compatibility of notation | Constraint syntax and `~`/`^` semantics follow Composer, not Cargo ([ADR 0002](../decisions/0002-composer-model.md)). |
| Skill layout | Skills sit at `skills/<skill>/SKILL.md` in a repository. This is fixed in code, not configurable. |
| Names | Repository and skill names: lowercase letters, digits, `.`, `_`, `-`; the first character is a letter or digit. |
| Provider folders | Taken from community documentation, not all confirmed by vendors. See [11](11-risks-and-technical-debt.md). |
