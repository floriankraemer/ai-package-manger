//! Read-only access to registry repositories through the `git` CLI.
//!
//! Each repository is kept as a bare mirror in a local cache. Content is read straight from git
//! objects, so the cache is never checked out and no `tar` is needed (works on Windows too).

use crate::registry::is_reserved_device_name;
use crate::{Context, Error, Result, filelock, fnv1a};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Only branches and tags are mirrored. Hosting services expose other refs (GitHub's
/// `refs/pull/*`) that anybody can create, so a tampered lock could point at them.
const FETCH_REFSPECS: [&str; 2] = ["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"];

// ponytail: ceilings against hostile or accidental huge repositories; raise if real skills need more.
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_EXPORT_FILES: usize = 5_000;

pub struct Repo {
    dir: PathBuf,
    lock: PathBuf,
}

impl Repo {
    fn at(url: &str, cache_root: &Path) -> Self {
        let key = cache_key(url);
        Self {
            dir: cache_root.join(&key),
            lock: cache_root.join(format!("{key}.lock")),
        }
    }

    /// Opens the cached mirror of `url` without touching the network; `None` if not cached yet.
    pub fn open(url: &str, cache_root: &Path) -> Result<Option<Self>> {
        let repo = Self::at(url, cache_root);
        let _lock = filelock::acquire(&repo.lock, url)?;
        Ok(repo.is_cached()?.then_some(repo))
    }

    /// Updates the mirror from `url`.
    pub fn fetch(&self, url: &str) -> Result<()> {
        // Different projects share the cache; hold the lock while fetching.
        let _lock = filelock::acquire(&self.lock, url)?;
        self.fetch_locked(url, "fetch")
    }

    /// Opens the cached mirror of `url` under `cache_root`, cloning or fetching it first.
    pub fn sync(url: &str, cache_root: &Path) -> Result<Self> {
        let repo = Self::at(url, cache_root);
        let _lock = filelock::acquire(&repo.lock, url)?;
        if repo.is_cached()? {
            repo.fetch_locked(url, "fetch")?;
        } else {
            repo.create(url, cache_root)?;
        }
        Ok(repo)
    }

    /// True if a mirror exists; upgrades caches made by `git clone --mirror`.
    fn is_cached(&self) -> Result<bool> {
        if !self.dir.join("HEAD").is_file() {
            return Ok(false);
        }
        let configured = self
            .git(&["config", "--get-all", "remote.origin.fetch"])
            .map(|out| lines(&out))
            .unwrap_or_default();
        if configured != FETCH_REFSPECS {
            self.set_refspecs()?;
            self.delete_foreign_refs()?;
        }
        Ok(true)
    }

    fn create(&self, url: &str, cache_root: &Path) -> Result<()> {
        fs::create_dir_all(cache_root)
            .context(|| format!("cannot create cache directory {}", cache_root.display()))?;
        // Clone next to the final location so an interrupted clone never looks valid.
        let partial = Self {
            dir: cache_root.join(format!("{}.partial", cache_key(url))),
            lock: self.lock.clone(),
        };
        if partial.dir.exists() {
            fs::remove_dir_all(&partial.dir)
                .context(|| format!("cannot remove {}", partial.dir.display()))?;
        }
        run(Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&partial.dir))?;
        partial.git(&["config", "remote.origin.url", url])?;
        partial.set_refspecs()?;
        partial.fetch_locked(url, "clone")?;
        fs::rename(&partial.dir, &self.dir)
            .context(|| format!("cannot move clone to {}", self.dir.display()))
    }

    fn set_refspecs(&self) -> Result<()> {
        self.git(&[
            "config",
            "--replace-all",
            "remote.origin.fetch",
            FETCH_REFSPECS[0],
        ])?;
        self.git(&["config", "--add", "remote.origin.fetch", FETCH_REFSPECS[1]])?;
        // `clone --mirror` sets this; it would make fetch ignore the refspecs above. Absent is fine.
        let _ = self.git(&["config", "--unset-all", "remote.origin.mirror"]);
        Ok(())
    }

    fn delete_foreign_refs(&self) -> Result<()> {
        let out = self.git(&["for-each-ref", "--format=%(refname)"])?;
        let commands: String = lines(&out)
            .iter()
            .filter(|r| !r.starts_with("refs/heads/") && !r.starts_with("refs/tags/"))
            .map(|r| format!("delete {r}\n"))
            .collect();
        if commands.is_empty() {
            return Ok(());
        }
        self.git_stdin(&["update-ref", "--stdin"], commands.as_bytes())?;
        Ok(())
    }

    fn fetch_locked(&self, url: &str, verb: &str) -> Result<()> {
        let fail = |e: Error| {
            Error::new(format!(
                "cannot {verb} {url}: {}",
                with_auth_hint(&e.to_string(), url)
            ))
        };
        self.git(&["fetch", "--prune", "--quiet", "--tags", "origin"])
            .map_err(fail)?;
        // The remote may have switched its default branch.
        let out = self
            .git(&["ls-remote", "--symref", "origin", "HEAD"])
            .map_err(fail)?;
        let target = String::from_utf8_lossy(&out).lines().find_map(|l| {
            l.strip_prefix("ref: ")?
                .strip_suffix("\tHEAD")
                .map(str::to_string)
        });
        if let Some(branch) = target.filter(|b| b.starts_with("refs/heads/")) {
            self.git(&["symbolic-ref", "HEAD", &branch])?;
        }
        Ok(())
    }

    /// True if commit `sha` is in the mirror.
    pub fn has_commit(&self, sha: &str) -> bool {
        self.git(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .is_ok()
    }

    /// True if commit `sha` is reachable from `rev` (a `refs/heads/...` name).
    pub fn is_ancestor(&self, sha: &str, rev: &str) -> Result<bool> {
        let output = output(
            &mut self.command(&["merge-base", "--is-ancestor", sha, rev]),
            None,
        )?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::new(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            )),
        }
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
        self.read_limited(sha, path, MAX_FILE_BYTES)
    }

    fn read_limited(&self, sha: &str, path: &str, max_file: u64) -> Result<Option<String>> {
        let spec = format!("{sha}:{path}");
        let is_blob = self
            .git(&["cat-file", "-t", &spec])
            .is_ok_and(|kind| kind.trim_ascii() == b"blob");
        if !is_blob {
            return Ok(None);
        }
        let size = self.git(&["cat-file", "-s", &spec])?;
        check_file_size(
            path,
            String::from_utf8_lossy(&size)
                .trim()
                .parse()
                .unwrap_or(u64::MAX),
            max_file,
        )?;
        let out = self.git(&["cat-file", "blob", &spec])?;
        Ok(Some(String::from_utf8_lossy(&out).into_owned()))
    }

    /// Writes the tree at `path` in commit `sha` into `dest` (created if missing).
    ///
    /// Symlinks are written as regular files containing their target, like git on systems
    /// without symlink support; submodules are skipped.
    pub fn export(&self, sha: &str, path: &str, dest: &Path) -> Result<()> {
        self.export_limited(
            sha,
            path,
            dest,
            MAX_FILE_BYTES,
            MAX_EXPORT_BYTES,
            MAX_EXPORT_FILES,
        )
    }

    fn export_limited(
        &self,
        sha: &str,
        path: &str,
        dest: &Path,
        max_file: u64,
        max_total: u64,
        max_files: usize,
    ) -> Result<()> {
        let prefix = format!("{path}/");
        let out = self.git(&["ls-tree", "-r", "-l", "-z", sha, "--", &prefix])?;
        // Validate everything before writing anything.
        let mut files = Vec::new();
        let mut total = 0u64;
        for entry in entries(&out).filter(|e| e.kind == "blob") {
            let Some(relative) = entry.path.strip_prefix(&prefix) else {
                continue;
            };
            // Crafted repositories can carry tree entries that escape `dest`, or names
            // Windows cannot create; reject them everywhere so locks are portable.
            if relative.split('/').any(unsafe_segment) {
                return Err(Error::new(format!(
                    "refusing to export unsafe path '{}'",
                    entry.path
                )));
            }
            let size = entry.size.unwrap_or(u64::MAX);
            check_file_size(entry.path, size, max_file)?;
            total = total.saturating_add(size);
            if files.len() >= max_files || total > max_total {
                return Err(Error::new(format!(
                    "skill '{path}' is too large; aip limits a skill to {max_files} files and {} MiB",
                    max_total / (1024 * 1024)
                )));
            }
            files.push((entry, relative));
        }
        fs::create_dir_all(dest).context(|| format!("cannot create {}", dest.display()))?;
        // ponytail: one `git cat-file` process per file; switch to `cat-file --batch` if skills get large.
        for (entry, relative) in files {
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
        run(&mut self.command(args))
    }

    fn git_stdin(&self, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
        success(output(&mut self.command(args), Some(input))?)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir").arg(&self.dir).args(args);
        cmd
    }
}

fn check_file_size(path: &str, size: u64, max_file: u64) -> Result<()> {
    if size > max_file {
        return Err(Error::new(format!(
            "skill file '{path}' is {size} bytes; aip limits skill files to {} MiB",
            max_file / (1024 * 1024)
        )));
    }
    Ok(())
}

/// Path segments that escape the destination or that Windows cannot create.
fn unsafe_segment(part: &str) -> bool {
    matches!(part, "" | "." | "..")
        || part.ends_with(['.', ' '])
        || part.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
        || part.chars().any(char::is_control)
        || is_reserved_device_name(part)
}

fn output(cmd: &mut Command, input: Option<&[u8]>) -> Result<Output> {
    // Without a terminal nobody can answer a credentials prompt; fail fast instead of hanging.
    if !std::io::stdin().is_terminal() {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
    // Registry URLs are untrusted: no `ext::` (runs commands), no other exotic transports.
    cmd.env("GIT_ALLOW_PROTOCOL", "https:ssh:git:file")
        .env("GIT_PROTOCOL_FROM_USER", "0");
    let cannot_run = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new("git not found; aip needs git installed and on PATH")
        } else {
            Error::new(format!("cannot run git: {e}"))
        }
    };
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(cannot_run)?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(input).map_err(cannot_run)?;
    }
    child.wait_with_output().map_err(cannot_run)
}

fn success(output: Output) -> Result<Vec<u8>> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::new(stderr.trim().to_string()))
    }
}

fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    success(output(cmd, None)?)
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
    /// Blob size; present only for `ls-tree -l`.
    size: Option<u64>,
    path: &'a str,
}

/// Parses `git ls-tree [-l] -z` output: `<mode> <type> <oid>[ <size>]\t<path>\0`.
fn entries(out: &[u8]) -> impl Iterator<Item = Entry<'_>> {
    out.split(|&b| b == 0).filter_map(|record| {
        let record = std::str::from_utf8(record).ok()?;
        let (meta, path) = record.split_once('\t')?;
        // With `-l` the size is space-padded, and `-` for non-blobs.
        let mut meta = meta.split_whitespace();
        Some(Entry {
            mode: meta.next()?,
            kind: meta.next()?,
            oid: meta.next()?,
            size: meta.next().and_then(|s| s.parse().ok()),
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

    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aip-git-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Source repository with one commit holding `skills/a/SKILL.md`.
    fn source(root: &Path) -> PathBuf {
        let src = root.join("src");
        fs::create_dir_all(src.join("skills/a")).unwrap();
        git_in(&src, &["init", "--quiet"]);
        commit_file(&src, "skills/a/SKILL.md", b"hello");
        src
    }

    fn commit_file(src: &Path, name: &str, content: &[u8]) -> String {
        fs::write(src.join(name), content).unwrap();
        git_in(src, &["add", "."]);
        git_in(src, &["commit", "--quiet", "-m", "c"]);
        git_in(src, &["rev-parse", "HEAD"])
    }

    fn refs(repo: &Repo) -> Vec<String> {
        lines(&repo.git(&["for-each-ref", "--format=%(refname)"]).unwrap())
    }

    #[test]
    fn mirror_has_only_branches_and_tags() {
        let root = scratch();
        let src = source(&root);
        git_in(&src, &["tag", "v1"]);
        git_in(&src, &["update-ref", "refs/pull/1/head", "HEAD"]);
        let url = src.to_str().unwrap();
        let repo = Repo::sync(url, &root.join("cache")).unwrap();
        assert_eq!(refs(&repo), ["refs/heads/main", "refs/tags/v1"]);
        assert!(repo.git(&["config", "remote.origin.mirror"]).is_err());
        assert_eq!(repo.default_branch().unwrap(), "main");
        // Second sync goes through the fetch path.
        Repo::sync(url, &root.join("cache")).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn open_does_not_clone_and_migrates_old_mirrors() {
        let root = scratch();
        let src = source(&root);
        git_in(&src, &["update-ref", "refs/pull/1/head", "HEAD"]);
        let url = src.to_str().unwrap();
        let cache = root.join("cache");
        assert!(Repo::open(url, &cache).unwrap().is_none());
        assert!(!cache.join(cache_key(url)).exists());

        let old = cache.join(cache_key(url));
        git_in(
            &root,
            &["clone", "--mirror", "--quiet", url, old.to_str().unwrap()],
        );
        let repo = Repo::open(url, &cache).unwrap().expect("cached");
        assert_eq!(refs(&repo), ["refs/heads/main"]);
        repo.fetch(url).unwrap();
        assert_eq!(refs(&repo), ["refs/heads/main"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_follows_default_branch_changes() {
        let root = scratch();
        let src = source(&root);
        git_in(&src, &["branch", "dev"]);
        let url = src.to_str().unwrap();
        let repo = Repo::sync(url, &root.join("cache")).unwrap();
        assert_eq!(repo.default_branch().unwrap(), "main");
        git_in(&src, &["symbolic-ref", "HEAD", "refs/heads/dev"]);
        repo.fetch(url).unwrap();
        assert_eq!(repo.default_branch().unwrap(), "dev");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_presence_and_ancestry() {
        let root = scratch();
        let src = source(&root);
        let first = git_in(&src, &["rev-parse", "HEAD"]);
        git_in(&src, &["checkout", "--quiet", "-b", "side"]);
        let side = commit_file(&src, "side", b"s");
        git_in(&src, &["checkout", "--quiet", "main"]);
        commit_file(&src, "main", b"m");
        let repo = Repo::sync(src.to_str().unwrap(), &root.join("cache")).unwrap();
        assert!(repo.has_commit(&first) && repo.has_commit(&side));
        assert!(!repo.has_commit(&"0".repeat(40)));
        assert!(repo.is_ancestor(&first, "refs/heads/main").unwrap());
        assert!(!repo.is_ancestor(&side, "refs/heads/main").unwrap());
        assert!(repo.is_ancestor(&first, "refs/heads/missing").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ext_transport_is_refused() {
        let root = scratch();
        let marker = root.join("pwned");
        let url = format!("ext::sh -c touch% {}", marker.display());
        assert!(Repo::sync(&url, &root.join("cache")).is_err());
        assert!(!marker.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn size_limits() {
        let root = scratch();
        let src = source(&root);
        commit_file(&src, "skills/a/big.bin", &[0u8; 2048]);
        let sha = git_in(&src, &["rev-parse", "HEAD"]);
        let repo = Repo::sync(src.to_str().unwrap(), &root.join("cache")).unwrap();

        assert_eq!(
            repo.read(&sha, "skills/a/SKILL.md").unwrap().as_deref(),
            Some("hello")
        );
        let err = repo
            .read_limited(&sha, "skills/a/big.bin", 1024 * 1024)
            .unwrap();
        assert!(err.is_some());
        let err = repo
            .read_limited(&sha, "skills/a/big.bin", 1024)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "skill file 'skills/a/big.bin' is 2048 bytes; aip limits skill files to 0 MiB"
        );

        let dest = root.join("out");
        let mib = 1024 * 1024;
        let err = repo
            .export_limited(&sha, "skills/a", &dest, 1024, 100 * mib, 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("'skills/a/big.bin' is 2048 bytes"), "{err}");
        let err = repo
            .export_limited(&sha, "skills/a", &dest, mib, 1024, 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too large"), "{err}");
        let err = repo
            .export_limited(&sha, "skills/a", &dest, mib, 100 * mib, 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too large"), "{err}");
        assert!(!dest.exists(), "nothing is written when validation fails");

        repo.export(&sha, "skills/a", &dest).unwrap();
        assert_eq!(fs::read(dest.join("SKILL.md")).unwrap(), b"hello");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_segments() {
        for bad in [
            "", "..", "a.", "a ", "CON", "nul.txt", "a:b", "a<b", "a|b", "a?b", "a*b", "a\"b",
            "a\x01b", "a\\b",
        ] {
            assert!(unsafe_segment(bad), "{bad:?}");
        }
        for ok in ["SKILL.md", "console.md", ".gitignore", "a-b_c"] {
            assert!(!unsafe_segment(ok), "{ok:?}");
        }
    }

    #[test]
    fn export_rejects_windows_device_names() {
        let root = scratch();
        let src = source(&root);
        commit_file(&src, "skills/a/NUL", b"x");
        let sha = git_in(&src, &["rev-parse", "HEAD"]);
        let repo = Repo::sync(src.to_str().unwrap(), &root.join("cache")).unwrap();
        let err = repo
            .export(&sha, "skills/a", &root.join("out"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsafe path 'skills/a/NUL'"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }
}
