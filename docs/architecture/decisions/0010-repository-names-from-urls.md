# 0010 Repository names from URLs

- Status: Accepted; amends [0005](0005-pluggable-registries.md)
- Date: 2026-11-06

## Context

Every registry entry needed a hand-written `name`.
Two users could pick different names for the same repository, so package names in shared `aip.json` files did not carry over between registries.
The URL already says who owns the repository and how it is called.

## Decision drivers

- No duplicated information in registry entries.
- The same repository gets the same package name in every registry.
- Names stay safe as folder names.

## Considered options

1. Keep a required `name`.
2. Derive only the last URL segment (`skills`).
3. Derive `<owner>/<repo>` from the full URL path; `name` stays as optional override.

## Decision

Use option 3.

- `derive_name` takes the URL path, lowercases it and removes query, fragment and a trailing `.git`.
- All path segments are kept, so GitLab subgroups give `group/sub/repo`.
- Azure DevOps `_git` and a leading SSH `v3` segment are dropped.
- A local path yields its last component.
- Every segment must pass `valid_name`; otherwise the user must set `name`.
- Repository names are `/`-separated; `split_package` splits at the last `/`, so the skill is the last segment.
- `Registries::get` suggests names that end or start with the unknown name, to help with old explicit names.

## Consequences

- Good: registries shrink to a list of URLs.
- Good: package names are stable across registries and mirrors with the same path.
- Bad: moving a repository on the host changes its derived name; an explicit `name` pins it.
- Bad: mirrors with a different path derive a different name.
- Bad: existing `aip.json` files with old explicit names need the new package name.
