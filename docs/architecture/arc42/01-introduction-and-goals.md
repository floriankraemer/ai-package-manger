# 01 Introduction and goals

Purpose: what aip does, who uses it, and which quality goals drive the design.

## Requirements overview

aip is a command-line package manager for AI agent skills.
A skill is a folder with a `SKILL.md`.
Skills live in git repositories that are listed in registries.
aip copies chosen skills into the skill folders of the AI harnesses on a machine (Claude Code, Codex, Cursor, Copilot, Gemini CLI, OpenCode).

Main capabilities, all in `src/`:

| Capability | Command | Module |
|---|---|---|
| Find which harnesses are configured | `detect` | `detect.rs` |
| Search skills in registries | `search` | `ops.rs`, `registry/` |
| Declare, resolve and install skills | `install` | `ops.rs`, `manifest.rs`, `constraint.rs`, `git.rs` |
| Move to newer versions | `update` | `ops.rs` |
| Remove skills | `remove` | `ops.rs` |

The model follows Composer: `aip.json` states intent, `aip.lock` states the resolved result.
See the [user guide](../../user-guide.md) for the user-facing view.

## Quality goals

| Priority | Goal | What it means here |
|---|---|---|
| 1 | Reproducibility | `aip install` with the same `aip.json` and `aip.lock` gives the same skill contents, pinned by commit SHA. |
| 2 | Safety against data loss | aip never overwrites or deletes a folder it did not install, unless `--force`. Writes go through a staging folder. A damaged or hostile lock file cannot make aip write outside the skill folders. |
| 3 | Cross-platform | Windows, macOS and Linux behave the same. No `tar`, no symlink dependence, paths built component-wise. CI runs on all three. |
| 4 | Extensibility | New registry types (an HTTP server is planned) and new providers can be added without touching resolution or installation. |

## Stakeholders

| Role | Interest |
|---|---|
| Skill consumer | Install and update skills with predictable results. |
| Skill author | Publish a repository, version it with git tags. |
| Registry maintainer | List repositories in a JSON file, later possibly serve them over HTTP. |
| Contributor | A small, readable code base with clear module boundaries and a Docker-based workflow. |
