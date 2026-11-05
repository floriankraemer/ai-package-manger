# 10 Quality requirements

Purpose: concrete scenarios behind the quality goals in [01](01-introduction-and-goals.md), and how each is checked.

| Goal | Scenario | Mechanism | Check |
|---|---|---|---|
| Reproducibility | A teammate runs `aip install` on a clean checkout with `aip.json` and `aip.lock`. They get the same files as the author, even if new tags were pushed since. | Lock stores commit SHA; locked entries that satisfy their constraint are not re-resolved. | `ops` tests: `install_resolves_constraint_and_writes_files`, `branch_constraint_pins_commit` |
| Reproducibility | `aip update` moves a skill to the newest version allowed by `aip.json`, never beyond. | Constraint matching with Composer semantics. | `constraint.rs` tests, `update_moves_to_newest_allowed_version` |
| Safety | A folder `.claude/skills/pdf` that the user wrote by hand exists. `aip install anthropic/pdf` must not touch it. | `plan` refuses unless the name is in the old lock or `--force`. | `refuses_to_overwrite_unmanaged_skill` |
| Safety | `aip.lock` contains `"name": "x/../../etc"` or a foreign `path`. aip must stop before writing. | `skill_of`, `valid_name`. | `names` test in `registry/mod.rs`; lock path check has no dedicated test |
| Safety | A network error occurs while copying a skill. The old version must survive. | Staging folder, then rename. | Not covered by an automated test |
| Cross-platform | Same behaviour on Windows, macOS and Linux. | No `tar`, component-wise paths, Unix-only mode bits. | CI matrix runs `cargo test` on all three |
| Extensibility | Add a registry type without changing `ops.rs` resolution. | Trait object `Box<dyn Registry>`. | Structural; see [08](08-crosscutting-concepts.md) and [05](05-building-block-view.md) |
| Usability | Errors tell the user what to do. | `Error` messages with hints. | `readable_errors` test |

There are no numeric performance targets.
Run time is dominated by git network access.
`search` fetches every listed repository on each call; see [11](11-risks-and-technical-debt.md).
