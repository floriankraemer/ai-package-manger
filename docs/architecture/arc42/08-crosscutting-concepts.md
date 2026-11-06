# 08 Cross-cutting concepts

Purpose: rules that apply across modules.

## Error handling

- One error type, `aip::Error`, wraps a message that is ready to show to the user.
- `Result<T>` is the crate alias.
- The crate-private trait `Context` adds a prefix to a lower-level error: `cannot write <path>: <os error>`.
  Use it on every `io` and `serde` call so messages name the file.
- No panics for user mistakes. The only `expect` calls are on serialization of own types and a `OnceCell` that was just set.
- `main` prints `error: <message>` to stderr and returns exit code 1.
- Library code never exits the process.
- Messages say what failed, and where possible what to do (`use --force`, `add "providers" to ...`, the expected format).
  The full list with user advice is in the [troubleshooting section](../../user-guide.md#troubleshooting).
- One tolerated failure: `search` reports an unreachable repository as a warning and goes on.

## Input validation at trust boundaries

Names become folder names, and the lock file is user-editable, so neither is trusted.

| Input | Check | Where |
|---|---|---|
| Skill name (package) | `valid_name`: starts with `a-z` or `0-9`, then `a-z 0-9 . _ -`. Rules out `..`, separators, drive letters, hidden folders. | `registry::valid_name`, `split_package` |
| Repository name (registry), explicit or derived from the URL (`owner/repo`) | `valid_repository_name`: `/`-separated segments, each a `valid_name`. `derive_name` lowercases the URL path and fails when a segment is invalid ([ADR 0010](../decisions/0010-repository-names-from-urls.md)). | `registry::valid_repository_name`, `registry::derive_name` |
| Package name from the command line, `aip.json`, `aip.lock` | `split_package` (repository is everything before the last `/`) | `ops.rs` |
| Lock `path` | Must equal `skills/<skill>`. A lock cannot point to other folders in the repository. | `ops::folder_of` |
| `"as"` (folder alias) in `aip.json`, `aip.lock`, `--as` | `valid_name`, like a skill name. The `aip.json` object form denies unknown keys. | `manifest::Requirement`, `ops::folder_of`, `ops::install_as` |
| Lock `reference` against `version` | A tag version must be the commit of a tag the resolver would pick for it; `dev-<branch>` must be a commit on that branch. Checked for every package before it is exported. | `ops::lock_matches` |
| Skill folder paths (project scope) | No existing component between the project root and a provider's skill folder may be a symbolic link. Global scope is not checked, because dotfiles are often symlinked. | `ops::checked_dir` |
| Skill names found by `search` | Skipped when invalid. | `ops::search` |
| Branch in `dev-<branch>` | No leading `-`, `..`, whitespace, control characters or `~ ^ : ? * [ \ #`. | `constraint::valid_branch` |
| Commit pin `#<hex>` | 7 to 64 hexadecimal digits; resolved with `rev-parse --verify` and checked with `merge-base --is-ancestor` against the branch. | `constraint::Constraint::parse`, `ops::Resolver::resolve` |
| Stability flag `@dev\|alpha\|beta\|RC\|stable` | Range constraints only; a pre-release matches if its stability is at least the flag. | `constraint::Stability` |
| Registry entries | Valid unique names, no unknown keys (`deny_unknown_fields`). An invalid entry is skipped with a warning, but its name stays claimed. | `registry::Listing::collect`, `registry/json.rs`, `registry/git.rs` |
| Repository URLs | No credentials (`user:secret@`, or `token@` for http/https). The URL is not echoed. | `registry::check_no_credentials` |
| Repository URLs, leading `-` | Rejected, so `git config` never reads the URL as an option. The URL is not echoed. | `registry::check_entry` |
| Git invocations | `--` before the URL in `clone`, and before paths in `ls-tree`. | `git.rs` |

Known gaps in this area are listed in [11](11-risks-and-technical-debt.md).

## Safe writes

- **Plan before write.** `unique_folders` and `plan` run fully, including the duplicate folder check, the ownership check and the local-changes check, before any file is touched. `--dry-run` stops here and reports the plan.
- **Stage everything, then commit.** Every export (each package in each target) goes to `<target>/.<folder>.aip-tmp` first, with its marker. For a package with `"as"`, the staged `SKILL.md` gets `name: <alias>` (`ops::rename_skill`), and its marker records the id of the rewritten file. If any export fails, all staging folders are deleted and nothing else has changed. Only then are `aip.json` and `aip.lock` written.
- **Swap.** Each skill folder is moved to `.<folder>.aip-old`, the staging folder is renamed into place, and the old folder is deleted. Removals and pruned folders are deleted last.
- **Repair by markers.** If a swap fails after the lock was written, the folders' markers show which ones are stale, and `aip install` rewrites them. The window is marked `ponytail:` in `ops::finish`.
- **Trusted sources.** `finish` checks that every locked `source.url` equals the URL the registries list for the repository, before `plan`. A mismatch is an error; `aip update <name>` repairs it.
- **Trusted references.** Before exporting, `Resolver::locked_repo` checks that the locked commit is what its version label says (see the validation table). A mismatch is an error; `aip update <name>` repairs it.
- **Portable lock URLs.** In memory, locked local URLs are absolute. `Project::read_lock` resolves them against the aip.json folder; `Project::write_lock` stores them relative to it (`registry::portable_url`, `relative_path`), or absolute when no relative path exists. `verify_sources` and `check` compare absolute URLs.
- **Versions need the skill.** A repository tag versions a skill only if `skills/<skill>/SKILL.md` exists at that tag (`ops::newest_with_skill`, newest first, stopping at the first hit). Without such a tag the skill is `dev-<default branch>`.
- **No silent re-resolve.** `refresh` resolves only the requirements it is told to (`forced`). Other requirements that are missing from the lock or no longer satisfied make the command fail before anything is written.
- **Idempotent writes.** `write_json` compares bytes and leaves unchanged files alone.

## Ownership of skill folders

- Every skill folder aip installs holds `.aip-package`: `{"name": "<package>", "reference": "<commit sha>", "files": {"<path>": "<blob id>"}}`.
- The folder name is the package's `"as"` if set, else its skill name (`ops::folder_of`). Ownership, pruning, rename takeover and deletion all work on that folder name. Two packages with the same folder are an error with a hint that suggests `"as"` (`ops::unique_folders`).
- A folder belongs to package P if and only if its marker parses and names P. The lock is never used to decide ownership.
- A folder is up to date if its marker names the package and the locked commit. Anything else is rewritten.
- aip replaces a folder without a matching marker only with `--force`, and never deletes one. Removal of such a folder in a target gives `warning: not removing <path>: it was not installed by aip`.
- A skill folder that is a symbolic link is never followed and counts as not installed by aip.
- **Pruning.** Skill folders of known providers that are not in `aip.json` are scanned too. A folder there whose marker names a package of the old or new lock is deleted, so removing a provider or a package cleans every provider's folder.
- **Takeover.** A folder whose marker names an old lock entry from the same repository is taken over without `--force` when that entry leaves the folder in this run: it was dropped (a rename) or got another `"as"`. No folder written in a run is deleted in the same run.
- **Moves.** A changed `"as"` is copied from `aip.json` into the lock by `refresh`, without re-resolving. `plan` writes the new folder, deletes the old one if its marker names the package, and reports `Change::Update` with `folder: Some((old, new))`.
- **Local changes.** `files` records the blob id of every exported file, from `ls-tree`. `ops::local_changes` lists missing, added (other than the marker) and differing files. It hashes all files present in one `git hash-object --no-filters --stdin-paths` (`git::hash_files`) with `--git-dir` of the cached mirror, so the ids use the repository's object format. Without a mirror it hashes as SHA-1 outside any repository, and treats 64-character ids as unknown. Markers without `files` are unknown, and unknown counts as unmodified.
- `plan` calls `guard_changes` before it overwrites or deletes a folder of aip (targets, takeovers, pruned copies, removals and moves). A modified folder is an error unless `--force`. Folders that stay as they are are not hashed. `check` hashes every folder that holds the locked commit and reports `Status::ModifiedLocally`.
- **Migration.** Folders from before markers existed have none. Such a folder is rewritten (with marker) without `--force` only if the old lock has the package from the same repository and the folder's `SKILL.md` equals the one at the old locked commit. This rule is for writing only; deletion always needs a marker.

## Caching as bare mirrors

- `Repo::sync` keeps one mirror per URL in `$AIP_HOME/cache/repos/<key>`; the key is the URL with every character outside `A-Za-z0-9._-` replaced by `_`.
- First use: `git init --bare` into `<key>.partial`, fetch, then rename. An interrupted clone is never mistaken for a valid cache.
- Only `refs/heads/*` and `refs/tags/*` are mirrored, so refs such as GitHub's `refs/pull/*` never reach the cache.
- Caches made by an older `clone --mirror` are migrated on open: the refspecs are rewritten and the other refs are deleted.
- Later use: `git fetch --prune --tags`, then the local `HEAD` follows the remote's default branch (`ls-remote --symref`).
- `Repo::open` reads the cache without network access; `Repo::fetch` updates it; `Repo::sync` does both.
- Every git call sets `GIT_ALLOW_PROTOCOL=https:ssh:git:file` and `GIT_PROTOCOL_FROM_USER=0`, so `ext::` and other transports cannot run commands.
- Export refuses files over 20 MiB, skills over 100 MiB or 5000 files, and path names that Windows cannot create (reserved device names, trailing `.` or space, `<>:"|?*`, control characters), on every platform.
- Repository and skill names also reject Windows device names and a trailing `.`.
- Skill descriptions have every control character replaced by a space, so SKILL.md cannot send terminal escape sequences.
- Content is read from objects (`ls-tree`, `cat-file`) at a commit SHA. There is never a working tree.
- The `Resolver` syncs each URL at most once per run.
- Exporting a locked commit uses `Repo::open` and does not fetch when the mirror has the commit and it matches its version label. Otherwise aip fetches once and checks again.
- The cache is disposable.

## Caching of downloaded registries

- `registry/cache.rs` keeps one file per registry URL: `$AIP_HOME/cache/registries/<fnv1a(url), 16 hex digits>.json`.
  It holds `url`, `etag`, `last_modified`, `fetched_at` (Unix seconds) and the raw `body`.
  A file whose `url` differs (hash collision) is ignored.
- Every run sends `If-None-Match` and `If-Modified-Since` when a cached entry exists.
- `200`: the document is parsed and validated first.
  Only then does aip replace the entry, by writing a temp file in the same folder and renaming it.
- `304`: aip uses the cached body and rewrites the entry with a new `fetched_at`.
- Unreachable server (DNS, connect, TLS, timeout) or `5xx`: aip warns on stderr and uses the cached body.
  Without an entry the original error stands.
- `4xx`, too large, not UTF-8: always an error.
  A fallback would hide a wrong URL or wrong credentials.
- A failed cache write is a warning, not an error.
- The cache is disposable and holds no credentials.

## Authentication

- Authentication is delegated to git ([ADR 0007](../decisions/0007-authentication-delegated-to-git.md)).
  SSH keys, credential helpers and `url.<base>.insteadOf` all work because aip runs the `git` CLI.
- This holds for git repositories only.
  Registries downloaded over HTTP use `auth.json` and `AIP_AUTH` ([ADR 0009](../decisions/0009-auth-json-for-http-registries.md)).
- aip stores no git secrets.
  There is no token setting in `aip.json`.
- Credential-in-URL guard: `check_no_credentials` rejects URLs with a password or an http(s) token before anything is resolved. This keeps secrets out of the committed `aip.json` and `aip.lock`.
- Non-interactive runs: when stdin is not a terminal, `git.rs` sets `GIT_TERMINAL_PROMPT=0` on every git process, so a missing credential fails fast. Interactive shells keep git's prompting.
- When a clone or fetch fails with an authentication-looking message, aip appends a hint to check `git ls-remote <url>`.

### Credentials for HTTP registries

- `auth.rs` reads `bearer` and `http-basic` maps (strict, `deny_unknown_fields`) from three sources: `AIP_AUTH`, `auth.json` next to `aip.json` (project scope only) and `$AIP_HOME/auth.json`.
  Sources are merged per host; the highest one wins.
- Keys are `host` or `host:port` (port only when it is not the default) and match the registry URL exactly, ignoring case.
- A credential is attached only if the request is https (or http to a loopback host) and the key matches.
- With a credential attached, `http.rs` turns off ureq's redirects and follows up to 5 by hand.
  A redirect to another scheme, host or port continues without the credential, and must itself be https or loopback.
- Secrets never reach messages: `Credential` and `Auth` have redacting `Debug` impls, parse errors show only the position and kind, and the cache stores no headers.
- On Unix, aip prints a warning once if an `auth.json` is readable by group or others (`mode & 0o077`).
- 401 and 403 produce an error that names the host and points to the user guide; there is no fallback to the cache.

## Remote registries (trust boundary)

- A registry downloaded from a URL is untrusted input.
- The registry URL must be `https://`; `http://` is accepted only for `localhost`, `127.0.0.1` and `[::1]`, and the URL must not carry credentials.
- Every entry of a remote registry must be a remote git URL (`https://`, `ssh://`, `git://`, `user@host:path`).
  Local paths and `file://` are rejected, so a registry cannot make aip read the user's file system.
- `http::get` limits a download to 30 seconds per request and 10 MB, enforced while reading.
- Names and credentials in entries go through the same checks as in file registries.

## Registry failure isolation (dependency confusion)

- `Registries` loads each registry lazily and at most once per run; success and failure are both cached.
- `get(name)` walks the registries in priority order, and the first registry that lists `name` wins.
- If a registry before the match failed to load, `get` returns an error.
  It never falls through silently, because the lower registry could supply a different URL for the same name.
- `get_locked(name, locked_url)` downgrades that error to a warning when the lower match has exactly the URL from aip.lock.
- `entries()` (used by `search`) skips failed registries with one warning each.
- Inside one document, an invalid entry is skipped with a warning, but the registry still claims its name (explicit, or derived from the URL).
  `get` on a claimed name fails with `repository '<name>' in registry <location> is invalid`.
  Duplicate names in one document are handled the same way.
- Malformed JSON and a wrong top-level shape fail the whole registry.
- Warnings never contain URLs and are printed once per run.

## Concurrency

- **Project lock.** `install`, `update` and `remove` take an exclusive advisory lock (`std::fs::File::lock`) as their first step, before reading `aip.json`. The file is `$AIP_HOME/locks/<fnv1a of the absolute aip.json path, 16 hex digits>.lock`. It is held until the operation returns. `search` takes no project lock.
- **Repository lock.** `Repo::sync` holds `$AIP_HOME/cache/repos/<key>.lock` while it clones or fetches, because projects share the cache.
- **Blocking.** If another process holds a lock, aip prints `Waiting for another aip process (<what>)...` to stderr once and waits. The OS releases the lock when the process ends, so a crashed run never blocks later runs.
- Lock files stay on disk; only the lock on the open file matters.

## Logging and output

There is no logging framework.
Result output is printed by `main.rs` only.
Progress (`Fetching <url>`) and warnings go to stderr from `ops.rs`.

## Configuration

`AIP_HOME` (default `~/.aip`) is the only aip-specific variable.
Everything else is in `aip.json`.
`aip.json` unknown keys are preserved through `Manifest.extra` (flattened map).

## Testing

Unit tests sit next to the code (`#[cfg(test)]`).
The `ops` tests create real git repositories in temp folders and run install, update, remove and search against them.
No mocks.
