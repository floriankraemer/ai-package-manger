# 0008 HTTP client: ureq with rustls

- Status: Accepted
- Date: 2026-11-05

## Context

aip downloads remote JSON registries and will later talk to a server registry.
It needs a small HTTPS client.
aip is a synchronous CLI, and it must build on Linux, macOS and Windows CI without extra toolchains.

## Decision drivers

- Few dependencies and fast builds; no async runtime.
- No system tools needed at run time or build time (no cmake, NASM).
- Controlled timeout, redirects and size limit.

## Considered options

1. Shell out to `curl`.
2. `reqwest` (async, tokio).
3. `ureq` 3 with rustls and the `ring` crypto provider.

## Decision

Use option 3, behind one module, `src/http.rs`.

- Default features are off; only `rustls` is enabled, which uses `ring`.
  The `aws-lc-rs` provider is avoided because it needs cmake and NASM on Windows.
- `http::get_text(url, max_bytes)` has a 30 second overall timeout, follows redirects, sets `User-Agent: aip/<version>` and enforces the size limit while reading.

## Consequences

- Good: no `curl` dependency, same behaviour on all platforms, errors under our control.
- Good: blocking API fits the synchronous `Registry` trait.
- Good: a later server registry reuses `http.rs`.
- Bad: a TLS stack (rustls, ring, webpki-roots) grows the binary and build time.
- Bad: root certificates are bundled (webpki-roots), so a corporate CA is not trusted without further work.
- Rejected: `curl` would need a process per request and varies by platform; `reqwest` pulls in tokio for no benefit here.
