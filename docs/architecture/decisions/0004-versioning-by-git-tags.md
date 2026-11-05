# 0004 Versioning by repository tags with optional per-skill tags

- Status: Accepted
- Date: 2026-11-05

## Context

A repository can hold many skills, and authors want to release them together or separately.
There is no central package index that stores versions.

## Decision drivers

- No extra release step beyond `git tag`.
- Simple for the common case of one version per repository.
- Support for repositories whose skills evolve independently.

## Considered options

1. Repository tags only.
2. A version field inside each `SKILL.md`.
3. Repository tags plus optional per-skill tags `<skill>/<version>`.

## Decision

Use option 3.

- Tags without `/` that parse as versions (`v1.2.0`, `1.2`) version all skills. A leading `v` or `V` is stripped, and one or two part versions are padded with zeros.
- Tags `<skill>/<version>` (`pdf/v1.3.0`) version one skill.
- If a skill has at least one tag of its own, only those apply to it. Repository tags are ignored for that skill. Other skills are not affected.
- With no version tags, the skill is available as `dev-<branch>`, and the default constraint is `dev-<default branch>`.
- Pre-releases are excluded from the default constraint and from `search`; they match only constraints that name a pre-release.
- The resolved commit is the tag's commit, and the skill is read from that commit.

## Consequences

- Good: no registry-side version data; the git history is the source of truth.
- Good: a single tag releases a whole repository.
- Good: per-skill tags allow independent versions in a mono-repository.
- Bad: a skill that gets its first per-skill tag silently stops following repository tags. Authors must tag consistently. The user guide documents this.
- Bad: no way to express "repository tags up to X, then per-skill tags".
- Bad: moved or deleted tags change what resolves. The lock pins commits, which limits the impact for consumers.
