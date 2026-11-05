# 0006 Docker Compose and Makefile development environment

- Status: Accepted
- Date: 2026-11-05

## Context

Contributors should get the same toolchain without installing Rust, and files created while building should not be owned by root.
CI must still prove that aip works on Windows and macOS.

## Decision drivers

- Same toolchain for everyone.
- Few host requirements.
- Fast rebuilds on WSL and macOS bind mounts.

## Considered options

1. Local Rust toolchain only.
2. Dev container image driven by Docker Compose and a Makefile.
3. Nix or devcontainer spec.

## Decision

Use option 2.

- `Dockerfile` builds from `rust:1.97-slim`, adds rustfmt, clippy and git, and creates a user with the host UID and GID.
- `compose.yaml` defines one service `dev`. The repository is bind-mounted at `/workspace`. `cargo-registry` and `cargo-target` are named volumes, and `CARGO_TARGET_DIR=/target` keeps build output off the bind mount.
- `Makefile` wraps `docker compose run --rm dev ...` with targets `image`, `shell`, `build`, `release`, `run`, `fmt`, `fmt-check`, `lint`, `test`, `check`, `clean`, `nuke`, and a `help` default.
- CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests natively on Ubuntu, macOS and Windows with the stable toolchain.

## Consequences

- Good: `make check` is the same on every Linux or macOS machine with Docker.
- Good: no root-owned files in the repository.
- Bad: the container is Linux only. Windows and macOS behaviour is covered only by CI.
- Bad: Rust version appears in three places (`Cargo.toml` 1.89, Dockerfile 1.97, CI `stable`).
- Bad: `make` is not available on plain Windows; Windows users run `cargo` directly.
