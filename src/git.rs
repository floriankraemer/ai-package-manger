//! Read-only access to registry repositories through the `git` CLI.
//!
//! Each repository is kept as a bare mirror in a local cache. Content is read straight from git
//! objects, so the cache is never checked out and no `tar` is needed (works on Windows too).

use crate::{Context, Error, Result, filelock, fnv1a};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Repo {
    dir: PathBuf,
}

impl Repo {
    /// Opens the cached mirror of `url` under `cache_root`, cloning or fetching it first.
    pub fn sync(url: &str, cache_root: &Path) -> Result<Self> {
        let key = cache_key(url);
        // Different projects share the cache; hold the lock while cloning or fetching.
        let _lock = filelock::acquire(&cache_root.join(format!("{key}.lock")), url)?;
        let repo = Self {
            dir: cache_root.join(&key),
        };
        if repo.dir.join("HEAD").is_file() {
            repo.git(&["fetch", "--prune", "--quiet"]).map_err(|e| {
                Error::new(format!(
                    "cannot fetch {url}: {}",
                    with_auth_hint(&e.to_string(), url)
                ))
            })?;
            return Ok(repo);
        }
        fs::create_dir_all(cache_root)
            .context(|| format!("cannot create cache directory {}", cache_root.display()))?;
        // Clone next to the final location so an interrupted clone never looks valid.
        let partial = cache_root.join(format!("{key}.partial"));
        if partial.exists() {
            fs::remove_dir_all(&partial)
                .context(|| format!("cannot remove {}", partial.display()))?;
        }
        run(Command::new("git")
            .args(["clone", "--mirror", "--quiet", "--", url])
            .arg(&partial))
        .map_err(|e| {
            Error::new(format!(
                "cannot clone {url}: {}",
                with_auth_hint(&e.to_string(), url)
            ))
        })?;
        fs::rename(&partial, &repo.dir)
            .context(|| format!("cannot move clone to {}", repo.dir.display()))?;
        Ok(repo)
    }

    /// All tag names.
    pub fn tags(&self) -> Result<Vec<String>> {
        let out = self.git(&["for-each-ref", "--format=%(refname:strip=2)", "refs/tags"])?;
        Ok(lines(&out))
    }

    /// Name of the branch the remote's HEAD points to, e.g. `main`.
    pub fn default_branch(&self) -> Result<String> {
        let out = self.git(&["symbolic-ref", "--short", "HEAD"])?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Commit SHA for `rev` (e.g. `refs/tags/v1.0.0`), or `None` if it does not exist.
    pub fn resolve(&self, rev: &str) -> Result<Option<String>> {
        let spec = format!("{rev}^{{commit}}");
        Ok(self
            .git(&["rev-parse", "--verify", "--quiet", &spec])
            .ok()
            .map(|out| String::from_utf8_lossy(&out).trim().to_string()))
    }

    /// Names of the directories directly inside `dir` at commit `sha`.
    pub fn subdirs(&self, sha: &str, dir: &str) -> Result<Vec<String>> {
        let out = self.git(&["ls-tree", "-z", sha, "--", &format!("{dir}/")])?;
        Ok(entries(&out)
            .filter(|e| e.kind == "tree")
            .filter_map(|e| e.path.rsplit('/').next().map(str::to_string))
            .collect())
    }

    /// Text content of the file at `path` in commit `sha`, or `None` if it does not exist.
    pub fn read(&self, sha: &str, path: &str) -> Result<Option<String>> {
        let spec = format!("{sha}:{path}");
        let is_blob = self
            .git(&["cat-file", "-t", &spec])
            .is_ok_and(|kind| kind.trim_ascii() == b"blob");
        if !is_blob {
            return Ok(None);
        }
        let out = self.git(&["cat-file", "blob", &spec])?;
        Ok(Some(String::from_utf8_lossy(&out).into_owned()))
    }

    /// Writes the tree at `path` in commit `sha` into `dest` (created if missing).
    ///
    /// Symlinks are written as regular files containing their target, like git on systems
    /// without symlink support; submodules are skipped.
    pub fn export(&self, sha: &str, path: &str, dest: &Path) -> Result<()> {
        let prefix = format!("{path}/");
        let out = self.git(&["ls-tree", "-r", "-z", sha, "--", &prefix])?;
        fs::create_dir_all(dest).context(|| format!("cannot create {}", dest.display()))?;
        // ponytail: one `git cat-file` process per file; switch to `cat-file --batch` if skills get large.
        for entry in entries(&out).filter(|e| e.kind == "blob") {
            let Some(relative) = entry.path.strip_prefix(&prefix) else {
                continue;
            };
            // Crafted repositories can carry tree entries that escape `dest`.
            if relative
                .split('/')
                .any(|part| matches!(part, "" | "." | "..") || part.contains(['\\', ':']))
            {
                return Err(Error::new(format!(
                    "refusing to export unsafe path '{}'",
                    entry.path
                )));
            }
            let target = relative
                .split('/')
                .fold(dest.to_path_buf(), |p, part| p.join(part));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .context(|| format!("cannot create {}", parent.display()))?;
            }
            let content = self.git(&["cat-file", "blob", entry.oid])?;
            fs::write(&target, content).context(|| format!("cannot write {}", target.display()))?;
            #[cfg(unix)]
            if entry.mode == "100755" {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
                    .context(|| format!("cannot set permissions on {}", target.display()))?;
            }
        }
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        run(Command::new("git")
            .arg("--git-dir")
            .arg(&self.dir)
            .args(args))
    }
}

fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    // Without a terminal nobody can answer a credentials prompt; fail fast instead of hanging.
    if !std::io::stdin().is_terminal() {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new("git not found; aip needs git installed and on PATH")
        } else {
            Error::new(format!("cannot run git: {e}"))
        }
    })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::new(stderr.trim().to_string()))
    }
}

/// Fragments (lowercase) of git's stderr that indicate missing or wrong credentials.
const AUTH_FAILURES: [&str; 7] = [
    "authentication failed",
    "could not read username",
    "could not read password",
    "permission denied (publickey)",
    "repository not found",
    "403",
    "terminal prompts disabled",
];

fn is_auth_failure(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    AUTH_FAILURES.iter().any(|m| stderr.contains(m))
}

/// Appends a hint about private repositories when git's message looks like an auth failure.
fn with_auth_hint(message: &str, url: &str) -> String {
    if !is_auth_failure(message) {
        return message.to_string();
    }
    format!(
        "{message}\nhint: the repository may be private; aip uses your git credentials — check that `git ls-remote {url}` works in this shell (SSH key, credential helper, or token via url.insteadOf)."
    )
}

/// Filesystem-safe, deterministic directory name for a repository URL.
///
/// The readable part maps unsafe characters to `_`, so distinct URLs can share it
/// (`a/b` vs `a_b`); the FNV-1a hash of the exact URL keeps them apart.
fn cache_key(url: &str) -> String {
    let readable: String = url
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let hash = fnv1a(url.as_bytes());
    format!("{readable}-{hash:016x}")
}

fn lines(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

struct Entry<'a> {
    // Only read on Unix, where the executable bit is restored.
    #[cfg_attr(not(unix), allow(dead_code))]
    mode: &'a str,
    kind: &'a str,
    oid: &'a str,
    path: &'a str,
}

/// Parses `git ls-tree -z` output: `<mode> <type> <oid>\t<path>\0`.
fn entries(out: &[u8]) -> impl Iterator<Item = Entry<'_>> {
    out.split(|&b| b == 0).filter_map(|record| {
        let record = std::str::from_utf8(record).ok()?;
        let (meta, path) = record.split_once('\t')?;
        let mut meta = meta.split(' ');
        Some(Entry {
            mode: meta.next()?,
            kind: meta.next()?,
            oid: meta.next()?,
            path,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_do_not_collide() {
        assert_ne!(cache_key("https://h/a/b"), cache_key("https://h/a_b"));
        assert_eq!(cache_key("x"), cache_key("x"));
        assert!(cache_key("https://h/a/b").starts_with("https___h_a_b-"));
    }

    #[test]
    fn auth_failures_get_a_hint() {
        for msg in [
            "fatal: Authentication failed for 'https://h/x.git/'",
            "fatal: could not read Username for 'https://h': terminal prompts disabled",
            "git@h: Permission denied (publickey).",
            "remote: Repository not found.",
            "The requested URL returned error: 403",
        ] {
            let out = with_auth_hint(msg, "https://h/x.git");
            assert!(out.starts_with(msg), "{out}");
            assert!(out.contains("\nhint: ") && out.contains("git ls-remote https://h/x.git"));
        }
        let plain = "fatal: unable to access 'https://h/x.git/': Could not resolve host: h";
        assert_eq!(with_auth_hint(plain, "https://h/x.git"), plain);
    }
}
