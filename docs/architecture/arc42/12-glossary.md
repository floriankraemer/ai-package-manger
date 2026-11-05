# 12 Glossary

Purpose: terms as used in the code and the docs.

| Term | Meaning |
|---|---|
| aip | The tool. Short for the project name, not an acronym used in the code. |
| Skill | A folder `skills/<skill>/` with `SKILL.md`, stored in a repository and copied to a harness skill folder. |
| Harness | An AI agent tool such as Claude Code or Codex. The code calls them providers. |
| Provider | Entry in `PROVIDERS` (`detect.rs`): id, name, detection markers, optional skill folders, optional env variable. |
| Marker | A file or folder whose existence signals a provider, e.g. `.claude/` or `CLAUDE.md`. |
| Scope | `Project` (current directory) or `Global` (home folder, manifest in `$AIP_HOME`). |
| Repository | A git repository listed in a registry under a short name. |
| Registry | An ordered source of repository entries (`name`, `url`). Trait `Registry`. |
| `RegistryConfig` | The `registries` entry in `aip.json`, a tagged enum (`type`). |
| `Registries` | The merged, prioritised set of registries used during a run. |
| Package name | `<repository>/<skill>`. |
| Manifest | `aip.json`. The `Manifest` type. |
| Lock | `aip.lock`. The `Lock` type with `LockedPackage` and `Source`. |
| Constraint | A version requirement string in Composer notation, or `dev-<branch>`. |
| Branch constraint | `dev-<branch>`; resolves to the branch head. |
| Repository tag | A tag like `v1.2.0` that versions all skills of a repository. |
| Skill tag | A tag `<skill>/<version>` that versions one skill and takes over from repository tags. |
| Mirror | The bare clone of a repository in `$AIP_HOME/cache/repos`. |
| `AIP_HOME` | aip's own folder, default `~/.aip`. |
| Target | A skill folder to write to, derived from a provider and scope. |
| Plan | The `Plan` built by `ops::plan`: changes, removals and writes. |
| Change | `Install`, `Update` or `Remove`; one line of the operation summary. |
| Managed | A skill folder whose package is in the old lock. Only managed folders are replaced without `--force`. |
| Staging folder | `.<skill>.aip-tmp` next to the target; used for the copy before the rename. |
| Unmanaged folder | An existing skill folder not listed in the lock. |
