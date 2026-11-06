//! A registry stored as a JSON document, read from a local file or downloaded over HTTP(S).
//!
//! ```json
//! { "repositories": [ { "url": "https://github.com/anthropics/skills.git" } ] }
//! ```

use super::cache::{self, CacheEntry};
use super::{
    Entry, Listing, RawEntry, Registry, RegistryContext, check_entry, check_no_credentials,
    is_remote_git_url,
};
use crate::http::{self, Request, Response};
use crate::{Context, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Largest registry document aip downloads.
const MAX_REGISTRY_BYTES: u64 = 10_000_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    repositories: Vec<RawEntry>,
}

/// Where a `json` registry is configured to come from: exactly one of `path` or `url`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "RawSource", into = "RawSource")]
pub enum JsonSource {
    /// Local file; relative paths resolve against the aip.json folder.
    Path(String),
    /// `https://` (or `http://` on localhost) URL of the document.
    Url(String),
}

/// The `path` / `url` fields as written in aip.json, before the either-or check.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

impl TryFrom<RawSource> for JsonSource {
    type Error = String;

    fn try_from(raw: RawSource) -> std::result::Result<Self, String> {
        match (raw.path, raw.url) {
            (Some(_), Some(_)) => {
                Err(r#"a "json" registry needs either "path" or "url", not both"#.into())
            }
            (None, None) => Err(r#"a "json" registry needs "path" or "url""#.into()),
            (Some(path), None) if is_http_url(&path) => {
                Err(r#""path" is a URL; use "url" instead"#.into())
            }
            (Some(path), None) => Ok(Self::Path(path)),
            (None, Some(url)) => Ok(Self::Url(url)),
        }
    }
}

impl From<JsonSource> for RawSource {
    fn from(source: JsonSource) -> Self {
        match source {
            JsonSource::Path(path) => Self {
                path: Some(path),
                url: None,
            },
            JsonSource::Url(url) => Self {
                path: None,
                url: Some(url),
            },
        }
    }
}

fn is_http_url(s: &str) -> bool {
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Where a registry document is read from.
enum Source {
    File(PathBuf),
    Url { url: String, ctx: RegistryContext },
}

/// Registry read from a JSON file or downloaded from a URL.
pub struct JsonRegistry {
    source: Source,
}

impl JsonRegistry {
    /// Registry read from the file at `path`.
    pub fn file(path: PathBuf) -> Self {
        Self {
            source: Source::File(super::normalize(&path)),
        }
    }

    /// A relative file path resolves against `base` (the aip.json folder); URLs use the
    /// cache and credentials in `ctx`.
    pub fn new(source: &JsonSource, base: &Path, ctx: &RegistryContext) -> Self {
        let source = match source {
            JsonSource::Path(path) => Source::File(super::normalize(&base.join(path))),
            JsonSource::Url(url) => Source::Url {
                url: url.clone(),
                ctx: ctx.clone(),
            },
        };
        Self { source }
    }

    /// Reads and checks the document; a URL is revalidated against its cached copy.
    fn load(&self) -> Result<Listing> {
        match &self.source {
            Source::File(path) => self.parse(&read_file(path)?),
            Source::Url { url, ctx } => {
                check_registry_url(url)?;
                self.load_url(url, ctx)
            }
        }
    }

    /// Conditional download with fallback to the cached copy.
    ///
    /// `200`: the new document is cached only after it passed validation. `304`: the cached
    /// document is used and its `fetched_at` refreshed, so the age in the offline warning
    /// counts from the last time the server confirmed it. Unreachable server or 5xx: the
    /// cached copy stands in with a warning. Any 4xx is an error, to not hide auth or URL problems.
    fn load_url(&self, url: &str, ctx: &RegistryContext) -> Result<Listing> {
        let cached = cache::load(&ctx.cache_dir, url);
        let credential = ctx.auth.credential_for(url)?;
        let request = Request {
            if_none_match: cached.as_ref().and_then(|c| c.etag.as_deref()),
            if_modified_since: cached.as_ref().and_then(|c| c.last_modified.as_deref()),
            auth: credential.as_ref(),
        };
        match http::get(url, &request, MAX_REGISTRY_BYTES) {
            Ok(Response::Ok {
                body,
                etag,
                last_modified,
            }) => {
                let entries = self.parse(&body)?;
                let fresh = CacheEntry {
                    url: url.into(),
                    etag,
                    last_modified,
                    fetched_at: cache::now(),
                    body,
                };
                store_or_warn(&ctx.cache_dir, &fresh);
                Ok(entries)
            }
            Ok(Response::NotModified) => {
                let cached = cached.ok_or_else(|| {
                    Error::new(format!(
                        "cannot download registry {url}: server answered 304 Not Modified to an unconditional request"
                    ))
                })?;
                let entries = self.parse(&cached.body)?;
                store_or_warn(
                    &ctx.cache_dir,
                    &CacheEntry {
                        fetched_at: cache::now(),
                        ..cached
                    },
                );
                Ok(entries)
            }
            Err(err) if err.allows_stale_copy() => {
                let reason = err.reason.clone();
                let cached = cached.ok_or(err)?;
                let age = cache::now().saturating_sub(cached.fetched_at);
                eprintln!("{}", offline_warning(url, &reason, age));
                self.parse(&cached.body)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Parses a registry document; invalid entries are recorded, not fatal.
    fn parse(&self, text: &str) -> Result<Listing> {
        let file: File = serde_json::from_str(text).context(|| "invalid JSON".to_string())?;
        Ok(Listing::collect(file.repositories, |raw| {
            self.validated(raw)
        }))
    }

    /// Checks one entry; remote registries may only list remote git URLs, file registries
    /// resolve relative local URLs against the file's folder.
    fn validated(&self, raw: &RawEntry) -> std::result::Result<Entry, String> {
        match &self.source {
            Source::File(path) => check_entry(raw, path.parent().unwrap_or(Path::new(""))),
            Source::Url { .. } => {
                if !is_remote_git_url(&raw.url) {
                    check_no_credentials(&raw.url).map_err(|e| e.to_string())?;
                    return Err("a remote registry may only list remote git URLs".to_string());
                }
                check_entry(raw, Path::new(""))
            }
        }
    }
}

impl Registry for JsonRegistry {
    fn location(&self) -> String {
        match &self.source {
            Source::File(path) => path.display().to_string(),
            Source::Url { url, .. } => url.clone(),
        }
    }

    fn listing(&self) -> Result<Listing> {
        self.load()
    }
}

/// A failed cache write must not fail a run that has a good registry document.
fn store_or_warn(dir: &Path, entry: &CacheEntry) {
    if let Err(e) = cache::store(dir, entry) {
        eprintln!("warning: cannot update registry cache: {e}");
    }
}

fn offline_warning(url: &str, reason: &str, age_seconds: u64) -> String {
    let age = cache::describe_age(age_seconds);
    format!("warning: cannot reach registry {url} ({reason}); using cached copy from {age} ago")
}

fn read_file(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => Error::new(format!(
            "registry not found at {} (create it with {{\"repositories\": [{{\"url\": \"<git url>\"}}]}} or list registries under \"registries\" in aip.json)",
            path.display()
        )),
        _ => Error::new(format!("cannot read registry {}: {e}", path.display())),
    })
}

/// The registry URL itself: no credentials, and https (plain http only for local testing).
fn check_registry_url(url: &str) -> Result<()> {
    check_no_credentials(url).context(|| "registry URL".to_string())?;
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(());
    }
    let authority = lower
        .strip_prefix("http://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    if lower.starts_with("http://") && ["localhost", "127.0.0.1", "::1"].contains(&host) {
        return Ok(());
    }
    if lower.starts_with("http://") {
        return Err(Error::new(format!(
            "registry {url} uses plain http; use https"
        )));
    }
    Err(Error::new(format!(
        "registry {url} is not an http(s) URL; use https"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use crate::http::tests::{request_header, response, serve, serve_with, temp_dir};
    use crate::registry::{Registries, RegistryConfig};

    fn context(auth: Auth) -> RegistryContext {
        RegistryContext {
            cache_dir: temp_dir("registry-cache"),
            auth,
        }
    }

    fn open(url: &str, ctx: &RegistryContext) -> Box<dyn Registry> {
        RegistryConfig::Json(JsonSource::Url(url.into())).open(Path::new("."), ctx)
    }

    fn url_registry(url: &str) -> Box<dyn Registry> {
        open(url, &context(Auth::none()))
    }

    fn listing(url: &str) -> String {
        format!(r#"{{"repositories": [{{"name": "demo", "url": "{url}"}}]}}"#)
    }

    fn parse(json: &str) -> std::result::Result<RegistryConfig, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    #[test]
    fn url_registry_resolves_entries() {
        let url = serve("200 OK", listing("https://example.com/demo.git"));
        let registry = url_registry(&url);
        assert_eq!(registry.location(), url);
        let registries = Registries::new(vec![registry]);
        assert_eq!(
            registries.get("demo").unwrap().url,
            "https://example.com/demo.git"
        );
    }

    #[test]
    fn url_registry_reports_http_errors() {
        let err = url_registry(&serve("404 Not Found", ""))
            .repositories()
            .unwrap_err();
        assert!(err.to_string().contains("HTTP 404"), "{err}");
    }

    #[test]
    fn plain_http_only_on_localhost_and_no_credentials() {
        let err = url_registry("http://example.com/r.json")
            .repositories()
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "registry http://example.com/r.json uses plain http; use https"
        );
        let err = url_registry("https://tok:secret@example.com/r.json")
            .repositories()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("credentials") && !err.contains("secret"),
            "{err}"
        );
        assert!(check_registry_url("HTTPS://example.com/r.json").is_ok());
        assert!(check_registry_url("http://[::1]:8080/r.json").is_ok());
        assert!(check_registry_url("http://localhost.evil.com/r.json").is_err());
    }

    #[test]
    fn remote_registry_lists_only_remote_git_urls() {
        for bad in ["../local", "/srv/repo.git", "file:///srv/repo.git"] {
            let url = serve("200 OK", listing(bad));
            let err = url_registry(&url).repositories().unwrap_err().to_string();
            assert_eq!(
                err,
                format!(
                    "repository 'demo' in {url}: a remote registry may only list remote git URLs"
                )
            );
        }
        for ok in [
            "https://h/x.git",
            "git@host:x.git",
            "ssh://git@h/x.git",
            "git://h/x.git",
        ] {
            let url = serve("200 OK", listing(ok));
            assert_eq!(url_registry(&url).repositories().unwrap()[0].url, ok);
        }
    }

    #[test]
    fn source_needs_exactly_one_of_path_or_url() {
        assert_eq!(
            parse(r#"{"type": "json", "url": "https://h/r.json"}"#),
            Ok(RegistryConfig::Json(JsonSource::Url(
                "https://h/r.json".into()
            )))
        );
        let both = parse(r#"{"type": "json", "path": "a", "url": "b"}"#).unwrap_err();
        assert!(
            both.contains(r#"needs either "path" or "url", not both"#),
            "{both}"
        );
        let neither = parse(r#"{"type": "json"}"#).unwrap_err();
        assert!(neither.contains(r#"needs "path" or "url""#), "{neither}");
        let url_in_path = parse(r#"{"type": "json", "path": "HTTPS://h/r.json"}"#).unwrap_err();
        assert!(
            url_in_path.contains(r#""path" is a URL; use "url" instead"#),
            "{url_in_path}"
        );
        let unknown = parse(r#"{"type": "json", "path": "a", "extra": 1}"#).unwrap_err();
        assert!(unknown.contains("unknown field `extra`"), "{unknown}");
    }

    #[test]
    fn source_round_trips() {
        for json in [
            r#"{"type":"json","path":"registry.json"}"#,
            r#"{"type":"json","url":"https://h/r.json"}"#,
        ] {
            let config = parse(json).unwrap();
            assert_eq!(serde_json::to_string(&config).unwrap(), json);
        }
    }

    fn names(registry: &dyn Registry) -> Vec<String> {
        registry
            .repositories()
            .unwrap()
            .into_iter()
            .map(|e| e.url)
            .collect()
    }

    /// A server that answers `200` with validators, and `304` when they come back.
    fn revalidating(body: String) -> crate::http::tests::Server {
        serve_with(move |head| {
            if request_header(head, "if-none-match").as_deref() == Some("\"v1\"") {
                response("304 Not Modified", &[], "")
            } else {
                response(
                    "200 OK",
                    &[
                        ("ETag", "\"v1\""),
                        ("Last-Modified", "Mon, 01 Jan 2024 00:00:00 GMT"),
                    ],
                    &body,
                )
            }
        })
    }

    #[test]
    fn caches_validators_and_revalidates() {
        let server = revalidating(listing("https://example.com/demo.git"));
        let ctx = context(Auth::none());
        let registry = open(&server.url, &ctx);
        assert_eq!(names(&*registry), ["https://example.com/demo.git"]);
        let cached = cache::load(&ctx.cache_dir, &server.url).unwrap();
        assert_eq!(cached.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            cached.last_modified.as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
        assert_eq!(cached.body, listing("https://example.com/demo.git"));
        assert_eq!(
            request_header(&server.last_request(), "if-none-match"),
            None
        );
        // Second run: conditional request, answered by 304, served from the cache.
        assert_eq!(
            names(&*open(&server.url, &ctx)),
            ["https://example.com/demo.git"]
        );
        let head = server.last_request();
        assert_eq!(
            request_header(&head, "if-none-match").as_deref(),
            Some("\"v1\"")
        );
        assert_eq!(
            request_header(&head, "if-modified-since").as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
    }

    #[test]
    fn unreachable_server_falls_back_to_cache() {
        let ctx = context(Auth::none());
        let dead = "http://127.0.0.1:1/r.json";
        assert!(open(dead, &ctx).repositories().is_err());
        let stale = CacheEntry {
            url: dead.into(),
            etag: None,
            last_modified: None,
            fetched_at: cache::now() - 7200,
            body: listing("https://example.com/demo.git"),
        };
        cache::store(&ctx.cache_dir, &stale).unwrap();
        assert_eq!(names(&*open(dead, &ctx)), ["https://example.com/demo.git"]);
        assert_eq!(
            offline_warning(dead, "connection refused", 7200),
            format!(
                "warning: cannot reach registry {dead} (connection refused); using cached copy from 2 hours ago"
            )
        );
    }

    #[test]
    fn server_errors_use_cache_but_client_errors_do_not() {
        let good = revalidating(listing("https://example.com/demo.git"));
        let ctx = context(Auth::none());
        names(&*open(&good.url, &ctx));
        let entry = cache::load(&ctx.cache_dir, &good.url).unwrap();
        for (status, falls_back) in [
            ("503 Service Unavailable", true),
            ("404 Not Found", false),
            ("401 Unauthorized", false),
        ] {
            let server = serve(Box::leak(status.to_string().into_boxed_str()), "");
            let ctx = context(Auth::none());
            cache::store(
                &ctx.cache_dir,
                &CacheEntry {
                    url: server.clone(),
                    ..entry.clone()
                },
            )
            .unwrap();
            let result = open(&server, &ctx).repositories();
            assert_eq!(result.is_ok(), falls_back, "{status}");
        }
    }

    #[test]
    fn file_location_is_normalized() {
        let source = JsonSource::Path("../x/./reg.json".into());
        let reg = JsonRegistry::new(&source, Path::new("/a/b"), &context(Auth::none()));
        assert_eq!(reg.location(), "/a/x/reg.json");
    }

    #[test]
    fn invalid_download_keeps_the_good_cache() {
        let ctx = context(Auth::none());
        let url = serve("200 OK", "not json");
        let good = CacheEntry {
            url: url.clone(),
            etag: Some("\"old\"".into()),
            last_modified: None,
            fetched_at: 1,
            body: listing("https://example.com/demo.git"),
        };
        cache::store(&ctx.cache_dir, &good).unwrap();
        let err = open(&url, &ctx).repositories().unwrap_err().to_string();
        assert!(err.starts_with("invalid JSON"), "{err}");
        assert_eq!(cache::load(&ctx.cache_dir, &url), Some(good));
    }

    #[test]
    fn sends_credentials_only_to_the_matching_host() {
        let server =
            serve_with(|_| response("200 OK", &[], &listing("https://example.com/demo.git")));
        let token = |host: &str| {
            Auth::standard(
                Some(format!(r#"{{"bearer": {{"{host}": "tok"}}}}"#)),
                None,
                &temp_dir("home"),
            )
        };
        names(&*open(&server.url, &context(token(&server.host_key()))));
        assert_eq!(
            request_header(&server.last_request(), "authorization").as_deref(),
            Some("Bearer tok")
        );
        names(&*open(&server.url, &context(token("other.example.com"))));
        assert_eq!(
            request_header(&server.last_request(), "authorization"),
            None
        );
        let basic = Auth::standard(
            Some(format!(
                r#"{{"http-basic": {{"{}": {{"username": "me", "password": "pw"}}}}}}"#,
                server.host_key()
            )),
            None,
            &temp_dir("home"),
        );
        names(&*open(&server.url, &context(basic)));
        assert_eq!(
            request_header(&server.last_request(), "authorization").as_deref(),
            Some("Basic bWU6cHc=")
        );
    }
}
