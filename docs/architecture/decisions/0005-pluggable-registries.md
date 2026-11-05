# 0005 Pluggable registries with ordered priority

- Status: Accepted
- Date: 2026-11-05

## Context

Users need a way to name repositories and find them.
A local JSON file is enough now.
An HTTP server with search and metadata is a likely later addition.
Teams also want to combine a shared registry with their own entries.

## Decision drivers

- Start simple without blocking a server later.
- Predictable conflict handling when names collide.
- No change to resolution and installation when a new registry type arrives.

## Considered options

1. Hard-coded single JSON file.
2. A trait `Registry` with a tagged-enum configuration, several registries, and priority by order.
3. A plugin mechanism (external executables).

## Decision

Use option 2.

- `trait Registry { location(); repositories() }` returns `Entry { name, url }`.
- `RegistryConfig` is a serde enum tagged by `type`; today only `json` with a `path`. Unknown types fail to parse with an error that names the variant.
- `Registries` merges the list. For duplicate names the first registry wins, like Composer repository priority. Later registries still add their other names.
- `aip.json` `registries` replaces the default `$AIP_HOME/registry.json`. It does not add to it.
- A package name is `<repository name>/<skill>`, where the name comes from the registry, not from the URL.
- Inside one registry, duplicate names are an error.
- Relative local URLs and paths resolve against the registry file or `aip.json`.

How to add an HTTP registry is described in [arc42 section 05](../arc42/05-building-block-view.md#registry-abstraction).

## Consequences

- Good: a new type is a new enum variant plus a trait implementation; `ops.rs` is untouched for the basic case.
- Good: overriding a public registry entry is a matter of listing your own registry first.
- Bad: the trait only returns a list, so server features such as search or metadata need extra trait methods (with defaults).
- Bad: all registries load fully and eagerly on first use; one broken registry fails the command.
- Bad: names from a lower-priority registry are shadowed silently.
