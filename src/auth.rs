//! Credentials for private HTTP registries, read from `auth.json` files and `AIP_AUTH`.
//!
//! Git repositories do not use this: git authenticates them itself (ADR 0007).
//!
//! ```json
//! {
//!   "bearer": { "skills.example.com": "TOKEN" },
//!   "http-basic": { "git.example.com:8443": { "username": "me", "password": "secret" } }
//! }
//! ```

use crate::{Error, Result};
use serde::Deserialize;
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use ureq::http::Uri;

/// Hosts that may be reached over plain http (local testing).
pub(crate) const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// One secret for one host. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Bearer(String),
    Basic { username: String, password: String },
}

impl Credential {
    /// Value of the `Authorization` header.
    pub fn header_value(&self) -> String {
        match self {
            Self::Bearer(token) => format!("Bearer {token}"),
            Self::Basic { username, password } => {
                format!(
                    "Basic {}",
                    base64(format!("{username}:{password}").as_bytes())
                )
            }
        }
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Credential::Bearer(<redacted>)"),
            Self::Basic { .. } => f.write_str("Credential::Basic(<redacted>)"),
        }
    }
}

/// Where credentials come from; earlier entries in [`Auth`] win.
#[derive(Debug, Clone)]
enum AuthSource {
    /// Text of `AIP_AUTH`.
    Env(String),
    File(PathBuf),
}

impl fmt::Display for AuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(_) => f.write_str("AIP_AUTH"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

type Credentials = BTreeMap<String, Credential>;

/// Credentials for HTTP registries, loaded on first use (once per run, so the permission
/// warning appears once). Clones share the loaded state.
#[derive(Clone)]
pub struct Auth {
    sources: Rc<Vec<AuthSource>>,
    loaded: Rc<OnceCell<std::result::Result<Credentials, String>>>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Auth(<redacted>)")
    }
}

impl Auth {
    fn new(sources: Vec<AuthSource>) -> Self {
        Self {
            sources: Rc::new(sources),
            loaded: Rc::new(OnceCell::new()),
        }
    }

    /// No credentials at all.
    pub fn none() -> Self {
        Self::new(Vec::new())
    }

    /// Sources by precedence: `env` (the text of `AIP_AUTH`), then the project's `auth.json`
    /// (`None` for global scope), then the one in aip's home.
    pub fn standard(env: Option<String>, project: Option<&Path>, aip_home: &Path) -> Self {
        let mut sources = Vec::new();
        sources.extend(env.filter(|e| !e.trim().is_empty()).map(AuthSource::Env));
        sources.extend(project.map(|dir| AuthSource::File(dir.join("auth.json"))));
        sources.push(AuthSource::File(aip_home.join("auth.json")));
        Self::new(sources)
    }

    /// The credential to send to `url`, if one is configured for its host[:port] and the
    /// connection is secure enough (https, or http to localhost).
    pub fn credential_for(&self, url: &str) -> Result<Option<Credential>> {
        let credentials = self
            .loaded
            .get_or_init(|| load(&self.sources))
            .as_ref()
            .map_err(|e| Error::new(e.clone()))?;
        Ok(url
            .parse::<Uri>()
            .ok()
            .filter(is_secure)
            .and_then(|uri| credentials.get(&host_key(&uri)).cloned()))
    }
}

/// https, or http to a loopback host.
pub(crate) fn is_secure(uri: &Uri) -> bool {
    match uri.scheme_str() {
        Some("https") => true,
        Some("http") => uri
            .host()
            .is_some_and(|h| LOOPBACK_HOSTS.contains(&h.trim_matches(['[', ']']))),
        _ => false,
    }
}

fn default_port(uri: &Uri) -> u16 {
    if uri.scheme_str() == Some("http") {
        80
    } else {
        443
    }
}

/// `host` or `host:port` (port only when not the default), lowercase: the key in auth.json.
pub fn host_key(uri: &Uri) -> String {
    let host = uri.host().unwrap_or_default().to_ascii_lowercase();
    match uri.port_u16() {
        Some(port) if port != default_port(uri) => format!("{host}:{port}"),
        _ => host,
    }
}

/// Same scheme, host and effective port.
pub(crate) fn same_origin(a: &Uri, b: &Uri) -> bool {
    a.scheme_str() == b.scheme_str()
        && a.host().map(str::to_ascii_lowercase) == b.host().map(str::to_ascii_lowercase)
        && a.port_u16().unwrap_or_else(|| default_port(a))
            == b.port_u16().unwrap_or_else(|| default_port(b))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthFile {
    #[serde(default)]
    bearer: BTreeMap<String, String>,
    #[serde(default, rename = "http-basic")]
    http_basic: BTreeMap<String, BasicEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BasicEntry {
    username: String,
    password: String,
}

/// Merges all sources; the first source naming a host wins.
fn load(sources: &[AuthSource]) -> std::result::Result<Credentials, String> {
    let mut merged = Credentials::new();
    for source in sources {
        let Some(text) = read(source)? else { continue };
        for (host, credential) in parse(&text).map_err(|e| format!("invalid {source}: {e}"))? {
            merged.entry(host).or_insert(credential);
        }
    }
    Ok(merged)
}

fn read(source: &AuthSource) -> std::result::Result<Option<String>, String> {
    match source {
        AuthSource::Env(text) => Ok(Some(text.clone())),
        AuthSource::File(path) => match fs::read_to_string(path) {
            Ok(text) => {
                if let Some(warning) = permission_warning(path) {
                    eprintln!("{warning}");
                }
                Ok(Some(text))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        },
    }
}

/// Parses one auth document. Errors never contain document text, because it holds secrets.
fn parse(text: &str) -> std::result::Result<Credentials, String> {
    let file: AuthFile = serde_json::from_str(text).map_err(|e| {
        format!(
            "{:?} at line {} column {}; expected {{\"bearer\": {{\"<host>\": \"<token>\"}}, \"http-basic\": {{\"<host>\": {{\"username\": \"...\", \"password\": \"...\"}}}}}}",
            e.classify(),
            e.line(),
            e.column()
        )
    })?;
    let mut credentials = Credentials::new();
    let bearer = file
        .bearer
        .into_iter()
        .map(|(h, t)| (h, Credential::Bearer(t)));
    let basic = file.http_basic.into_iter().map(|(h, b)| {
        let credential = Credential::Basic {
            username: b.username,
            password: b.password,
        };
        (h, credential)
    });
    for (host, credential) in bearer.chain(basic) {
        if host.is_empty() || host.contains(['/', '@', ' ']) {
            return Err(format!("key '{host}' must be a host or host:port"));
        }
        if credentials
            .insert(host.to_ascii_lowercase(), credential)
            .is_some()
        {
            return Err(format!("host '{host}' is listed more than once"));
        }
    }
    Ok(credentials)
}

/// Warns when other users can read an auth file (Unix only).
fn permission_warning(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).ok()?.permissions().mode();
        if mode & 0o077 != 0 {
            let p = path.display();
            return Some(format!(
                "warning: {p} is readable by other users; run chmod 600 {p}"
            ));
        }
    }
    let _ = path;
    None
}

/// Standard base64 (RFC 4648) with padding, for Basic authentication.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(env: Option<&str>, project: Option<&str>, global: Option<&str>) -> Auth {
        let dir = crate::http::tests::temp_dir("auth");
        let write = |name: &str, text: Option<&str>| {
            let path = dir.join(name);
            fs::create_dir_all(&path).unwrap();
            if let Some(text) = text {
                fs::write(path.join("auth.json"), text).unwrap();
            }
            path
        };
        let (project, global) = (write("project", project), write("home", global));
        Auth::standard(env.map(String::from), Some(&project), &global)
    }

    fn header(auth: &Auth, url: &str) -> Option<String> {
        auth.credential_for(url).unwrap().map(|c| c.header_value())
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn builds_bearer_and_basic_headers() {
        let a = auth(
            Some(
                r#"{"bearer": {"Skills.Example.com": "tok"}, "http-basic": {"git.example.com:8443": {"username": "me", "password": "pw"}}}"#,
            ),
            None,
            None,
        );
        assert_eq!(
            header(&a, "https://skills.example.com/r.json").unwrap(),
            "Bearer tok"
        );
        assert_eq!(
            header(&a, "https://git.example.com:8443/r.json").unwrap(),
            "Basic bWU6cHc="
        );
        // Port must match exactly; other hosts get nothing.
        assert_eq!(header(&a, "https://git.example.com/r.json"), None);
        assert_eq!(header(&a, "https://other.example.com/r.json"), None);
        assert!(header(&a, "https://skills.example.com:443/r.json").is_some());
    }

    #[test]
    fn never_sends_over_insecure_http() {
        let a = auth(
            Some(r#"{"bearer": {"example.com": "tok", "localhost": "t"}}"#),
            None,
            None,
        );
        assert_eq!(header(&a, "http://example.com/r.json"), None);
        assert!(header(&a, "http://localhost/r.json").is_some());
    }

    #[test]
    fn env_beats_project_beats_global() {
        let doc = |t: &str| format!(r#"{{"bearer": {{"example.com": "{t}"}}}}"#);
        let a = auth(
            Some(&doc("env")),
            Some(&doc("project")),
            Some(&doc("global")),
        );
        assert_eq!(header(&a, "https://example.com/").unwrap(), "Bearer env");
        let a = auth(None, Some(&doc("project")), Some(&doc("global")));
        assert_eq!(
            header(&a, "https://example.com/").unwrap(),
            "Bearer project"
        );
        let a = auth(None, None, Some(&doc("global")));
        assert_eq!(header(&a, "https://example.com/").unwrap(), "Bearer global");
        // Hosts merge across sources.
        let a = auth(
            Some(&doc("env")),
            Some(r#"{"bearer": {"b.example.com": "p"}}"#),
            None,
        );
        assert_eq!(header(&a, "https://b.example.com/").unwrap(), "Bearer p");
    }

    #[test]
    fn invalid_documents_name_the_source_and_hide_secrets() {
        let a = auth(Some(r#"{"http-basic": {"h": "hunter2"}}"#), None, None);
        let err = a.credential_for("https://h/").unwrap_err().to_string();
        assert!(err.starts_with("invalid AIP_AUTH: "), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        let a = auth(None, Some(r#"{"bearer": {"h": "tok"}, "extra": 1}"#), None);
        let err = a.credential_for("https://h/").unwrap_err().to_string();
        assert!(
            err.contains("auth.json: ") && err.starts_with("invalid "),
            "{err}"
        );
        let a = auth(Some(r#"{"bearer": {"https://h/x": "tok"}}"#), None, None);
        assert!(a.credential_for("https://h/").is_err());
    }

    #[test]
    fn debug_output_is_redacted() {
        let basic = Credential::Basic {
            username: "me".into(),
            password: "hunter2".into(),
        };
        let shown = format!("{:?} {:?}", basic, Credential::Bearer("tok123".into()));
        assert!(!shown.contains("hunter2") && !shown.contains("tok123") && !shown.contains("me"));
        let a = auth(Some(r#"{"bearer": {"h": "tok123"}}"#), None, None);
        assert!(!format!("{a:?}").contains("tok123"));
    }

    #[cfg(unix)]
    #[test]
    fn warns_about_files_readable_by_others() {
        use std::os::unix::fs::PermissionsExt;
        let path = crate::http::tests::temp_dir("perm").join("auth.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let p = path.display();
        assert_eq!(
            permission_warning(&path).unwrap(),
            format!("warning: {p} is readable by other users; run chmod 600 {p}")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(permission_warning(&path), None);
    }

    #[test]
    fn origin_comparison_uses_effective_port() {
        let u = |s: &str| s.parse::<Uri>().unwrap();
        assert!(same_origin(
            &u("https://H.com/a"),
            &u("https://h.com:443/b")
        ));
        assert!(!same_origin(&u("https://h.com/"), &u("http://h.com/")));
        assert!(!same_origin(
            &u("https://h.com/"),
            &u("https://h.com:8443/")
        ));
        assert_eq!(host_key(&u("https://H.com:443/")), "h.com");
        assert_eq!(host_key(&u("https://h.com:8443/")), "h.com:8443");
    }
}
