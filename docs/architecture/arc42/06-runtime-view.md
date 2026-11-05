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
    M->>O: install(project, specs, force)
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
    O->>O: refresh remaining requirements, plan
    O->>F: write aip.json, then aip.lock
    O->>G: export(sha, skills/pdf, .pdf.aip-tmp)
    G->>F: ls-tree and cat-file per file
    O->>F: delete old folder, rename staging to pdf
    O-->>M: Report (changes, targets)
    M-->>U: Package operations ... Skill folders ...
```

If a constraint is given (`anthropic/pdf:^1.2`), the `default_constraint` step is skipped and the constraint is parsed instead.
`plan` fails before anything is written if a target folder exists and the lock does not list the package.

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
    M->>O: install(project, [], force)
    O->>F: read aip.json (error if missing) and aip.lock
    loop each requirement
        O->>O: lock entry satisfies constraint?
        opt not locked or no longer satisfied
            O->>R: resolve(name, constraint)
            R->>G: sync, tags, read SKILL.md
        end
    end
    O->>O: plan against disk
    Note over O,F: A package is skipped if its locked source is unchanged and the folder exists in every target
    O->>F: write aip.json and aip.lock if changed
    loop each package to write
        O->>G: Repo.sync(lock source url), export(lock reference)
        O->>F: staging folder, then rename
    end
    O-->>M: Report
```

When everything is locked, the registry is never read; aip fetches the URL stored in the lock.
When nothing is missing, no git process runs.

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
    M->>O: update(project, names, force)
    O->>F: read aip.json (error if missing) and aip.lock
    O->>O: names must be in require, forced set = names or all
    loop each forced requirement
        O->>R: resolve(name, constraint)
        R->>G: sync, list tags, pick newest match
        R-->>O: new LockedPackage
    end
    O->>O: plan: Update if the source changed, Install if the folder is missing
    O->>F: write aip.lock, aip.json
    O->>G: export changed packages
    O->>F: staging, then rename
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
        O->>F: does every skill folder exist
        O->>O: status, first match wins
    end
    O-->>M: Vec of SkillStatus
    M-->>U: colored table with hints, or JSON
```

A package that cannot be resolved gets an `error` status and the loop continues.
Check takes no project lock and writes nothing except the git cache, which `Repo.sync` locks.

## Remove

`aip remove anthropic/pdf`

```mermaid
sequenceDiagram
    actor U as User
    participant M as main
    participant O as ops
    participant F as FS
    U->>M: aip remove anthropic/pdf
    M->>O: remove(project, names)
    O->>F: read aip.json and aip.lock
    O->>O: drop names from require, error if not required
    O->>O: refresh: lock entries not in require are dropped
    O->>O: plan: Remove change for each dropped entry
    O->>F: write aip.json, aip.lock
    O->>F: delete skills/pdf in every target folder
    O-->>M: Report
    M-->>U: Package operations: 0 installs, 0 updates, 1 removal
```

`remove` makes no network access, unless the remaining requirements need resolving.
It never forces: `plan` is called with `force = false`, and removals are limited to lock entries.

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
            O->>G: latest(skill): newest stable tag or branch head
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

Search is always project scope.
It does not write anything except the git cache.

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
