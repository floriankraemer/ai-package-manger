# 11 Risks and technical debt

Purpose: known limits and shortcuts.
Items 1 to 3, 25, 26 and 28 are marked in the code with `ponytail:` comments.

## Marked in code (`ponytail:`)

| # | Location | Limit | Upgrade path |
|---|---|---|---|
| 1 | `git.rs` `Repo::export` | One `git cat-file` process per file. Slow for skills with many files. | Use `git cat-file --batch`. |
| 2 | `ops.rs` `Resolver::repo` | Progress (`Fetching <url>`) is printed to stderr from library code. A GUI or other front end cannot capture it. | Pass a progress callback or reporter into `Resolver`. |
| 3 | `registry/mod.rs` `description` | Line-based frontmatter reader. Only single-line `description:` values at column 0. Multi-line YAML gives a wrong or missing description. | Use a YAML parser. |

## Other known limits

| # | Limit | Effect |
|---|---|---|
| 4 | Resolved: ownership is recorded per folder in `.aip-package`. Folders of providers no longer in `aip.json` are pruned, and `remove` deletes from every provider's folder. A provider added over a user's own folder is refused without `--force`. | None. |
| 5 | `search` fetches every repository of every registry on every call, then reads each skill. | Slow with many or large repositories; needs network. A server registry could answer this directly. |
| 6 | Skill folder paths for several providers (Copilot `.github/skills` and `~/.copilot/skills`, Cursor, Gemini CLI, OpenCode, `.agents/skills`) come from community docs and are not vendor-confirmed. | Skills may land where the harness does not look. Change `PROVIDERS` in `detect.rs`. |
| 7 | Resolved: `Provider.global_env` names a config-home variable (`CLAUDE_CONFIG_DIR`, `COPILOT_HOME`, `GEMINI_CLI_HOME`, `CODEX_HOME`). Global detection and global skill folders resolve through `detect::scoped_path`. Codex skills stay in `~/.agents/skills`, as documented. | None. |
| 8 | Resolved: `verify_sources` compares every locked `source.url` with the registries before planning. `source.reference` is validated as a 40- or 64-character hex commit SHA. | None. |
| 9 | Resolved: `Repo::export` rejects tree paths with empty, `.`, `..`, `\` or `:` components before writing. | None. |
| 10 | Partly resolved: all exports are staged before aip.json and aip.lock are written, so a failed export changes nothing. The swaps after that are not atomic as a group. | A crash during the swaps leaves some folders at the old version, or with `.<skill>.aip-old` beside them. Their markers show it: `aip check` reports `folder out of date`, and `aip install` repairs it. |
| 11 | A leftover `.<skill>.aip-tmp` or `.<skill>.aip-old` folder after a crash stays until the next write of that skill. | Clutter in the skill folder; some harnesses may list dot-folders. |
| 12 | Resolved: cache keys append an FNV-1a hash of the exact URL to the readable part, so `a/b` and `a_b` no longer share a mirror. | None. |
| 13 | Resolved: a project lock and a per-repository cache lock serialize concurrent runs (see 08). | None. |
| 14 | Partly resolved: installing from the lock does not fetch when the cached mirror has the locked commits. Resolving (`update`, a named `install`, `check`, `search`) still fetches. The registries are always read to verify locked URLs. | A URL registry needs its cached copy when offline. |
| 15 | Resolved: `install` fails with `aip.lock is out of date with aip.json` instead of re-resolving; `aip update <name>` resolves. | None. |
| 16 | A tag that points to a different commit after a force-push changes what `update` would pick, and a locked SHA can vanish from a remote. | Install from lock fails until the commit is available. |
| 17 | Skill names are global per target: `a/pdf` and `b/pdf` cannot coexist. | Rejected with an error. |
| 18 | Providers that detect but have no skills support (windsurf, cline, roo, kiro, continue, junie, amazon-q, aider) are not install targets. | Warning or error as described in the user guide. |
| 19 | The `ops.rs` tests need `git` and write to the temp directory. | Not hermetic. |
| 20 | Rust version is pinned in three places with different values (`Cargo.toml` 1.89, Dockerfile 1.97, CI `stable`). | Drift. |
| 21 | Resolved: downloaded registries are cached and revalidated; an unreachable server or 5xx falls back to the cached copy with a warning (see 08). | Repositories missing from the git cache still need the network (item 14). |
| 22 | Resolved: `auth.json` and `AIP_AUTH` give HTTP registries bearer and Basic credentials ([ADR 0009](../decisions/0009-auth-json-for-http-registries.md)). | See item 23. |
| 23 | `auth.json` holds secrets in plain text. aip warns about loose permissions on Unix only, and cannot check Windows ACLs. | Anyone who can read the file can use the credentials. Use short-lived tokens, `chmod 600`, and `.gitignore`. |
| 24 | A stale cached registry can be used while the server is down. The cache has no maximum age. | Changes on the server stay hidden until it answers again. The warning shows the age. |
| 25 | `ops::installed_before_markers` adopts a folder without a marker if its `SKILL.md` matches the old locked commit. Other files are not compared. | A user folder with an unchanged `SKILL.md` but edited other files can be overwritten once, without `--force`. Remove the rule once installs without markers are gone. |
| 26 | `ops::finish`: the swaps after the lock is written are not atomic as a group (item 10). | See item 10. |
| 27 | A provider's skill folder behind a symbolic link is an error if the provider is in `aip.json`, and silently not scanned for pruning if it is not. | Copies there are not pruned. |
| 28 | `ops::rename_skill` rewrites `name:` line by line in the first frontmatter block, like `description` (item 3). | A multi-line or indented `name:` value is not replaced cleanly. |
| 29 | Local-change detection compares file content and the set of files, not the executable bit. Folders installed before markers had `files` are never reported as modified. | A `chmod` alone is not detected. Such folders can be overwritten without `--force` until they are reinstalled once. |
| 30 | A rename or `"as"` swap between packages from different repository URLs does not take over the other package's folder. | `install` refuses with `not installed by aip`; doing it in two steps, or `--force`, works. |

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Vendors change or standardise skill folder locations | Medium | Single table in `detect.rs`; add provider-specific tests when vendors document paths. |
| Malicious skill content | Inherent | aip installs instructions that an AI agent follows. Only add repositories you trust. aip pins commits but does not verify signatures. |
| Hostile `aip.lock` in a pull request | Low | Items 8 and 9, the reference check (08, validation table) and ownership markers: a lock cannot relabel a commit, and cannot delete folders aip did not install. Review lock changes. |
| Hostile symbolic link in a repository (`.claude/skills -> ~/.local`) | Low | `checked_dir` refuses project skill folders reached through a symbolic link. |
