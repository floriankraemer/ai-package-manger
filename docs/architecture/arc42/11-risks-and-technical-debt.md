# 11 Risks and technical debt

Purpose: known limits and shortcuts.
Items 1 to 3 are marked in the code with `ponytail:` comments.

## Marked in code (`ponytail:`)

| # | Location | Limit | Upgrade path |
|---|---|---|---|
| 1 | `git.rs` `Repo::export` | One `git cat-file` process per file. Slow for skills with many files. | Use `git cat-file --batch`. |
| 2 | `ops.rs` `Resolver::repo` | Progress (`Fetching <url>`) is printed to stderr from library code. A GUI or other front end cannot capture it. | Pass a progress callback or reporter into `Resolver`. |
| 3 | `registry/mod.rs` `description` | Line-based frontmatter reader. Only single-line `description:` values at column 0. Multi-line YAML gives a wrong or missing description. | Use a YAML parser. |

## Other known limits

| # | Limit | Effect |
|---|---|---|
| 4 | No clean-up when providers change. Removing a provider from `aip.json` leaves its skill folder with the old skills, untracked. Adding a provider makes `plan` see a missing folder and re-export those skills into all targets. | Stale folders stay. `remove` only deletes from the current targets. |
| 5 | `search` fetches every repository of every registry on every call, then reads each skill. | Slow with many or large repositories; needs network. A server registry could answer this directly. |
| 6 | Skill folder paths for several providers (Copilot `.github/skills` and `~/.copilot/skills`, Cursor, Gemini CLI, OpenCode, `.agents/skills`) come from community docs and are not vendor-confirmed. | Skills may land where the harness does not look. Change `PROVIDERS` in `detect.rs`. |
| 7 | `detect --global` honours `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, but `install --global` does not. | A relocated Claude or Codex config shows as detected while skills go to `~/.claude/skills` or `~/.agents/skills`. |
| 8 | Resolved: `verify_sources` compares every locked `source.url` with the registries before planning. `source.reference` is validated as a 40- or 64-character hex commit SHA. | None. |
| 9 | Resolved: `Repo::export` rejects tree paths with empty, `.`, `..`, `\` or `:` components before writing. | None. |
| 10 | Replace is delete then rename, not an atomic swap. | A crash between the two steps leaves no folder. `aip install` repairs it. |
| 11 | A leftover `.<skill>.aip-tmp` folder after a crash stays until the next install of that skill. | Clutter in the skill folder; some harnesses may list dot-folders. |
| 12 | Resolved: cache keys append an FNV-1a hash of the exact URL to the readable part, so `a/b` and `a_b` no longer share a mirror. | None. |
| 13 | Resolved: a project lock and a per-repository cache lock serialize concurrent runs (see 08). | None. |
| 14 | Offline use is not supported: `sync` always fetches when the mirror exists. | Failing fetch fails the command. |
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

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Vendors change or standardise skill folder locations | Medium | Single table in `detect.rs`; add provider-specific tests when vendors document paths. |
| Malicious skill content | Inherent | aip installs instructions that an AI agent follows. Only add repositories you trust. aip pins commits but does not verify signatures. |
| Hostile `aip.lock` in a pull request | Low | Items 8 and 9. Review lock changes. |
