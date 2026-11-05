# 0002 Composer model

- Status: Accepted
- Date: 2026-11-05

## Context

Users need to declare which skills a project uses and get the same result on every machine.
Many developers already know Composer (PHP), whose workflow is `composer.json`, `composer.lock`, `require`, `update`, `remove` and `global`.

## Decision drivers

- Reproducible installs.
- A workflow users already understand.
- Review-friendly files that fit in version control.

## Considered options

1. Composer model: `aip.json` (intent) and `aip.lock` (result), Composer constraint notation.
2. Single file with exact versions only.
3. Cargo-style notation (`semver::VersionReq`) with the same two files.

## Decision

Use option 1.

- `aip.json` holds `registries`, `providers` and `require` (package name to constraint). Unknown keys are preserved.
- `aip.lock` holds name, version, description, source (`git`, URL, commit SHA) and path for each package, sorted by name.
- Both are written as 4-space-indented JSON with a trailing newline, like Composer.
- Commands follow Composer: `install` (from lock, or add packages), `update`, `remove`, and `-g/--global`.
- Constraint syntax is Composer's: `^`, `~`, wildcards, comparison operators, `||`, hyphen ranges, `dev-<branch>`.
- `~` uses Composer semantics: `~1.2` is `>=1.2.0 <2.0.0` and `~1.2.3` is `>=1.2.3 <1.3.0`. Cargo reads `~1.2` as `<1.3.0`, so `semver::VersionReq` is not used. `constraint.rs` implements the notation and uses `semver::Version` only for parsing and ordering.
- Pre-releases match only constraints that name a pre-release.
- Default constraint on `install <pkg>`: `^major.minor` of the latest stable version (`^0.0.patch` for 0.0.x), or `dev-<default branch>` without tags.

## Consequences

- Good: reproducible installs and small, readable diffs.
- Good: users of Composer have nothing new to learn.
- Bad: aip maintains its own constraint parser and tests.
- Bad: surprises for Cargo or npm users on `~`. The user guide states the rule.
- Bad: no solver is needed because skills have no dependencies. If skill dependencies are added, the resolver must change.
- Difference from Composer: an unsatisfied locked version is silently re-resolved on `install`.
