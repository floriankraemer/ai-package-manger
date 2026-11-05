# aip user guide

aip installs AI agent skills from git repositories into the skill folders of your AI tools.
It works like Composer: you list what you need in `aip.json`, aip resolves versions and records them in `aip.lock`, and `aip install` reproduces exactly that state on any machine.

## Contents

- [Install aip](#install-aip)
- [Concepts](#concepts)
- [Quick start](#quick-start)
- [Commands](#commands)
- [Version constraints](#version-constraints)
- [Versioning for skill authors](#versioning-for-skill-authors)
- [Publishing a skill repository](#publishing-a-skill-repository)
- [Writing a registry file](#writing-a-registry-file)
- [Multiple registries and priority](#multiple-registries-and-priority)
- [Private repositories](#private-repositories)
- [Private registries](#private-registries)
- [Offline use and the registry cache](#offline-use-and-the-registry-cache)
- [Environment variables](#environment-variables)
- [Safety: unmanaged folders and --force](#safety-unmanaged-folders-and---force)
- [Troubleshooting](#troubleshooting)

## Install aip

aip needs `git` on your `PATH` at run time.
It is a single Rust binary without other runtime dependencies.

Build from source (Rust 1.89 or newer):

```sh
cargo build --release
# binary: target/release/aip (aip.exe on Windows)
```

Copy the binary to a folder on your `PATH`.

If you prefer not to install Rust, the repository ships a Docker Compose setup; see [Development](#development-without-a-local-rust-toolchain).
The container is meant for developing aip, not for managing skills on your machine.

### Development without a local Rust toolchain

```sh
make          # list targets
make build    # debug build in the container
make check    # format check, clippy, tests
make run ARGS="detect --json ."
```

## Concepts

| Term | Meaning |
|---|---|
| Skill | A folder that contains a `SKILL.md` file and optional helper files. AI harnesses load it as an instruction package. |
| Repository | A git repository that holds one or more skills under `skills/<skill>/`. |
| Registry | A list of repositories, each with a short name and a git URL. A registry is a JSON document (a local file or a download) or a single inline git repository. |
| Package name | `<repository name>/<skill>`, for example `anthropic/pdf`. The repository name comes from the registry, not from the git URL. |
| Provider | An AI harness aip knows about (Claude Code, Codex, Cursor, ...). Each provider with skills support has a skill folder. |
| Project scope | The default. `aip.json`, `aip.lock` and the skill folders live in the current directory. |
| Global scope | Selected with `-g/--global`. `aip.json` and `aip.lock` live in `$AIP_HOME`, skill folders live in your home folder. Like `composer global`. |
| `aip.json` | What you require: providers, package constraints, optional registries. You commit it. |
| `aip.lock` | What aip resolved: exact version and commit per package. aip writes it; do not edit it. You commit it. |

Providers and their skill folders:

| Provider id | Project folder | Global folder |
|---|---|---|
| `claude-code` | `.claude/skills` | `~/.claude/skills` |
| `codex` | `.agents/skills` | `~/.agents/skills` |
| `agents-md` | `.agents/skills` | `~/.agents/skills` |
| `gemini-cli` | `.gemini/skills` | `~/.gemini/skills` |
| `cursor` | `.cursor/skills` | `~/.cursor/skills` |
| `copilot` | `.github/skills` | `~/.copilot/skills` |
| `opencode` | `.opencode/skills` | `~/.config/opencode/skills` |

`codex` and `agents-md` share one folder, so aip writes it once.
`aip detect` also recognises windsurf, cline, roo, kiro, continue, junie, amazon-q and aider, but aip cannot install skills for them.
These folder locations come from community documentation and are not all confirmed by the vendors.
The table lives in `src/detect.rs`.

## Quick start

1. Create a registry at `~/.aip/registry.json`:

   ```json
   { "repositories": [ { "name": "anthropic", "url": "https://github.com/anthropics/skills.git" } ] }
   ```

2. Go to your project and look for skills:

   ```sh
   cd my-project
   aip search pdf
   ```

3. Install one:

   ```sh
   aip install anthropic/pdf
   ```

   aip detects your providers (if `aip.json` does not name any yet), writes `aip.json` and `aip.lock`, and copies the skill into each provider's skill folder.

4. Commit `aip.json` and `aip.lock`.
   A teammate runs `aip install` and gets the same skills at the same commits.

## Commands

All commands print errors as `error: <message>` on standard error and exit with a non-zero status.
Progress lines (`Fetching <url>`) and warnings also go to standard error.

### aip init

```sh
aip init
aip init --provider claude-code --provider codex
aip init --global
```

Creates `aip.json` with the `providers` that receive skills.
`--provider <id>` (`-p`, repeatable) names them; ids come from `aip detect`.
Without it, aip uses the providers it detects in the folder.
Use `--provider` when nothing is detected yet, so aip knows where to write skills.
`--global` creates the global `aip.json` in `$AIP_HOME`.
It fails if `aip.json` already exists, or if a provider is unknown or has no skill folder.

### aip detect

```sh
aip detect [PATH]
aip detect --global
aip detect --json [PATH]
```

Shows which providers are configured in a folder, based on marker files and folders.
`PATH` defaults to the current directory.
`--global` scans your home folder instead and cannot be combined with `PATH`.
`--json` prints a JSON array of `{ "id", "name", "markers" }`.

```
$ aip detect
Detected 2 providers in /home/me/project:
  Claude Code    (claude-code)   .claude/, CLAUDE.md
  OpenAI Codex   (codex)         .codex/
```

```
$ aip detect --global
Detected 1 provider in global config (/home/me):
  Claude Code   (claude-code)   .claude/
```

If nothing matches: `No AI providers detected in <location>.`

With `--global`, a provider can also be detected through an environment variable.
`CLAUDE_CONFIG_DIR` (Claude Code) and `CODEX_HOME` (Codex) count when they point to an existing directory.
The marker is then shown as `$CLAUDE_CONFIG_DIR` or `$CODEX_HOME`.
This only affects detection.
`aip install --global` always writes to `~/.claude/skills` and `~/.agents/skills`, not to the relocated directory.

### aip search

```sh
aip search <query>
aip search --json <query>
```

Looks up skills in all repositories of your registries.
The query is case-insensitive and matches the package name and the description.
aip uses each skill's latest stable version and reads its `SKILL.md` from there.
`search` always works on the current directory: it reads `aip.json` there, if present, to find the registries.
It has no `--global` flag.

```
$ aip search pdf
anthropic/pdf    1.2.0      Work with PDF files
anthropic/xlsx   dev-main   Create spreadsheets and read PDF exports
```

If nothing matches: `No skills found matching '<query>'.`
`--json` prints an array of `{ "name", "version", "description" }`; `description` is left out when the skill has none.

The table shows only the first sentence of each description, cut to 90 characters.
Use `aip info` for the full text.

Search fetches every repository in your registries, so it can be slow with many entries.
A repository that cannot be reached is skipped with `warning: skipping repository '<name>': <reason>` on standard error.

### aip info

```sh
aip info <repository>/<skill>
aip info --json <repository>/<skill>
```

Shows the full description of one skill from the registry, wrapped to 80 columns.
It looks the skill up like `aip search`, so it is as slow and reads `aip.json` in the current directory the same way.
If no skill has that exact name: `error: skill '<name>' not found in the registry`.
`--json` prints the same object as one `search` entry.

### aip check

```sh
aip check [-g] [--json]
```

Shows every skill in `aip.json` with its locked version, its constraint, the newest stable version, and a status.
It only reads: it never changes `aip.json`, `aip.lock` or skill folders.
It fetches the skill repositories into the cache to see new tags and commits.

```
$ aip check
┌────────────┬───────────┬────────────┬────────┬─────────────────────────┐
│ Package    │ Installed │ Constraint │ Latest │ Status                  │
├────────────┼───────────┼────────────┼────────┼─────────────────────────┤
│ demo/hello │ 1.0.0     │ ^1.0       │ 2.0.0  │ update available: 1.1.0 │
└────────────┴───────────┴────────────┴────────┴─────────────────────────┘
Run `aip update` to apply 1 available update.
```

The first status that applies is shown:

| Status | Meaning |
|---|---|
| `not installed` | The skill is not in `aip.lock`. Run `aip install`. |
| `source changed` | The locked URL differs from the registry URL. Run `aip update <name>`. |
| `folder missing` | A skill folder of a provider lacks the skill. Run `aip install`. |
| `new commits: 1a2b3c4` | A `dev-<branch>` skill has a newer commit. Run `aip update`. |
| `update available: 1.1.0` | A newer version fits the constraint. Run `aip update`. |
| `update available: 1.1.0 (2.0.0 outside constraint)` | Same, and an even newer version needs a changed constraint. |
| `2.0.0 outside ^1.0` | You have the newest allowed version, but a newer one exists. Change the constraint in `aip.json`, then run `aip update`. |
| `up to date` | Nothing to do. |
| `error (see below)` | aip could not resolve this skill, for example because its repository is unreachable. The details are printed below the table as `error: <package>: <message>`, and the other rows are still shown. |

On a terminal the status is colored (green, yellow, red); `NO_COLOR` turns that off.
Below the table, aip prints error details and every next step that applies, such as `aip install`, `aip update`, or widening a constraint.
If everything is current it prints `All skills are up to date.`
`--json` prints an array with `name`, `constraint`, `installed`, `latest` and `status` (an object with a `state` field).

### aip install

```sh
aip install [<repository>/<skill>[:<constraint>]...] [-g] [--force]
```

With packages, aip adds them to `aip.json` (creating the file if needed), resolves them and installs them.
Without a constraint aip writes a default: `^<major>.<minor>` of the latest stable version, or `dev-<default branch>` when the skill has no version tags.
For a `0.0.x` version the default is `^0.0.<patch>`.
A package you name is always resolved again, even if it is already locked.

Without packages, aip installs what `aip.json` and `aip.lock` say.
Locked packages that still satisfy their constraint are installed at the locked commit.
Like Composer, aip does not silently change versions here.
If a requirement is not in `aip.lock`, or its locked version no longer satisfies the constraint in `aip.json`, `aip install` stops with `aip.lock is out of date with aip.json`, lists the packages, and writes nothing.
Run `aip update <package>...` to resolve them, or name the package in `aip install <package>[:<constraint>]`.
Packages that are no longer required are removed from the lock and from disk.
aip also checks that the URL locked for every package matches the one the registries list for its repository.
Runs are serialized per project: a second aip process in the same project waits until the first one has finished.

If `aip.json` has no `providers`, aip fills them from `aip detect` (only providers that support skills).

```
$ aip install anthropic/pdf
Package operations: 1 install, 0 updates, 0 removals
  - Installing anthropic/pdf (1.2.0)
Skill folders: .claude/skills, .agents/skills
```

While it works, aip prints `Fetching <git url>` to standard error, once per repository.

```
$ aip install anthropic/pdf:^1.2 anthropic/xlsx:dev-main
```

When nothing needs to change: `Nothing to install, update or remove`.
With `--global`, folders are shown as `~/.claude/skills`.
`--force` replaces folders that aip did not install; see [Safety](#safety-unmanaged-folders-and---force).

### aip update

```sh
aip update [<repository>/<skill>...] [-g] [--force]
```

Re-resolves the named packages, or all required packages if you name none, to the newest version their constraint allows.
It updates `aip.lock` and the skill folders; `aip.json` keeps its constraints.
A package must already be in `aip.json`.

```
$ aip update anthropic/pdf
Package operations: 0 installs, 1 update, 0 removals
  - Upgrading anthropic/pdf (1.2.0 => 1.3.0)
Skill folders: .claude/skills, .agents/skills
```

The verb is `Upgrading`, `Downgrading` or `Updating` (for branch constraints, where versions cannot be compared).
For `dev-<branch>` constraints, `update` moves the lock to the current head of the branch.
The change shows the short commits, for example `Updating anthropic/xlsx (dev-main 1a2b3c4 => dev-main 5d6e7f8)`.

### aip remove

```sh
aip remove <repository>/<skill>... [-g]
```

Removes packages from `aip.json` and `aip.lock` and deletes their skill folders from every provider folder.
aip deletes only folders for packages that the lock lists.

```
$ aip remove anthropic/pdf
Package operations: 0 installs, 0 updates, 1 removal
  - Removing anthropic/pdf (1.2.0)
Skill folders: .claude/skills, .agents/skills
```

`remove` has no `--force`.

The summary line pluralises by count: "1 install", "0 installs", "1 removal".

## aip.json

```json
{
    "registries": [
        { "type": "json", "path": "registry.json" },
        { "type": "json", "path": "/srv/shared/registry.json" },
        { "type": "git", "name": "acme", "url": "git@github.com:acme/private-skills.git" }
    ],
    "providers": ["claude-code", "codex"],
    "require": {
        "anthropic/pdf": "^1.2",
        "anthropic/xlsx": "dev-main"
    }
}
```

- `registries` is optional.
  Relative paths resolve against the folder of `aip.json`.
  If you set it, aip uses only those registries and ignores `$AIP_HOME/registry.json`.
  Types: `json` (`path` of a registry file, or `url` of a registry to download) and `git` (`name` and `url` of one repository, see [Private repositories](#private-repositories)).
- `providers` lists provider ids.
  Unknown ids are an error.
  Providers without skills support are skipped with a warning.
- `require` maps package names to constraints.
  aip keeps it sorted.
- Other keys are kept unchanged when aip rewrites the file.
- aip rewrites the file with four-space indentation.

`aip.lock` contains for each package: `name`, `version`, `description` (if any), `source` (`type: git`, `url`, `reference` = commit SHA) and `path`.
aip rewrites it only when the content changes.

## Version constraints

Constraints use Composer notation.

| Constraint | Meaning |
|---|---|
| `1.2.3`, `=1.2.3`, `==1.2.3` | exactly that version |
| `^1.2.3` | `>=1.2.3 <2.0.0` |
| `^1.2` | `>=1.2.0 <2.0.0` |
| `^0.3` | `>=0.3.0 <0.4.0` |
| `^0.0.3` | `>=0.0.3 <0.0.4` |
| `~1.2` | `>=1.2.0 <2.0.0` |
| `~1.2.3` | `>=1.2.3 <1.3.0` |
| `~1` | `>=1.0.0 <2.0.0` |
| `1.2.*`, `1.2.x` | `>=1.2.0 <1.3.0` |
| `1.*` | `>=1.0.0 <2.0.0` |
| `*` | any stable version |
| `>=1.0 <2.0`, `>=1.0,<2.0` | all parts must hold (space or comma) |
| `>`, `>=`, `<`, `<=`, `!=`, `<>` | comparison operators |
| `^1.0 \|\| ^3.0` (also a single `\|`) | either alternative |
| `1.0 - 2.0` | `>=1.0.0 <2.1.0` (a partial upper bound includes everything it prefixes) |
| `1.0.0 - 2.1.0` | `>=1.0.0 <=2.1.0` |
| `dev-main` | latest commit of branch `main` |

Tilde follows Composer, not Cargo.
`~1.2` allows `1.9.0`; in Cargo it would stop at `1.3.0`.
Use `~1.2.3` to stay within one minor version.

Pre-release rule: a version such as `2.0.0-beta` only matches a constraint that itself names a pre-release (for example `>=2.0.0-beta`).
`^1.0` never installs `1.1.0-beta`.
The default constraint and `search` ignore pre-releases.

Branch constraints (`dev-<branch>`) match no tag.
They resolve to the head commit of the branch and lock to that commit.
Branch names with spaces, control characters, `~ ^ : ? * [ \`, a leading `-` or `..` are rejected.

Tags are read as versions after removing a leading `v` or `V` and any `+build` part.
`1`, `1.2` and `1.2.3` are accepted and padded with zeros.
Tags that are not versions (`release-1`, `1.2.3.4`) are ignored.

## Versioning for skill authors

aip versions skills by git tags.

- Repository tags like `v1.2.0` or `1.2.0` version all skills of the repository together.
- A single skill can have its own tags in the form `<skill>/<version>`, for example `pdf/v1.3.0` or `pdf/2.0`.
- If a skill has at least one per-skill tag, only the per-skill tags apply to that skill, and repository tags are ignored for it.
  Other skills in the same repository keep using the repository tags.
- Tags that contain a `/` never count as repository tags.
- Without any version tag, the skill is only available as `dev-<branch>`.

Example: tags `v1.0.0`, `v1.1.0`, `pdf/v2.0.0` mean `pdf` is at `2.0.0`, and every other skill is at `1.1.0`.

aip reads the skill's files from the tagged commit, so tag after you commit the skill.

## Publishing a skill repository

Layout:

```
my-skills/
  skills/
    pdf/
      SKILL.md
      scripts/extract.py
    xlsx/
      SKILL.md
```

- Every skill is a direct child of `skills/`.
- The folder name is the skill name.
  It must start with a lowercase letter or digit and contain only lowercase letters, digits, `.`, `_` and `-`.
- Each skill needs `skills/<skill>/SKILL.md`.
  `search` skips folders without it, and `install` fails.
- aip copies the whole folder.
  Symlinks become regular files that contain the link target, and submodules are skipped.
  On Unix, files with mode 755 stay executable.
- `description` in the frontmatter is shown by `search`:

  ```markdown
  ---
  name: pdf
  description: Work with PDF files
  ---
  ```

  aip reads only a single-line `description:` at the start of a line inside the first `---` block.
  Surrounding quotes are removed.
  Multi-line (folded or block) values are not supported.

Then add the repository to a registry and tag releases (see above).

## Writing a registry file

```json
{
    "repositories": [
        { "name": "anthropic", "url": "https://github.com/anthropics/skills.git" },
        { "name": "mine", "url": "../my-skills" }
    ]
}
```

- `name` is the first half of every package name.
  Same character rules as skill names.
  Names must be unique inside one file.
- `url` is anything `git clone` accepts: HTTPS, SSH (`git@host:path`), or a local path.
- A relative local path resolves against the folder of the registry file.
- Unknown keys in the file or in an entry are an error.

The default registry is `$AIP_HOME/registry.json`.

### Remote registries

A `json` registry in `aip.json` takes exactly one of `path` and `url`.
Setting both, neither, or a URL in `path` is an error.

```json
{ "type": "json", "url": "https://example.com/registry.json" }
```

- aip downloads the same JSON format on every run that needs the registry, and validates it like a file.
  A cached copy makes this cheap and lets aip work offline, see [Offline use and the registry cache](#offline-use-and-the-registry-cache).
- The URL must use `https://`.
  Plain `http://` works only for `localhost`, `127.0.0.1` and `[::1]`, for local testing.
- The URL must not contain credentials.
  For a private registry, put the credentials in `auth.json`, see [Private registries](#private-registries).
- Every `url` in a remote registry must be a remote git URL: `https://`, `ssh://`, `git://` or `user@host:path`.
  Local paths and `file://` are rejected, because a remote document must not point aip at files on your machine.
- The download follows redirects, times out after 30 seconds and is limited to 10 MB.

## Multiple registries and priority

List several registries in `aip.json`.
If two list the same repository name, the first registry in the list wins.
Repositories that only a later registry lists are still available.

## Private repositories

aip never handles passwords or tokens itself.
It runs the `git` command line, so git authenticates the same way it does for your own `git clone`: SSH keys, credential helpers, and `url.<base>.insteadOf` rewrites.
If `git ls-remote <url>` works in your shell, aip works too.

### Declaring a private repository in aip.json

A registry entry of type `git` lists exactly one repository, like a `vcs` repository in Composer.
You do not need a registry file for it.

```json
{
    "registries": [
        { "type": "git", "name": "acme", "url": "git@github.com:acme/private-skills.git" },
        { "type": "json", "path": "registry.json" }
    ],
    "require": {
        "acme/review": "^1.0"
    }
}
```

- `name` follows the same rules as a repository name in a registry file.
- `url` is anything `git clone` accepts.
  A relative local path resolves against the folder of `aip.json`.
- Unknown keys are an error.
- Priority is positional, like for every registry.
  If the entry comes first, it overrides a repository with the same name in a later registry.

### SSH keys

Use an SSH URL and make sure your key is loaded (`ssh-add -l`) or configured in `~/.ssh/config`.
Test with `git ls-remote git@github.com:acme/private-skills.git`.

### Credential helper

For HTTPS URLs, let a credential helper store the token.
Git Credential Manager does this on Windows and macOS.
With the GitHub CLI, run `gh auth login` and then `gh auth setup-git`.
Test with `git ls-remote https://github.com/acme/private-skills.git`.

### CI with a token

Keep the token out of `aip.json` and tell git to add it:

```sh
git config --global url."https://x-access-token:${TOKEN}@github.com/".insteadOf "https://github.com/"
aip install
```

The URL in `aip.json` and `aip.lock` stays `https://github.com/acme/private-skills.git`, without the token.
Git adds the token only while it runs.
When stdin is not a terminal, aip sets `GIT_TERMINAL_PROMPT=0`, so a missing credential fails at once instead of waiting for a prompt that nobody can answer.
In an interactive shell, git may still prompt as usual.

### Credentials in URLs are rejected

aip copies repository URLs into `aip.json` and `aip.lock`, which you commit.
A URL such as `https://user:token@github.com/acme/x.git` or `https://token@github.com/acme/x.git` would leak the secret, so aip stops with an error before it resolves or writes anything.
The error does not repeat the URL.
`ssh://git@host/...` and `git@host:path` are fine because they carry no secret.
Remove the credentials and use one of the methods above.

## Private registries

This section is about registries that you download over HTTP(S) and that need a token or a password.
It is not about private git repositories.
Those use your git credentials, see [Private repositories](#private-repositories).
`auth.json` is never used for git URLs.

aip reads credentials from `auth.json` files and from the variable `AIP_AUTH`.
All sources use the same format, which is the one Composer uses:

```json
{
  "bearer": { "skills.example.com": "TOKEN" },
  "http-basic": { "git.example.com:8443": { "username": "me", "password": "secret" } }
}
```

- `bearer` maps a host to a token.
  aip sends `Authorization: Bearer <token>`.
- `http-basic` maps a host to a user name and password.
  aip sends `Authorization: Basic <base64 of user:password>`.
- A key is `host`, or `host:port` when the registry URL uses a port that is not the default (not 443 for https).
  It must match the host and port of the registry URL exactly.
  The case does not matter.
- The file may contain only these two keys.
  Unknown keys are an error.

Sources, from highest to lowest precedence:

1. The variable `AIP_AUTH` holds the JSON text.
   Use it in CI, where you do not want a file with secrets.
2. `auth.json` in the folder of `aip.json`.
   Only projects use it, `--global` does not.
3. `auth.json` in `$AIP_HOME`.

aip merges the sources per host.
If two sources name the same host, the one with the higher precedence wins.
Invalid JSON in a source is an error that names the source (`invalid AIP_AUTH: ...` or `invalid <path>: ...`).
The error never shows the secrets.

Rules:

- Add `auth.json` to the `.gitignore` of your project.
  Never commit it.
- On Linux and macOS, keep the file private with `chmod 600 auth.json`.
  aip warns if other users can read it.
- aip stores the secrets as plain text.
  Anyone who can read the file can use them.
  Use short-lived tokens where you can.
- aip sends a credential only to the host and port in the key.
  It sends it only over `https://`, or over `http://` to `localhost`, `127.0.0.1` and `[::1]`.
- If the server redirects to another scheme, host or port, aip does not send the credential there.
  With credentials, aip follows at most 5 redirects.
- aip never writes credentials to `aip.json`, `aip.lock`, the registry cache, error messages or logs.

Example for CI:

```sh
export AIP_AUTH='{"bearer": {"skills.example.com": "'"$SKILLS_TOKEN"'"}}'
aip install
```

## Offline use and the registry cache

aip keeps a copy of every registry that it downloads from a URL.
The copy is in `$AIP_HOME/cache/registries/<hash of the URL>.json`.
It contains the registry document and the validators of the server (`ETag`, `Last-Modified`), but never credentials.

On every run that needs the registry, aip asks the server if the copy is still current (`If-None-Match`, `If-Modified-Since`):

| Answer | What aip does |
|---|---|
| `200` | Validates the new document. Only a valid document replaces the cached copy. |
| `304 Not Modified` | Uses the cached copy and records the time of the check. |
| No connection (DNS, connect, TLS, timeout) or `5xx` | Uses the cached copy and prints `warning: cannot reach registry <url> (<reason>); using cached copy from <N> minutes/hours/days ago`. Without a cached copy it is an error. |
| `401`, `403`, `404` and other `4xx` | Error, with no fallback. aip does not hide a wrong URL or wrong credentials behind old data. |

The cache is per URL.
A changed URL starts with an empty cache.
Offline, this only covers the registry.
Repositories that are not yet in the git cache (`$AIP_HOME/cache/repos`) still need the network.

To clear the registry cache, delete the folder `$AIP_HOME/cache/registries`.

## Environment variables

| Variable | Effect |
|---|---|
| `AIP_HOME` | aip's own folder. Default `~/.aip`. Holds `registry.json`, `auth.json`, the repository cache (`cache/repos`), the registry cache (`cache/registries`) and, for `--global`, `aip.json` and `aip.lock`. |
| `AIP_AUTH` | Credentials for HTTP registries, as JSON text in the `auth.json` format. Highest precedence. See [Private registries](#private-registries). |
| `CLAUDE_CONFIG_DIR` | Only for `aip detect --global`: counts as a Claude Code marker if it names an existing directory. |
| `CODEX_HOME` | Only for `aip detect --global`: same for Codex. |
| `HOME` / `USERPROFILE` | Used to find your home folder (Unix / Windows). |

An empty `AIP_HOME` is treated as unset.
Repositories are cached as bare git mirrors, so you can delete `$AIP_HOME/cache` at any time.
Git credentials (SSH keys, credential helpers) are those of your `git` command.

## Safety: unmanaged folders and --force

aip only replaces or deletes a skill folder if `aip.lock` says aip installed it.
If `.claude/skills/pdf` already exists and the lock has no entry for a package with skill name `pdf`, `aip install` stops with:

```
error: refusing to overwrite .claude/skills/pdf: it was not installed by aip (use --force to replace it)
```

Nothing is changed in this case.
`--force` (on `install` and `update`) replaces the folder.
Two packages that would install the same skill name (for example `a/pdf` and `b/pdf`) are rejected, because skill folders are named by skill only.

aip copies each skill into a temporary folder `.<skill>.aip-tmp` next to the target and then renames it, so a failed copy does not leave half a skill.
aip writes `aip.json` and `aip.lock` before it changes skill folders; if something fails, run `aip install` again to finish.

## Troubleshooting

Messages are shown as `error: <message>`.
`<...>` marks the variable parts.

### Setup and files

| Message | Meaning and fix |
|---|---|
| `cannot determine home directory` | `HOME` / `USERPROFILE` is not set. Set it, or set `AIP_HOME` (project scope only). |
| `cannot determine current directory: <os error>` | The working directory was deleted or is not accessible. `cd` elsewhere. |
| `no aip.json in <dir>; run `aip install <repository>/<skill>` to add a skill` | `aip install` without packages in a folder without `aip.json`. Add a skill first. |
| `no aip.json in <dir>` | `update` or `remove` without `aip.json`. Use `-g` for global, or change directory. |
| `package '<name>' is not required in <aip.json path>` | `update` or `remove` of a package that is not in `require`. Check the name with the file. |
| `cannot read <file>: <os error>` | Permission or I/O problem with `aip.json` or `aip.lock`. |
| `invalid <file>: <parse error>` | The JSON is malformed or has a wrong shape, for example an unknown registry `type`. Fix the file. For `aip.lock`, delete it and run `aip install`. |
| `cannot create <dir>` / `cannot write <file>` / `cannot remove <path>` / `cannot serialize <file>` | File system error (permissions, read-only disk, folder in use on Windows). |
| `aip.lock is out of date with aip.json:` followed by `- <name>: not in aip.lock` or `- <name>: locked <version> does not satisfy <constraint>` | You edited `aip.json` by hand. Run the `aip update <names>` command shown in the message. |
| `aip.lock: <name> is locked to <url>, but the registries list <url> for '<repository>'; run `aip update <name>` to re-resolve it` | The lock points to another URL than your registries. Run `aip update <name>` if the registry is right; otherwise check who changed the lock. |
| `Waiting for another aip process (<aip.json path>)...` | Not an error. Another aip run holds the project lock (`$AIP_HOME/locks`); aip continues when it ends. The same message with a URL means another run is fetching that repository. |
| `invalid aip.lock: package <name> has path '<path>', expected 'skills/<skill>'` | The lock was edited or is corrupt. Delete it and run `aip install`. |

### Providers

| Message | Meaning and fix |
|---|---|
| `no AI provider with skills support detected in <dir>; add "providers" to <aip.json> (e.g. ["claude-code"]) or run aip init --provider <id>` | Auto-detection found nothing aip can install into. Run `aip init --provider <id>`, create e.g. `.claude/`, or set `providers`. |
| `unknown provider '<id>' in <aip.json> (known: <ids>)` | Typo in `providers`. Use an id from the list. |
| `none of the providers in <aip.json> support skills` | Only providers without a skill folder are listed. Add one that has. |
| `warning: provider '<id>' does not support skills; skipping` | Not an error; that provider is ignored. |
| `cannot scan <dir>: <os error>` | The folder cannot be read. |
| `<path>: <os error>` (from `detect`) | The path does not exist or is not a directory (`not a directory`). |

### Names and constraints

| Message | Meaning and fix |
|---|---|
| `invalid package name '<name>' (expected <repository>/<skill> using lowercase letters, digits, '.', '_', '-')` | Name has uppercase letters, extra `/`, `..`, or a leading `.`/`-`. |
| `invalid version constraint '<input>': <reason>` (prefixed with `<package>: ` when it comes from install/update) | Constraint syntax error: empty, bad number, operator without version, bad branch name. See the table above. |
| `<package>: no version matches '<constraint>' (available: <list>)` | No tag satisfies the constraint. Pre-releases are excluded unless named. `available: none` means no version tags; use `dev-<branch>`. |
| `<package>: branch '<branch>' not found in <url>` | The `dev-` branch does not exist in the repository. |
| `<package>: skill '<skill>' not found in <url> at <version> (expected skills/<skill>/SKILL.md)` | The skill does not exist at that version. Check the name and tag. |
| `tag '<tag>' does not point to a commit` | The tag is broken. Ask the repository owner. |
| `default branch '<branch>' has no commits` | Empty repository. |
| `<a> and <b> both install a skill named '<skill>'` | Two packages share a skill name. Remove one. |

### Registries

| Message | Meaning and fix |
|---|---|
| `registry not found at <path> (create it with {...} or list registries under "registries" in aip.json)` | No `$AIP_HOME/registry.json`, or a path in `registries` is wrong. |
| `cannot read registry <path>: <os error>` | Permission problem. |
| `invalid registry <path>: <parse error>` | Malformed registry file or unknown keys. |
| `cannot download registry <url>: HTTP <code> <reason>` | The server answered with an error status. Check the URL. |
| `cannot download registry <url>: HTTP 401 Unauthorized; add credentials for <host> to auth.json or AIP_AUTH ...` | The registry needs credentials. See [Private registries](#private-registries). Same for `403`. |
| `... HTTP 401 Unauthorized; credentials for <host> were rejected` | The server refused the token or password from `auth.json` or `AIP_AUTH`. Check that it is valid and that the key matches the host and port. |
| `invalid AIP_AUTH: ...` / `invalid <path>: ...` | The credentials are not valid JSON in the documented format. The message shows the position, never the secrets. |
| `warning: <path> is readable by other users; run chmod 600 <path>` | Other users can read your `auth.json`. Run the command shown. On WSL, Windows drives such as `/mnt/c` ignore `chmod` unless they are mounted with the `metadata` option (`[automount] options = "metadata"` in `/etc/wsl.conf`); otherwise keep `auth.json` in `$AIP_HOME` on the Linux filesystem or use `AIP_AUTH`. |
| `warning: cannot reach registry <url> (<reason>); using cached copy from <age> ago` | The server was not reachable or answered `5xx`. aip used the cached registry. See [Offline use and the registry cache](#offline-use-and-the-registry-cache). |
| `warning: cannot update registry cache: ...` | aip could not write under `$AIP_HOME/cache/registries`. The run continues. |
| `cannot download registry <url>: <error>` | DNS, connection, TLS or timeout (30 s) problem. Check the network and the URL. |
| `registry <url> is larger than 10 MB` | The download exceeds the size limit. |
| `registry <url> uses plain http; use https` | Use an `https://` URL (`http://` is only allowed for localhost). |
| `repository '<name>' in registry <url>: a remote registry may only list remote git URLs` | Entries of a downloaded registry must be `https://`, `ssh://`, `git://` or `user@host:path` URLs. |
| `a "json" registry needs "path" or "url"` / `... either "path" or "url", not both` / `"path" is a URL; use "url" instead` | Fix the registry entry in `aip.json` (shown as `invalid aip.json: ...`). |
| `invalid repository name '<name>' in registry <path> (use lowercase letters, digits, '.', '_', '-')` | Rename the entry. |
| `repository '<name>' in <registry>: URL contains credentials; remove them ...` | The URL has a password or token. See [Private repositories](#private-repositories). |
| `duplicate repository name '<name>' in registry <path>` | Names must be unique in one file. Use a second registry for overrides. |
| `unknown repository '<name>' (not listed in any registry: <paths>)` | The first half of the package name is not in any registry. For a locked package the message starts with `<package>: `. |
| `cannot resolve <url>: <os error>` | A relative local URL could not be made absolute. |

### Git

| Message | Meaning and fix |
|---|---|
| `git not found; aip needs git installed and on PATH` | Install git. |
| `cannot run git: <os error>` | git exists but cannot start. |
| `cannot clone <url>: <git message>` | Network, URL or credentials. Try `git clone <url>` yourself. |
| `hint: the repository may be private; ...` | Printed after a clone or fetch error that looks like an authentication failure. See [Private repositories](#private-repositories). |
| `cannot fetch <url>: <git message>` | Same, for an already cached repository. Offline use is not supported. |
| `cannot create cache directory <dir>` / `cannot move clone to <dir>` | File system error under `$AIP_HOME/cache`. |
| `warning: skipping repository '<name>': <reason>` | `search` could not read one repository; the others are still shown. |

### Safety

| Message | Meaning and fix |
|---|---|
| `refusing to overwrite <path>: it was not installed by aip (use --force to replace it)` | See [Safety](#safety-unmanaged-folders-and---force). Move the folder away, or use `--force`. |
