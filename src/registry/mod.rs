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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub url: String,
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
    Git { name: String, url: String },
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
        self.entries()?
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| {
                let locations: Vec<_> = self.registries.iter().map(|r| r.location()).collect();
                Error::new(format!(
                    "unknown repository '{name}' (not listed in any registry: {})",
                    locations.join(", ")
                ))
            })
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
pub fn valid_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// Checks a registry entry (name, no credentials in the URL) and resolves a relative local URL
/// against `base`. `location` names the registry in error messages.
pub(crate) fn validated_entry(mut entry: Entry, base: &Path, location: &str) -> Result<Entry> {
    if !valid_name(&entry.name) {
        return Err(Error::new(format!(
            "invalid repository name '{}' in registry {location} (use lowercase letters, digits, '.', '_', '-')",
            entry.name
        )));
    }
    check_no_credentials(&entry.url)
        .context(|| format!("repository '{}' in {location}", entry.name))?;
    entry.url = resolve_url(&entry.url, base)?;
    Ok(entry)
}

/// Makes a relative local URL absolute against `base`; remote URLs are returned unchanged.
pub(crate) fn resolve_url(url: &str, base: &Path) -> Result<String> {
    if !is_local(url) || !Path::new(url).is_relative() {
        return Ok(url.to_string());
    }
    let local = path::absolute(base.join(url)).context(|| format!("cannot resolve {url}"))?;
    Ok(normalize(&local).to_string_lossy().into_owned())
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
    name.split_once('/')
        .filter(|(repo, skill)| valid_name(repo) && valid_name(skill))
        .ok_or_else(|| {
            Error::new(format!(
                "invalid package name '{name}' (expected <repository>/<skill> using lowercase letters, digits, '.', '_', '-')"
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
        .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
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
        assert!(split_package("a/b/c").is_err());
    }

    #[test]
    fn frontmatter_description() {
        let md = "---\nname: pdf\ndescription: \"Work with PDFs\"\n---\n# PDF\ndescription: no";
        assert_eq!(description(md).as_deref(), Some("Work with PDFs"));
        assert_eq!(description("# no frontmatter"), None);
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
                name: name.into(),
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
                name: "a".into(),
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
}
