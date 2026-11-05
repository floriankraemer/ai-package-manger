# 0007 Authentication delegated to git

- Status: Accepted; amended in part by [0009](0009-auth-json-for-http-registries.md)
- Date: 2026-11-05

> Amended: this decision still holds for git repositories.
> It no longer holds for registries downloaded over HTTP.
> They use `auth.json` ([0009](0009-auth-json-for-http-registries.md)).

## Context

Teams keep skills in private repositories.
aip reads every repository through the `git` CLI ([0003](0003-git-cli-instead-of-libgit2.md)), so git can already authenticate.
URLs are written to `aip.json` and `aip.lock`, which are committed, so a secret in a URL would leak.

## Decision drivers

- Keep secrets out of aip and out of committed files.
- Use mechanisms that users and CI systems already know.
- Fail fast in non-interactive runs.

## Considered options

1. A Composer-style `auth.json` where aip stores tokens per host.
2. Delegate to git: SSH keys, credential helpers, `url.<base>.insteadOf`.

## Decision

Use option 2.

- aip stores no credentials and has no auth configuration.
- `check_no_credentials` rejects URLs with a password, or a token in an http(s) URL, in every registry entry.
- With stdin not a terminal, git runs with `GIT_TERMINAL_PROMPT=0`.
- Clone and fetch failures that look like authentication problems get a hint that points to `git ls-remote <url>`.
- The `git` registry type lets a project declare a private repository inline.

## Consequences

- Good: no secret storage, no new dependency, same behaviour as plain `git clone`.
- Good: CI uses one `git config url.<base>.insteadOf` line; the URL in `aip.json` stays token-free.
- Bad: aip cannot set tokens itself; users must configure git.
- Bad: the auth hint is a substring match on git's English output and can miss other locales.
- Future option: aip could inject a token into git processes through the environment variables `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>`.
  It must not use command-line arguments, because those show up in process listings.
