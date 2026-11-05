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
| Repository name (registry) and skill name (package) | `valid_name`: starts with `a-z` or `0-9`, then `a-z 0-9 . _ -`. Rules out `..`, separators, drive letters, hidden folders. | `registry::valid_name`, `split_package` |
| Package name from the command line, `aip.json`, `aip.lock` | `split_package` | `ops.rs` |
| Lock `path` | Must equal `skills/<skill>`. A lock cannot point to other folders in the repository. | `ops::skill_of` |
| Skill names found by `search` | Skipped when invalid. | `ops::search` |
| Branch in `dev-<branch>` | No leading `-`, `..`, whitespace, control characters or `~ ^ : ? * [ \`. | `constraint::valid_branch` |
| Registry entries | Valid unique names, no unknown keys (`deny_unknown_fields`). | `registry/json.rs`, `registry/git.rs` |
| Repository URLs | No credentials (`user:secret@`, or `token@` for http/https). The URL is not echoed. | `registry::check_no_credentials` |
| Git invocations | `--` before the URL in `clone`, and before paths in `ls-tree`. | `git.rs` |

Known gaps in this area are listed in [11](11-risks-and-technical-debt.md).

## Safe writes

- **Staging then rename.** A skill is exported to `<target>/.<skill>.aip-tmp`, the old folder is deleted, and the staging folder is renamed into place. A failed export leaves the previous folder intact; only the short window between delete and rename can leave neither.
- **Lock before files.** `finish` writes `aip.json` and `aip.lock` first and applies the plan second. An interrupted run is repaired by running `aip install`, because the plan treats a missing folder as an install.
- **Plan before write.** `plan` runs fully, including the unmanaged-folder check and duplicate skill name check, before any file is touched.
- **Trusted sources.** `finish` checks that every locked `source.url` equals the URL the registries list for the repository, before `plan`. A mismatch is an error; `aip update <name>` repairs it.
- **No silent re-resolve.** `refresh` resolves only the requirements it is told to (`forced`). Other requirements that are missing from the lock or no longer satisfied make the command fail before anything is written.
- **Ownership by lock.** aip deletes or replaces only folders of packages in the old lock (`managed`), unless `--force`.
- **Idempotent writes.** `write_json` compares bytes and leaves unchanged files alone.

## Caching as bare mirrors

- `Repo::sync` keeps one mirror per URL in `$AIP_HOME/cache/repos/<key>`; the key is the URL with every character outside `A-Za-z0-9._-` replaced by `_`.
- First use: `git clone --mirror` into `<key>.partial`, then rename. An interrupted clone is never mistaken for a valid cache.
- Later use: `git fetch --prune`.
- Content is read from objects (`ls-tree`, `cat-file`) at a commit SHA. There is never a working tree.
- The `Resolver` syncs each URL at most once per run.
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
