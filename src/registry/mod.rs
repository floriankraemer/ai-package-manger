//! Registries: ordered sources of skill repositories.
//!
//! Every repository keeps its skills at `skills/<skill>/SKILL.md`; package names are
//! `<repository name>/<skill>`.

mod cache;
mod git;
mod json;

pub use git::GitRepositoryRegistry;
pub use json::{JsonRegistry, JsonSource};

use crate::auth::Auth;
use crate::{Context, Error, Result};
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::path::{self, Component, Path, PathBuf};

/// A repository listed in a registry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub url: String,
}

/// A repository as written in a registry: `name` is optional and derived from the URL when absent.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEntry {
    #[serde(default)]
    pub name: Option<String>,
    pub url: String,
}

impl RawEntry {
    /// Names the entry in error messages without echoing the URL.
    pub(crate) fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("repository '{name}'"),
            None => "repository".to_string(),
        }
    }
}

/// A source of skill repositories (JSON file or URL now; HTTP server later).
pub trait Registry {
    /// Path or URL, for messages.
    fn location(&self) -> String;
    /// Repositories this registry lists, with git URLs ready for `git clone`.
    fn repositories(&self) -> Result<Vec<Entry>>;
}

/// What registries need besides their own configuration.
#[derive(Debug, Clone)]
pub struct RegistryContext {
    /// Folder for downloaded registry documents (`<aip home>/cache/registries`).
    pub cache_dir: PathBuf,
    /// Credentials for HTTP registries.
    pub auth: Auth,
}

/// Registry configuration as written in aip.json.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum RegistryConfig {
    /// Registry document from a file (`path`) or a URL (`url`).
    Json(JsonSource),
    /// One repository declared inline.
    Git {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        url: String,
    },
}

impl RegistryConfig {
    /// Opens the registry; a relative path is resolved against `base` (the aip.json folder).
    pub fn open(&self, base: &Path, ctx: &RegistryContext) -> Box<dyn Registry> {
        match self {
            Self::Json(source) => Box::new(JsonRegistry::new(source, base, ctx)),
            Self::Git { name, url } => Box::new(GitRepositoryRegistry {
                name: name.clone(),
                url: url.clone(),
                base: base.to_path_buf(),
            }),
        }
    }
}

/// Ordered set of registries; the first registry listing a repository name wins
/// (like Composer repository priority). Registries are read on first use.
pub struct Registries {
    registries: Vec<Box<dyn Registry>>,
    entries: OnceCell<Vec<Entry>>,
}

impl Registries {
    pub fn new(registries: Vec<Box<dyn Registry>>) -> Self {
        Self {
            registries,
            entries: OnceCell::new(),
        }
    }

    /// The repository called `name`, from the first registry that lists it.
    pub fn get(&self, name: &str) -> Result<Entry> {
        let entries = self.entries()?;
        if let Some(entry) = entries.iter().find(|e| e.name == name) {
            return Ok(entry.clone());
        }
        let locations: Vec<_> = self.registries.iter().map(|r| r.location()).collect();
        let similar: Vec<String> = entries
            .iter()
            .filter(|e| {
                e.name.ends_with(&format!("/{name}")) || e.name.starts_with(&format!("{name}/"))
            })
            .take(3)
            .map(|e| format!("'{}'", e.name))
            .collect();
        let hint = if similar.is_empty() {
            String::new()
        } else {
            format!("; did you mean {}?", similar.join(", "))
        };
        Err(Error::new(format!(
            "unknown repository '{name}' (not listed in any registry: {}){hint}",
            locations.join(", ")
        )))
    }

    /// All repositories, first registry wins on duplicate names.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        if self.entries.get().is_none() {
            let mut merged: Vec<Entry> = Vec::new();
            for registry in &self.registries {
                for entry in registry.repositories()? {
                    if !merged.iter().any(|e| e.name == entry.name) {
                        merged.push(entry);
                    }
                }
            }
            let _ = self.entries.set(merged);
        }
        Ok(self.entries.get().expect("entries loaded above").clone())
    }
}

/// Repository and skill names become directory names, so they are restricted to a safe set.
///
/// Names Windows cannot create (`con`, `nul.txt`, trailing `.`) are refused on every platform, so
/// a lock built on Linux also works on Windows.
pub fn valid_name(name: &str) -> bool {
    !name.ends_with('.')
        && !is_reserved_device_name(name)
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// Windows device names (`CON`, `nul.txt`, `COM1`, ...), which cannot be files there, with or
/// without extension, in any case.
pub(crate) fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| ('1'..='9').contains(&c)))
}

/// Repository names are `/`-separated paths such as `owner/repo`; each segment is a [`valid_name`].
pub fn valid_repository_name(name: &str) -> bool {
    name.split('/').all(valid_name)
}

/// Derives `<owner>/<repo>` (the full lowercase URL path) from a git URL or local path.
///
/// Local paths yield their final component; Azure DevOps `_git` and `v3` segments are dropped.
pub fn derive_name(url: &str) -> Result<String> {
    let (host, path) = if is_local(url) {
        ("", url.rsplit(['/', '\\']).next().unwrap_or(""))
    } else {
        let (host_part, path) = match url.split_once("://") {
            Some((_, rest)) => rest.split_once('/').unwrap_or((rest, "")),
            None => url.split_once(':').unwrap_or((url, "")),
        };
        let host = host_part.rsplit('@').next().unwrap_or("");
        (host, path.split(['?', '#']).next().unwrap_or(""))
    };
    let azure = host.to_ascii_lowercase().ends_with("dev.azure.com");
    let lower = path.to_ascii_lowercase();
    let mut segments: Vec<&str> = lower
        .split('/')
        .filter(|s| !s.is_empty() && *s != "_git")
        .collect();
    if azure && segments.first() == Some(&"v3") {
        segments.remove(0);
    }
    if let Some(last) = segments.last_mut() {
        *last = last.strip_suffix(".git").unwrap_or(last);
    }
    if segments.is_empty() || !segments.iter().all(|s| valid_name(s)) {
        return Err(Error::new(format!(
            "cannot derive a repository name from {url}; set \"name\" (lowercase letters, digits, '.', '_', '-', segments separated by '/')"
        )));
    }
    Ok(segments.join("/"))
}

/// Checks a registry entry (name, no credentials in the URL), resolves a relative local URL
/// against `base` and derives a missing name from the URL. `location` names the registry in
/// error messages.
pub(crate) fn validated_entry(raw: RawEntry, base: &Path, location: &str) -> Result<Entry> {
    if let Some(name) = raw.name.as_deref().filter(|n| !valid_repository_name(n)) {
        return Err(Error::new(format!(
            "invalid repository name '{name}' in registry {location} (use lowercase letters, digits, '.', '_', '-', segments separated by '/')"
        )));
    }
    // The URL reaches `git config` as a value; a leading '-' must never look like an option.
    if raw.url.starts_with('-') {
        return Err(Error::new("URL must not start with '-'"))
            .context(|| format!("{} in {location}", raw.label()));
    }
    check_no_credentials(&raw.url).context(|| format!("{} in {location}", raw.label()))?;
    let url = resolve_url(&raw.url, base)?;
    let name = match raw.name {
        Some(name) => name,
        None => derive_name(&url).context(|| format!("repository in {location}"))?,
    };
    Ok(Entry { name, url })
}

/// Makes a relative local URL absolute against `base`; remote URLs are returned unchanged.
pub(crate) fn resolve_url(url: &str, base: &Path) -> Result<String> {
    if !is_local(url) || !Path::new(url).is_relative() {
        return Ok(url.to_string());
    }
    let local = path::absolute(base.join(url)).context(|| format!("cannot resolve {url}"))?;
    Ok(normalize(&local).to_string_lossy().into_owned())
}

/// The URL as aip.lock stores it: an absolute local path becomes relative to `base` (an absolute,
/// normalized folder) with `/` separators when a relative path exists, so teammates can use the
/// lock from another location. Everything else is kept.
pub(crate) fn portable_url(url: &str, base: &Path) -> String {
    if is_local(url) && Path::new(url).is_absolute() {
        relative_path(base, Path::new(url)).unwrap_or_else(|| url.to_string())
    } else {
        url.to_string()
    }
}

/// Path from folder `from_dir` to `to`, written with `/` separators (`.` if equal). `None` when
/// there is none, e.g. different Windows drives or a relative path against an absolute one.
pub(crate) fn relative_path(from_dir: &Path, to: &Path) -> Option<String> {
    let (from, to) = (normalize(from_dir), normalize(to));
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None;
    }
    let parts: Vec<String> = std::iter::repeat_n("..".to_string(), from.len() - common)
        .chain(
            to[common..]
                .iter()
                .map(|c| c.as_os_str().to_string_lossy().into_owned()),
        )
        .collect();
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Rejects credentials in a URL: aip copies URLs into aip.json and aip.lock, which get committed.
///
/// Refuses `scheme://user:secret@host` and, for http(s), `scheme://token@host`; `ssh://git@host`
/// and `git@host:path` carry no secret. The URL is never echoed.
pub(crate) fn check_no_credentials(url: &str) -> Result<()> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Ok(());
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let Some((userinfo, _host)) = authority.rsplit_once('@') else {
        return Ok(());
    };
    let http = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    if userinfo.contains(':') || http {
        return Err(Error::new(
            "URL contains credentials; remove them and let git authenticate (SSH key, credential helper, or url.<base>.insteadOf) — see \"Private repositories\" in docs/user-guide.md",
        ));
    }
    Ok(())
}

/// Splits `<repository>/<skill>` and validates both parts.
pub fn split_package(name: &str) -> Result<(&str, &str)> {
    name.rsplit_once('/')
        .filter(|(repo, skill)| valid_repository_name(repo) && valid_name(skill))
        .ok_or_else(|| {
            Error::new(format!(
                "invalid package name '{name}' (expected <repository>/<skill>, e.g. owner/repo/skill, using lowercase letters, digits, '.', '_', '-')"
            ))
        })
}

/// `description` from the YAML frontmatter of a SKILL.md.
// ponytail: line-based frontmatter reader, single-line values only; use a YAML parser if skills need more.
pub fn description(skill_md: &str) -> Option<String> {
    let mut lines = skill_md.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.strip_prefix("description:"))
        // Control characters would let a SKILL.md drive the terminal (ANSI/OSC) or corrupt aip.lock.
        .map(|v| {
            v.trim()
                .trim_matches(['"', '\''])
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|v| !v.is_empty())
}

/// Lexically resolves `.` and `..` (no symlink resolution, no Windows `\\?\` prefixes).
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Remote git URL: `https://`, `ssh://`, `git://` or scp-like `user@host:path`.
pub(crate) fn is_remote_git_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["https://", "ssh://", "git://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        || (!url.contains("://") && !is_local(url))
}

/// Anything that is not a URL (`scheme://`) or scp-like address (`user@host:path`) is a local path.
pub(crate) fn is_local(url: &str) -> bool {
    !(url.contains("://")
        || url
            .split_once(':')
            .is_some_and(|(host, _)| host.contains('@')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("anthropic"));
        assert!(valid_name("my-skills.v2_x"));
        for bad in ["", ".", "..", "-x", "Upper", "a/b", "a b", ".hidden"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(split_package("a/pdf").unwrap(), ("a", "pdf"));
        assert!(split_package("pdf").is_err());
        assert!(split_package("a/../x").is_err());
        assert_eq!(split_package("a/b/c").unwrap(), ("a/b", "c"));
        for bad in ["a//c", "/c", "a/", "a/../c"] {
            assert!(split_package(bad).is_err(), "{bad}");
        }
        assert!(valid_repository_name("group/sub/repo"));
        assert!(!valid_repository_name("a//b") && !valid_repository_name(""));
    }

    #[test]
    fn frontmatter_description() {
        let md = "---\nname: pdf\ndescription: \"Work with PDFs\"\n---\n# PDF\ndescription: no";
        assert_eq!(description(md).as_deref(), Some("Work with PDFs"));
        assert_eq!(description("# no frontmatter"), None);
    }

    #[test]
    fn description_strips_control_characters() {
        let md = "---\ndescription: \x1b]52;c;xx\x07evil\r\n---\n";
        assert_eq!(description(md).as_deref(), Some("]52;c;xx evil"));
    }

    #[test]
    fn windows_unsafe_names_are_invalid() {
        for bad in ["con", "nul.txt", "aux.tar.gz", "com1", "lpt9.x", "name."] {
            assert!(!valid_name(bad), "{bad}");
        }
        for ok in ["console", "com0", "com10", "lpt", "a.b", "nul-x"] {
            assert!(valid_name(ok), "{ok}");
        }
        assert!(is_reserved_device_name("CoN") && is_reserved_device_name("Nul.TXT"));
    }

    #[test]
    fn local_urls() {
        assert!(is_local("../repo"));
        assert!(is_local("/srv/repo.git"));
        assert!(is_local(r"C:\repos\x"));
        assert!(!is_local("https://github.com/a/b.git"));
        assert!(!is_local("git@github.com:a/b.git"));
        assert_eq!(normalize(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
    }

    use std::fs;

    #[test]
    fn relative_paths() {
        let rel = |from: &str, to: &str| relative_path(Path::new(from), Path::new(to));
        assert_eq!(rel("/a/proj", "/a/repo").as_deref(), Some("../repo"));
        assert_eq!(rel("/a/proj", "/a/proj/sub/x").as_deref(), Some("sub/x"));
        assert_eq!(rel("/a/b/proj", "/a/repo").as_deref(), Some("../../repo"));
        assert_eq!(rel("/a/proj", "/a/proj").as_deref(), Some("."));
        assert_eq!(rel("/a/p/../q", "/a/./r").as_deref(), Some("../r"));
        assert_eq!(rel("/a", "b"), None);
        #[cfg(windows)]
        {
            assert_eq!(rel(r"C:\a\proj", r"C:\a\repo").as_deref(), Some("../repo"));
            assert_eq!(rel(r"C:\a", r"D:\a"), None);
        }
    }

    #[test]
    fn portable_urls() {
        let base = Path::new("/a/proj");
        assert_eq!(portable_url("/a/my-skills", base), "../my-skills");
        assert_eq!(portable_url("https://h/a/b.git", base), "https://h/a/b.git");
        assert_eq!(portable_url("git@h:a/b.git", base), "git@h:a/b.git");
        assert_eq!(portable_url("../already", base), "../already");
    }

    #[test]
    fn urls_starting_with_dash_are_rejected_without_echo() {
        let raw = RawEntry {
            name: Some("x".into()),
            url: "--upload-pack=evil".into(),
        };
        let err = validated_entry(raw, Path::new("."), "reg.json")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "repository 'x' in reg.json: URL must not start with '-'"
        );
    }

    fn no_context() -> RegistryContext {
        RegistryContext {
            cache_dir: PathBuf::from("unused"),
            auth: Auth::none(),
        }
    }

    fn json_registry(dir: &Path, file: &str, body: &str) -> Box<dyn Registry> {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(file), body).unwrap();
        RegistryConfig::Json(JsonSource::Path(file.into())).open(dir, &no_context())
    }

    #[test]
    fn first_registry_wins_and_later_ones_add() {
        let dir = std::env::temp_dir().join(format!("aip-registries-{}", std::process::id()));
        let a = json_registry(
            &dir,
            "a.json",
            r#"{"repositories": [{"name": "demo", "url": "https://a/demo.git"}]}"#,
        );
        let b = json_registry(
            &dir,
            "b.json",
            r#"{"repositories": [{"name": "demo", "url": "https://b/demo.git"}, {"name": "extra", "url": "../extra"}]}"#,
        );
        let registries = Registries::new(vec![a, b]);
        assert_eq!(registries.get("demo").unwrap().url, "https://a/demo.git");
        assert!(registries.get("extra").unwrap().url.ends_with("extra"));
        assert_eq!(registries.entries().unwrap().len(), 2);
        let err = registries.get("nope").unwrap_err().to_string();
        assert!(
            err.contains("unknown repository 'nope'") && err.contains("b.json"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_parses_json_and_rejects_unknown_types() {
        let ok: Vec<RegistryConfig> =
            serde_json::from_str(r#"[{"type": "json", "path": "registry.json"}]"#).unwrap();
        assert_eq!(
            ok,
            [RegistryConfig::Json(JsonSource::Path(
                "registry.json".into()
            ))]
        );
        let err = serde_json::from_str::<RegistryConfig>(r#"{"type": "ftp", "path": "x"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant `ftp`"), "{err}");
    }

    #[test]
    fn credentials_in_urls() {
        for bad in [
            "https://user:pass@github.com/a/b.git",
            "https://token@github.com/a/b.git",
            "http://token@host/a/b.git",
            "ssh://git:secret@host/a/b.git",
        ] {
            let err = check_no_credentials(bad).unwrap_err().to_string();
            assert!(err.contains("credentials"), "{err}");
            assert!(!err.contains("pass") && !err.contains("token@"), "{err}");
        }
        for ok in [
            "ssh://git@github.com/a/b.git",
            "git@github.com:a/b.git",
            "https://github.com/a/b.git",
            "http://host:8080/a/b.git",
            "https://github.com/a/b@v1.git",
            "../local/repo",
            "/srv/repo.git",
        ] {
            assert!(check_no_credentials(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn inline_git_registry() {
        let dir = std::env::temp_dir().join(format!("aip-inline-git-{}", std::process::id()));
        let json = json_registry(
            &dir,
            "r.json",
            r#"{"repositories": [{"name": "demo", "url": "https://pub/demo.git"}, {"name": "other", "url": "https://pub/other.git"}]}"#,
        );
        let inline = |name: &str, url: &str| {
            RegistryConfig::Git {
                name: Some(name.into()),
                url: url.into(),
            }
            .open(&dir, &no_context())
        };
        let registries = Registries::new(vec![
            inline("demo", "git@github.com:acme/private.git"),
            inline("local", "../private"),
            json,
        ]);
        assert_eq!(
            registries.get("demo").unwrap().url,
            "git@github.com:acme/private.git"
        );
        assert_eq!(
            registries.get("other").unwrap().url,
            "https://pub/other.git"
        );
        let local = registries.get("local").unwrap().url;
        assert!(
            local.ends_with("private") && !local.contains(".."),
            "{local}"
        );
        let manifest = dir.join("aip.json").display().to_string();
        assert_eq!(inline("demo", "x").location(), manifest);

        let err = inline("Bad Name", "x")
            .repositories()
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid repository name 'Bad Name'"), "{err}");
        let err = inline("acme", "https://u:secret@h/x.git")
            .repositories()
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(&format!(
                "repository 'acme' in {manifest}: URL contains credentials"
            )) && !err.contains("secret"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_registry_rejects_credentials() {
        let dir = std::env::temp_dir().join(format!("aip-json-creds-{}", std::process::id()));
        let r = json_registry(
            &dir,
            "c.json",
            r#"{"repositories": [{"name": "x", "url": "https://tok@h/x.git"}]}"#,
        );
        let err = r.repositories().unwrap_err().to_string();
        assert!(
            err.contains("repository 'x' in") && err.contains("credentials"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_git_config_parses_strictly() {
        let ok: RegistryConfig =
            serde_json::from_str(r#"{"type": "git", "name": "a", "url": "u"}"#).unwrap();
        assert_eq!(
            ok,
            RegistryConfig::Git {
                name: Some("a".into()),
                url: "u".into()
            }
        );
        let err = serde_json::from_str::<RegistryConfig>(
            r#"{"type": "git", "name": "a", "url": "u", "branch": "x"}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field `branch`"), "{err}");
    }

    #[test]
    fn derives_names() {
        for (url, want) in [
            (
                "https://github.com/anthropics/skills.git",
                "anthropics/skills",
            ),
            (
                "git@github.com:FlorianKraemer/claude-ai.git",
                "floriankraemer/claude-ai",
            ),
            (
                "ssh://git@gitlab.com:2222/group/sub/repo.git",
                "group/sub/repo",
            ),
            ("https://gitlab.com/group/sub/repo/", "group/sub/repo"),
            ("https://dev.azure.com/org/proj/_git/repo", "org/proj/repo"),
            ("git@ssh.dev.azure.com:v3/org/proj/repo", "org/proj/repo"),
            ("https://github.com/a/b?x=1#f", "a/b"),
            ("../my-skills", "my-skills"),
            (r"C:\repos\skills.git", "skills"),
        ] {
            assert_eq!(derive_name(url).unwrap(), want, "{url}");
        }
        for bad in ["https://github.com/", "https://h/a b/c", "/"] {
            let err = derive_name(bad).unwrap_err().to_string();
            assert!(
                err.starts_with("cannot derive a repository name from"),
                "{err}"
            );
        }
    }

    #[test]
    fn entries_without_name_derive_it() {
        let dir = std::env::temp_dir().join(format!("aip-derived-{}", std::process::id()));
        let json = json_registry(
            &dir,
            "d.json",
            r#"{"repositories": [{"url": "https://github.com/anthropics/skills.git"}, {"url": "../local-repo"}, {"name": "x", "url": "https://h/y.git"}]}"#,
        );
        let names: Vec<_> = json
            .repositories()
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["anthropics/skills", "local-repo", "x"]);

        let dup = json_registry(
            &dir,
            "dup.json",
            r#"{"repositories": [{"url": "https://a/o/r.git"}, {"url": "git@b:o/r.git"}]}"#,
        );
        let err = dup.repositories().unwrap_err().to_string();
        assert!(err.contains("duplicate repository name 'o/r'"), "{err}");

        let registries = Registries::new(vec![json]);
        let err = registries.get("skills").unwrap_err().to_string();
        assert!(
            err.ends_with("; did you mean 'anthropics/skills'?"),
            "{err}"
        );
        assert!(
            !registries
                .get("zzz")
                .unwrap_err()
                .to_string()
                .contains("did you mean")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_git_without_name() {
        let cfg: RegistryConfig =
            serde_json::from_str(r#"{"type": "git", "url": "git@github.com:Acme/Priv.git"}"#)
                .unwrap();
        assert_eq!(
            cfg,
            RegistryConfig::Git {
                name: None,
                url: "git@github.com:Acme/Priv.git".into()
            }
        );
        assert_eq!(
            serde_json::to_string(&cfg).unwrap(),
            r#"{"type":"git","url":"git@github.com:Acme/Priv.git"}"#
        );
        let repos = cfg
            .open(Path::new("."), &no_context())
            .repositories()
            .unwrap();
        assert_eq!(repos[0].name, "acme/priv");
    }
}
