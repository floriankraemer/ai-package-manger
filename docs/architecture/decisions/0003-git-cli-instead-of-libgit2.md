# 0003 git CLI instead of libgit2

- Status: Accepted
- Date: 2026-11-05

## Context

aip must clone repositories, list tags and branches, and read file contents at a given commit.
Skill repositories are often private, and users already have working git credentials (SSH agents, credential helpers, tokens).

## Decision drivers

- Reuse of the user's authentication and proxy setup.
- No native build dependencies; simple cross-compilation.
- Efficient reads without a checkout.
- Windows support without extra tools.

## Considered options

1. Call the `git` command line.
2. libgit2 through the `git2` crate.
3. Pure-Rust `gix`.
4. Download archives (tarballs) from hosting services.

## Decision

Call `git` as a child process (`git.rs`).

- Each repository is a bare mirror (`git clone --mirror`, later `git fetch --prune`) in `$AIP_HOME/cache/repos`.
- Reads use plumbing commands against objects: `for-each-ref`, `rev-parse`, `ls-tree`, `cat-file`, `symbolic-ref`. No checkout happens.
- `export` writes blobs one by one into the destination. That avoids `git archive` and `tar`, which Windows does not ship in a form that can be relied upon.
- A clone goes to `<key>.partial` and is renamed on success.
- Missing git gives a specific message: `git not found; aip needs git installed and on PATH`.

## Consequences

- Good: credentials, SSH config, proxies and `insteadOf` rules work unchanged.
- Good: no C library or large dependency; the binary stays small.
- Good: works with local paths and any URL git accepts.
- Bad: `git` must be installed. Output formats of plumbing commands are stable, but versions differ.
- Bad: one process per file during export (`ponytail:` note in code).
- Bad: error text comes from git's stderr and is not structured.
- Bad: paths and revisions pass through a process boundary, so arguments need care (`--` separators; see risks in [arc42 section 11](../arc42/11-risks-and-technical-debt.md)).

## Pros and cons of the other options

- libgit2: in-process and typed, but credential handling must be reimplemented, it needs a C build, and SSH support is a known pain.
- gix: pure Rust, but the git CLI is the reference for transports and authentication, and reusing it needs no extra work.
- Archives: depend on a hosting service's API, no support for arbitrary git servers or private repositories with SSH.
