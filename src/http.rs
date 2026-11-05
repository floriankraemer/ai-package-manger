//! Minimal blocking HTTP client (ureq + rustls) with conditional requests and credentials.

use crate::Error;
use crate::auth::{self, Credential};
use std::fmt;
use std::io::Read;
use std::time::Duration;
use ureq::http::Uri;

/// Overall time limit for one request, including reading the body.
/// ponytail: applies per hop, so a redirect chain can take longer; share a deadline if it matters.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Redirects followed by hand when credentials are attached.
const MAX_REDIRECTS: usize = 5;

/// What to send besides the URL.
#[derive(Debug, Default)]
pub struct Request<'a> {
    pub if_none_match: Option<&'a str>,
    pub if_modified_since: Option<&'a str>,
    /// Sent only to the URL's own origin, never to a redirect target on another origin.
    pub auth: Option<&'a Credential>,
}

/// A successful answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Response {
    Ok {
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    /// `304`: the copy the validators belong to is still current.
    NotModified,
}

/// How a download failed; decides whether a cached copy may stand in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// DNS, connect, TLS, timeout or a broken connection.
    Transport,
    /// HTTP 5xx.
    Server,
    /// HTTP 4xx and other unexpected statuses: URL or credential problems.
    Client,
    /// The answer arrived but is unusable: too large or not UTF-8.
    Invalid,
}

#[derive(Debug)]
pub struct HttpError {
    pub kind: ErrorKind,
    /// Short cause, e.g. `HTTP 503 Service Unavailable`.
    pub reason: String,
    message: String,
}

impl HttpError {
    fn new(kind: ErrorKind, url: &str, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let message = format!("cannot download registry {url}: {reason}");
        Self {
            kind,
            reason,
            message,
        }
    }

    fn too_large(url: &str, max_bytes: u64) -> Self {
        let limit = format!("larger than {} MB", max_bytes / 1_000_000);
        Self {
            kind: ErrorKind::Invalid,
            message: format!("registry {url} is {limit}"),
            reason: limit,
        }
    }

    /// The server could not be reached or failed itself, so an older copy is better than none.
    pub fn allows_stale_copy(&self) -> bool {
        matches!(self.kind, ErrorKind::Transport | ErrorKind::Server)
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<HttpError> for Error {
    fn from(err: HttpError) -> Self {
        Error::new(err.message)
    }
}

fn agent(follow_redirects: bool) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("aip/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false);
    if !follow_redirects {
        builder = builder.max_redirects(0);
    }
    builder.build().into()
}

/// Downloads `url` as text; fails if the body exceeds `max_bytes`.
///
/// Without credentials ureq follows redirects. With credentials aip follows up to
/// [`MAX_REDIRECTS`] by hand and drops the credentials on a redirect to another origin.
pub fn get(
    url: &str,
    request: &Request,
    max_bytes: u64,
) -> std::result::Result<Response, HttpError> {
    let fail = |kind, reason: &str| HttpError::new(kind, url, reason);
    let origin: Uri = url
        .parse()
        .map_err(|_| fail(ErrorKind::Client, "not a valid URL"))?;
    let agent = agent(request.auth.is_none());
    let mut current = origin.clone();
    let mut credential = request.auth;
    for _ in 0..=MAX_REDIRECTS {
        let mut call = agent.get(current.to_string());
        for (name, value) in [
            ("If-None-Match", request.if_none_match),
            ("If-Modified-Since", request.if_modified_since),
        ] {
            if let Some(value) = value {
                call = call.header(name, value);
            }
        }
        if let Some(credential) = credential {
            call = call.header("Authorization", credential.header_value());
        }
        let response = call
            .call()
            .map_err(|e| fail(ErrorKind::Transport, &describe(&e)))?;
        let status = response.status();
        if request.auth.is_some()
            && status.is_redirection()
            && status != 304
            && let Some(location) = header(&response, "location")
        {
            let next = resolve(&current, &location)
                .ok_or_else(|| fail(ErrorKind::Client, "redirect to an invalid URL"))?;
            if !auth::same_origin(&current, &next) {
                credential = None;
                if !auth::is_secure(&next) {
                    return Err(fail(
                        ErrorKind::Client,
                        "redirected to a URL that is not https",
                    ));
                }
            }
            current = next;
            continue;
        }
        return finish(response, &current, credential.is_some(), max_bytes, url);
    }
    Err(fail(ErrorKind::Client, "too many redirects"))
}

fn finish(
    response: ureq::http::Response<ureq::Body>,
    current: &Uri,
    sent_credentials: bool,
    max_bytes: u64,
    url: &str,
) -> std::result::Result<Response, HttpError> {
    let fail = |kind, reason: &str| HttpError::new(kind, url, reason);
    let status = response.status();
    if status == 304 {
        return Ok(Response::NotModified);
    }
    if !status.is_success() {
        let reason = status.canonical_reason().unwrap_or("");
        let mut reason = format!("HTTP {} {reason}", status.as_u16())
            .trim_end()
            .to_string();
        if status == 401 || status == 403 {
            let host = auth::host_key(current);
            reason += &if sent_credentials {
                format!("; credentials for {host} were rejected")
            } else {
                format!(
                    "; add credentials for {host} to auth.json or AIP_AUTH (see \"Private registries\" in docs/user-guide.md)"
                )
            };
        }
        let kind = if status.is_server_error() {
            ErrorKind::Server
        } else {
            ErrorKind::Client
        };
        return Err(fail(kind, &reason));
    }
    let etag = header(&response, "etag");
    let last_modified = header(&response, "last-modified");
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(max_bytes + 1)
        .read_to_end(&mut body)
        .map_err(|e| fail(ErrorKind::Transport, &e.to_string()))?;
    if body.len() as u64 > max_bytes {
        return Err(HttpError::too_large(url, max_bytes));
    }
    let body = String::from_utf8(body)
        .map_err(|_| fail(ErrorKind::Invalid, "response is not valid UTF-8"))?;
    Ok(Response::Ok {
        body,
        etag,
        last_modified,
    })
}

fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    let value = response.headers().get(name)?.to_str().ok()?;
    Some(value.to_string())
}

/// Resolves a `Location` header against the URL that returned it.
fn resolve(base: &Uri, location: &str) -> Option<Uri> {
    if location.contains("://") {
        return location.parse().ok();
    }
    let scheme = base.scheme_str()?;
    let authority = base.authority()?;
    let path = if let Some(rest) = location.strip_prefix("//") {
        return format!("{scheme}://{rest}").parse().ok();
    } else if location.starts_with('/') {
        location.to_string()
    } else {
        let dir = base.path().rsplit_once('/').map_or("", |(dir, _)| dir);
        format!("{dir}/{location}")
    };
    format!("{scheme}://{authority}{path}").parse().ok()
}

/// Plain-language transport errors instead of ureq's `timeout: global` / `io: ...` labels.
fn describe(err: &ureq::Error) -> String {
    match err {
        ureq::Error::Timeout(_) => format!("timed out after {} s", TIMEOUT.as_secs()),
        ureq::Error::Io(e) => e.to_string(),
        ureq::Error::HostNotFound => "host not found".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A fresh, empty temp folder for one test.
    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aip-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Raw HTTP response text.
    pub(crate) fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let headers: String = headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect();
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Value of request header `name` in a recorded request head.
    pub(crate) fn request_header(head: &str, name: &str) -> Option<String> {
        head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    /// Local server whose answers come from a closure and which records request heads.
    pub(crate) struct Server {
        /// `http://127.0.0.1:<port>/r.json`
        pub url: String,
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        pub(crate) fn host_key(&self) -> String {
            self.url
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap()
                .to_string()
        }

        pub(crate) fn last_request(&self) -> String {
            self.requests
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    /// Answers every request with `handler(request head)` (a raw response from [`response`]).
    pub(crate) fn serve_with(handler: impl Fn(&str) -> String + Send + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/r.json", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let (mut head, mut line) = (String::new(), String::new());
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    head += &line;
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                let answer = handler(&head);
                recorded.lock().unwrap().push(head);
                let _ = reader.get_mut().write_all(answer.as_bytes());
            }
        });
        Server { url, requests }
    }

    /// Serves `status` and `body` to every request on 127.0.0.1; returns the URL.
    pub(crate) fn serve(status: &'static str, body: impl Into<String>) -> String {
        let body = body.into();
        serve_with(move |_| response(status, &[], &body)).url
    }

    fn get_text(url: &str, max_bytes: u64) -> crate::Result<String> {
        match get(url, &Request::default(), max_bytes)? {
            Response::Ok { body, .. } => Ok(body),
            Response::NotModified => Err(Error::new("unexpected 304")),
        }
    }

    #[test]
    fn downloads_text() {
        assert_eq!(get_text(&serve("200 OK", "hello"), 100).unwrap(), "hello");
    }

    #[test]
    fn reports_http_status() {
        let url = serve("404 Not Found", "nope");
        let err = get_text(&url, 100).unwrap_err().to_string();
        assert_eq!(
            err,
            format!("cannot download registry {url}: HTTP 404 Not Found")
        );
    }

    #[test]
    fn rejects_oversized_body() {
        let url = serve("200 OK", "0123456789");
        let err = get(&url, &Request::default(), 5).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Invalid);
        assert!(err.to_string().contains("is larger than"), "{err}");
    }

    #[test]
    fn reports_transport_error() {
        let err = get("http://127.0.0.1:1/r.json", &Request::default(), 5).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Transport);
        assert!(
            err.to_string()
                .starts_with("cannot download registry http://127.0.0.1:1/r.json: "),
            "{err}"
        );
    }

    #[test]
    fn classifies_statuses() {
        let kind =
            |status: &'static str| get(&serve(status, ""), &Request::default(), 5).unwrap_err();
        assert!(kind("503 Service Unavailable").allows_stale_copy());
        for status in ["404 Not Found", "401 Unauthorized", "400 Bad Request"] {
            assert!(!kind(status).allows_stale_copy(), "{status}");
        }
    }

    #[test]
    fn returns_validators_and_sends_conditions() {
        let server = serve_with(|head| {
            if request_header(head, "if-none-match").as_deref() == Some("\"v1\"") {
                response("304 Not Modified", &[], "")
            } else {
                response(
                    "200 OK",
                    &[
                        ("ETag", "\"v1\""),
                        ("Last-Modified", "Mon, 01 Jan 2024 00:00:00 GMT"),
                    ],
                    "{}",
                )
            }
        });
        let first = get(&server.url, &Request::default(), 100).unwrap();
        assert_eq!(
            first,
            Response::Ok {
                body: "{}".into(),
                etag: Some("\"v1\"".into()),
                last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            }
        );
        let request = Request {
            if_none_match: Some("\"v1\""),
            if_modified_since: Some("Mon, 01 Jan 2024 00:00:00 GMT"),
            auth: None,
        };
        assert_eq!(
            get(&server.url, &request, 100).unwrap(),
            Response::NotModified
        );
        let head = server.last_request();
        assert_eq!(
            request_header(&head, "if-modified-since").unwrap(),
            "Mon, 01 Jan 2024 00:00:00 GMT"
        );
    }

    fn bearer() -> Credential {
        Credential::Bearer("tok".into())
    }

    #[test]
    fn sends_authorization_and_explains_rejection() {
        let server = serve_with(|head| match request_header(head, "authorization") {
            Some(v) if v == "Bearer tok" => response("200 OK", &[], "ok"),
            _ => response("401 Unauthorized", &[], ""),
        });
        let credential = bearer();
        let request = Request {
            auth: Some(&credential),
            ..Request::default()
        };
        assert!(get(&server.url, &request, 100).is_ok());
        let wrong = Credential::Bearer("bad".into());
        let request = Request {
            auth: Some(&wrong),
            ..Request::default()
        };
        let err = get(&server.url, &request, 100).unwrap_err().to_string();
        assert!(
            err.ends_with(&format!(
                "HTTP 401 Unauthorized; credentials for {} were rejected",
                server.host_key()
            )),
            "{err}"
        );
        assert!(!err.contains("bad"), "{err}");
        let err = get(&server.url, &Request::default(), 100)
            .unwrap_err()
            .to_string();
        assert!(
            err.ends_with(&format!("HTTP 401 Unauthorized; add credentials for {} to auth.json or AIP_AUTH (see \"Private registries\" in docs/user-guide.md)", server.host_key())),
            "{err}"
        );
    }

    #[test]
    fn redirects_keep_credentials_on_same_origin_only() {
        let other = serve_with(|_| response("200 OK", &[], "elsewhere"));
        let other_url = other.url.clone();
        let server = serve_with(move |head| {
            if head.starts_with("GET /same") {
                response("200 OK", &[], "same")
            } else if head.starts_with("GET /to-same") {
                response("302 Found", &[("Location", "/same")], "")
            } else {
                response("302 Found", &[("Location", &other_url)], "")
            }
        });
        let credential = bearer();
        let request = Request {
            auth: Some(&credential),
            ..Request::default()
        };
        let same = server.url.replace("r.json", "to-same");
        assert!(get(&same, &request, 100).is_ok());
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| request_header(r, "authorization").is_some())
        );
        let body = get(&server.url, &request, 100).unwrap();
        assert!(matches!(body, Response::Ok { ref body, .. } if body == "elsewhere"));
        assert_eq!(request_header(&other.last_request(), "authorization"), None);
    }

    #[test]
    fn stops_after_too_many_redirects() {
        let server = serve_with(|_| response("302 Found", &[("Location", "/r.json")], ""));
        let credential = bearer();
        let request = Request {
            auth: Some(&credential),
            ..Request::default()
        };
        let err = get(&server.url, &request, 100).unwrap_err().to_string();
        assert!(err.ends_with("too many redirects"), "{err}");
        assert_eq!(server.requests.lock().unwrap().len(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn resolves_locations() {
        let base: Uri = "https://h.com/a/b.json".parse().unwrap();
        for (location, expected) in [
            ("/x", "https://h.com/x"),
            ("c.json", "https://h.com/a/c.json"),
            ("//o.com/y", "https://o.com/y"),
            ("http://o.com/z", "http://o.com/z"),
        ] {
            assert_eq!(resolve(&base, location).unwrap().to_string(), expected);
        }
    }
}
