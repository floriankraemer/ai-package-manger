# 04 Solution strategy

Purpose: the main decisions that shape the solution and how they serve the quality goals.

| Goal or problem | Approach | Where |
|---|---|---|
| Reproducibility | Composer model: `aip.json` plus `aip.lock` with commit SHAs. Install from lock does not consult tags. | [ADR 0002](../decisions/0002-composer-model.md), `manifest.rs`, `ops.rs` |
| Familiar version syntax | Own constraint parser with Composer semantics; `semver::Version` only for parsing and ordering. | `constraint.rs` |
| Versions without a central server | Git tags are the versions. Repository tags version everything; `<skill>/<version>` tags override for one skill. | [ADR 0004](../decisions/0004-versioning-by-git-tags.md), `ops.rs::versions` |
| Use existing git setup | Call the `git` CLI. Keep bare mirrors in `$AIP_HOME/cache/repos` and read blobs directly from objects, so no checkout and no `tar`. | [ADR 0003](../decisions/0003-git-cli-instead-of-libgit2.md), `git.rs` |
| Registry types may grow | `Registry` trait, `RegistryConfig` tagged enum, `Registries` merging in priority order. | [ADR 0005](../decisions/0005-pluggable-registries.md), `registry/` |
| No data loss | Install only over folders the lock lists; stage then rename; lock is written before files are changed so a rerun repairs. | `ops.rs::plan`, `apply`, `finish` |
| Untrusted input | Validate names and lock paths before they become file paths. | `registry::valid_name`, `ops.rs::skill_of` |
| Future GUI | Library crate holds all logic; `main.rs` only parses arguments and prints. Results are values (`Report`, `Change`, `SearchHit`, `Detection`). | `lib.rs`, `main.rs` |
| Cross-platform | Marker paths are `/`-separated and joined per component; no shell scripts; file modes only set on Unix. | `detect::marker_path`, `git.rs` |
| Contributor setup | Docker Compose dev image driven by a Makefile; CI runs natively on three OSes. | [ADR 0006](../decisions/0006-docker-compose-dev-environment.md) |

The code favours small, direct solutions.
Known shortcuts carry a `ponytail:` comment naming their limit; see [11](11-risks-and-technical-debt.md).
