# 0009 auth.json for HTTP registries

- Status: Accepted
- Date: 2026-11-05
- Amends: [0007](0007-authentication-delegated-to-git.md) in part

## Context

[0007](0007-authentication-delegated-to-git.md) delegates authentication to git.
That works for repositories, because aip runs the `git` CLI.
A registry downloaded over HTTP is different: aip itself makes the request, so git cannot help.
Teams that host a private registry document behind a token or Basic authentication could not use it.
The same change adds a cache for downloaded registries, so that a short outage or no network does not stop every command.

## Decision drivers

- Keep secrets out of `aip.json` and `aip.lock`, which are committed.
- Use a format that users know.
- Never send a secret to a host that the user did not name.
- Do not add a dependency.

## Considered options

1. Credentials in the registry URL.
2. Fields for headers in `aip.json`.
3. Composer-style `auth.json` and `AIP_AUTH`, matched by host.

## Decision

Use option 3, for HTTP registries only.

- Format: `bearer` and `http-basic` maps, keyed by `host` or `host:port`.
- Sources by precedence: `AIP_AUTH`, `auth.json` next to `aip.json` (project scope), `$AIP_HOME/auth.json`.
- aip sends a credential only over https (or http to localhost), only to the exact host and port, and drops it on a redirect to another origin.
- Secrets never appear in errors, `Debug` output or the cache.
  aip warns when another user can read an `auth.json`.
- Git repositories keep authenticating through git.
  `check_no_credentials` still rejects credentials in every URL.
- Downloaded registries are cached in `$AIP_HOME/cache/registries` and revalidated with `ETag` and `Last-Modified`.
  The cached copy replaces the download only when the server is unreachable or answers 5xx, never on 4xx.

## Consequences

- Good: private HTTP registries work, in CI too, without secrets in committed files.
- Good: the registry works offline once it was downloaded.
- Bad: aip now stores secrets in plain text in `auth.json`.
  The file permission check and `.gitignore` advice reduce this risk, they do not remove it.
- Bad: two authentication mechanisms exist, one for registries and one for repositories.
  The user guide keeps them apart.
- Bad: a stale cached registry can hide a change on the server while the server is down.
  The warning names the age.
