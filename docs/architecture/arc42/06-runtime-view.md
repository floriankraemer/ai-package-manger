# 06 Runtime view

Purpose: the main flows, step by step.
Participants: `main` (CLI), `ops`, `Resolver` (inside `ops`), `Registries`, `Repo` (`git.rs`), `git` (the child process), `FS` (files on disk).

## Install a named package

`aip install anthropic/pdf`

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant R as Resolver and Registries
    participant G as Repo (git.rs)
    participant F as FS
    U->>M: aip install anthropic/pdf
    M->>O: install(project, specs, options)
    O->>F: read aip.json and aip.lock
    O->>O: targets (detect providers if none set)
    O->>R: default_constraint(anthropic/pdf)
    R->>F: read registry.json
    R->>G: Repo.sync(url)
    G->>F: git clone --mirror, or git fetch --prune
    R->>G: tags(), default_branch()
    G-->>R: newest stable version, e.g. 1.2.0
    R-->>O: constraint ^1.2
    O->>R: resolve(name, ^1.2)
    R->>G: resolve tag to commit, read SKILL.md
    R-->>O: LockedPackage (version, sha, path, description)
    O->>O: refresh remaining requirements, verify sources
    O->>F: refuse target folders behind a symbolic link
    O->>F: plan: read .aip-package markers
    O->>G: verify sha matches version, export(sha, skills/pdf, .pdf.aip-tmp)
    G->>F: ls-tree and cat-file per file
    O->>F: write .aip-package into the staging folder
    O->>F: write aip.json, then aip.lock
    O->>F: move pdf to .pdf.aip-old, rename staging to pdf, delete .pdf.aip-old
    O->>F: delete marked folders of removed packages and of dropped providers
    O-->>M: Report (changes, targets, pruned)
    M-->>U: Package operations ... Skill folders ...
```

If a constraint is given (`anthropic/pdf:^1.2`), the `default_constraint` step is skipped and the constraint is parsed instead.
`plan` fails before anything is written if a target folder exists and its `.aip-package` marker does not name the package (see 08, "Ownership of skill folders").
With `--dry-run`, `finish` returns the report after `plan` and writes nothing.
If any export fails, the staging folders are deleted and aip.json, aip.lock and the skill folders stay as they were.

## Install from the lock

`aip install` without arguments

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant R as Resolver and Registries
    participant G as Repo (git.rs)
    participant F as FS
    U->>M: aip install
    M->>O: install(project, [], options)
    O->>F: read aip.json (error if missing) and aip.lock
    Note over O,F: no aip.lock: message on stderr, every requirement is resolved as in update
    loop each requirement
        O->>O: lock entry satisfies constraint?
        opt not locked or no longer satisfied
            O->>R: resolve(name, constraint)
            R->>G: sync, tags, read SKILL.md
        end
    end
    O->>R: verify_sources: locked URLs against the registries
    O->>O: plan against disk
    Note over O,F: A target is skipped if its marker names the package and the locked commit
    loop each package to write
        O->>G: Repo.open(url): cached mirror, no network
        opt commit missing or not the locked version
            O->>G: Repo.sync(url), verify again
        end
        O->>F: export into staging folder, with marker
    end
    O->>F: write aip.json and aip.lock if changed
    O->>F: swap staging folders in, delete removed and pruned folders
    O-->>M: Report
```

The registries are always read, because `verify_sources` checks every locked URL against them.
A local registry file needs no network; a URL registry falls back to its cached copy (see "Fetch a URL registry").
Git repositories are not fetched when the cached mirror already has the locked commit and that commit matches its version label.
When every folder is up to date, no git process runs.

## Update

`aip update [packages]`

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant R as Resolver and Registries
    participant G as Repo (git.rs)
    participant F as FS
    U->>M: aip update anthropic/pdf
    M->>O: update(project, names, options)
    O->>F: read aip.json (error if missing) and aip.lock
    O->>O: names must be in require, forced set = names or all
    loop each forced requirement
        O->>R: resolve(name, constraint)
        R->>G: sync, list tags, pick newest match
        R-->>O: new LockedPackage
    end
    O->>O: plan: Update if the source changed, Install if a folder is missing or stale
    O->>G: export changed packages into staging folders
    O->>F: write aip.lock, aip.json
    O->>F: swap staging folders in
    O-->>M: Report (Upgrading, Downgrading or Updating)
```

Packages not in the forced set are kept at their locked version as long as they still satisfy their constraint.

## Check

`aip check`

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant R as Resolver and Registries
    participant G as Repo (git.rs)
    participant F as FS
    U->>M: aip check
    M->>O: check(project)
    O->>F: read aip.json (error if missing) and aip.lock
    O->>O: skill folders from a copy of the manifest (nothing is written)
    loop each requirement, sorted by name
        O->>R: resolve(name, constraint) and resolve(name, *)
        R->>G: sync, list tags, pick newest match
        R-->>O: allowed and latest version
        O->>F: does every skill folder exist with a marker for the locked commit
        O->>O: status, first match wins
    end
    O-->>M: Vec of SkillStatus
    M-->>U: colored table with hints, or JSON
```

A package that cannot be resolved gets an `error` status and the loop continues.
Check takes no project lock and writes nothing except the git cache, which `Repo.sync` locks.
`main` exits with 1 when a row has an `error` status, and with `--strict` when any row is not `up to date`.

## Remove

`aip remove anthropic/pdf`

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant F as FS
    U->>M: aip remove anthropic/pdf
    M->>O: remove(project, names, options)
    O->>F: read aip.json and aip.lock
    O->>O: drop names from require, error if not required
    O->>O: refresh: lock entries not in require are dropped
    O->>O: plan: Remove change for each dropped entry
    O->>F: write aip.json, aip.lock
    O->>F: delete pdf in every provider's skill folder whose marker names the package
    O-->>M: Report
    M-->>U: Package operations: 0 installs, 0 updates, 1 removal
```

`remove` makes no network access, unless the remaining requirements need resolving.
It never forces: `plan` is called with `force = false`.
A folder is deleted only if its `.aip-package` marker names the removed package, so a tampered aip.lock cannot delete a user's own folder.
Unmarked folders in a target get `warning: not removing <path>: it was not installed by aip`.

## Search

`aip search pdf`

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant R as Registries
    participant G as Repo (git.rs)
    U->>M: aip search pdf
    M->>O: search(project, query)
    O->>R: entries() (merged, first registry wins)
    loop each repository entry
        O->>G: Repo.sync(url)
        O->>G: default branch head, subdirs of skills/
        loop each valid skill name
            O->>G: latest(skill): newest stable tag that contains the skill, else branch head
            O->>G: read skills/skill/SKILL.md at that commit
        end
        alt repository fails
            O-->>U: warning: skipping repository (stderr)
        end
    end
    O->>O: keep hits whose name or description contains the query
    O-->>M: Vec of SearchHit
    M-->>U: aligned table, or JSON
```

Search uses the registries of the project, or of the global `aip.json` with `--global`.
It does not write anything except the git cache.

`aip info <repository>/<skill>` runs a shorter flow: `Registries::get` finds only the named repository, only that repository is synced, and the skill is read at its `latest` version.

## Fetch a URL registry

A `json` registry with a `url` is read when `Registries::entries` first needs it.
The flow is the same for every command that resolves a name.

```mermaid
sequenceDiagram
    participant J as JsonRegistry
    participant C as Cache
    participant A as Auth
    participant H as http
    participant S as Server
    J->>C: load entry for the URL
    J->>A: credential_for(url)
    J->>H: get(url, validators, credential)
    H->>S: GET with If-None-Match and Authorization
    alt 200
        S-->>H: body, ETag, Last-Modified
        J->>J: parse and validate
        J->>C: store atomically
    else 304
        S-->>H: not modified
        J->>C: refresh fetched_at, use cached body
    else unreachable or 5xx
        J-->>J: warn on stderr, use cached body
    else 4xx
        J-->>J: error, no fallback
    end
```

Without a cache entry, the unreachable and 5xx cases are errors too.
