//! Package operations: `search`, `check`, `install`, `update` and `remove`.

use crate::auth::Auth;
use crate::constraint::{self, Constraint};
use crate::detect::{self, Scope};
use crate::filelock::{self, FileLock};
use crate::git::Repo;
use crate::manifest::{
    self, LOCK_FILE, Lock, LockedPackage, MANIFEST_FILE, Manifest, Requirement, Source,
};
use crate::registry::{self, JsonRegistry, Registries, RegistryContext};
use crate::{Context, Error, Result, fnv1a};
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// aip's own folder: `$AIP_HOME`, else `~/.aip`. Holds the default registry, the repository
/// cache and the global aip.json / aip.lock.
pub fn aip_home() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("AIP_HOME").filter(|d| !d.is_empty()) {
        return Ok(dir.into());
    }
    detect::global_root()
        .map(|home| home.join(".aip"))
        .ok_or_else(|| Error::new("cannot determine home directory"))
}

/// Where a manifest lives and where its skills get installed.
pub struct Project {
    pub scope: Scope,
    /// Folder that skill folders are relative to: the project, or the home folder for global.
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub lock_path: PathBuf,
    pub aip_home: PathBuf,
}

impl Project {
    pub fn local(dir: PathBuf, aip_home: PathBuf) -> Self {
        Self {
            scope: Scope::Project,
            manifest_path: dir.join(MANIFEST_FILE),
            lock_path: dir.join(LOCK_FILE),
            root: dir,
            aip_home,
        }
    }

    /// Like `composer global`: manifest and lock in aip's home, skills in the user's home.
    pub fn global(home: PathBuf, aip_home: PathBuf) -> Self {
        Self {
            scope: Scope::Global,
            root: home,
            manifest_path: aip_home.join(MANIFEST_FILE),
            lock_path: aip_home.join(LOCK_FILE),
            aip_home,
        }
    }

    /// Registries from aip.json (paths relative to it), else `$AIP_HOME/registry.json`.
    pub fn registries(&self, manifest: Option<&Manifest>) -> Registries {
        let auth_env = env::var("AIP_AUTH").ok();
        self.registries_with_auth(manifest, auth_env)
    }

    /// Like [`Self::registries`], with the text of `AIP_AUTH` passed in.
    pub fn registries_with_auth(
        &self,
        manifest: Option<&Manifest>,
        auth_env: Option<String>,
    ) -> Registries {
        let base = self.manifest_path.parent().unwrap_or(Path::new(""));
        let ctx = self.registry_context(auth_env);
        let configured: Vec<_> = manifest
            .map(|m| m.registries.iter().map(|r| r.open(base, &ctx)).collect())
            .unwrap_or_default();
        if !configured.is_empty() {
            return Registries::new(configured);
        }
        Registries::new(vec![Box::new(JsonRegistry::file(
            self.aip_home.join("registry.json"),
        ))])
    }

    /// Registry cache folder and credentials: `AIP_AUTH`, the project's `auth.json` next to
    /// aip.json (project scope only), then `auth.json` in aip's home.
    fn registry_context(&self, auth_env: Option<String>) -> RegistryContext {
        let project_dir = match self.scope {
            Scope::Project => self.manifest_path.parent(),
            Scope::Global => None,
        };
        RegistryContext {
            cache_dir: self.aip_home.join("cache").join("registries"),
            auth: Auth::standard(auth_env, project_dir, &self.aip_home),
        }
    }
}

impl Project {
    /// aip.json's folder, absolute and normalized.
    fn base_dir(&self) -> Result<PathBuf> {
        let dir = self.manifest_path.parent().unwrap_or(Path::new(""));
        let dir = std::path::absolute(dir)
            .context(|| format!("cannot resolve {}", self.manifest_path.display()))?;
        Ok(registry::normalize(&dir))
    }

    /// aip.lock with local source URLs resolved against aip.json's folder, so they compare
    /// equal to the registries' URLs. An empty lock if the file does not exist.
    fn read_lock(&self) -> Result<Lock> {
        let base = self.manifest_path.parent().unwrap_or(Path::new(""));
        let mut lock = Lock::read(&self.lock_path)?;
        for pkg in &mut lock.packages {
            pkg.source.url = registry::resolve_url(&pkg.source.url, base)?;
        }
        Ok(lock)
    }

    /// Writes aip.lock with local source URLs relative to aip.json's folder (see
    /// [`registry::portable_url`]), so a committed lock works on every machine.
    fn write_lock(&self, lock: &Lock) -> Result<bool> {
        let base = self.base_dir()?;
        let mut portable = lock.clone();
        for pkg in &mut portable.packages {
            pkg.source.url = registry::portable_url(&pkg.source.url, &base);
        }
        manifest::write_json(&self.lock_path, &portable)
    }

    /// Serializes aip runs on this project: one lock per manifest path under `$AIP_HOME/locks`.
    fn lock(&self) -> Result<FileLock> {
        let manifest = std::path::absolute(&self.manifest_path)
            .context(|| format!("cannot resolve {}", self.manifest_path.display()))?;
        let key = fnv1a(manifest.to_string_lossy().as_bytes());
        let path = self.aip_home.join("locks").join(format!("{key:016x}.lock"));
        filelock::acquire(&path, &self.manifest_path.display().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Install {
        name: String,
        version: String,
    },
    Update {
        name: String,
        from: String,
        to: String,
        /// `(old, new)` folder name when the package's `"as"` changed.
        folder: Option<(String, String)>,
    },
    Remove {
        name: String,
        version: String,
    },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install { name, version } => write!(f, "Installing {name} ({version})"),
            Self::Remove { name, version } => write!(f, "Removing {name} ({version})"),
            Self::Update {
                name,
                from,
                to,
                folder,
            } => {
                let verb = match (
                    constraint::parse_version(from),
                    constraint::parse_version(to),
                ) {
                    (Some(a), Some(b)) if b > a => "Upgrading",
                    (Some(a), Some(b)) if b < a => "Downgrading",
                    _ => "Updating",
                };
                write!(f, "{verb} {name} ({from} => {to}")?;
                if let Some((old, new)) = folder {
                    write!(f, ", folder {old} => {new}")?;
                }
                f.write_str(")")
            }
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub changes: Vec<Change>,
    /// Skill folders written to.
    pub targets: Vec<PathBuf>,
    /// `(package, skill folder)` copies removed from providers no longer in aip.json.
    pub pruned: Vec<(String, PathBuf)>,
}

/// Flags shared by `install`, `update` and `remove`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Replace skill folders that aip did not install, and discard local changes in its own.
    pub force: bool,
    /// Work out and report the changes without writing anything.
    pub dry_run: bool,
}

/// `aip install [packages]`: adds `<name>[:<constraint>]` specs to aip.json, then installs
/// everything from aip.lock. Requirements the lock does not cover are an error unless named.
pub fn install(project: &Project, specs: &[String], options: Options) -> Result<Report> {
    install_as(project, specs, None, options)
}

/// [`install`] with `--as`: the one package named in `specs` is installed into folder `alias`.
pub fn install_as(
    project: &Project,
    specs: &[String],
    alias: Option<&str>,
    options: Options,
) -> Result<Report> {
    if let Some(alias) = alias {
        if specs.len() != 1 {
            return Err(Error::new("--as needs exactly one package"));
        }
        if !registry::valid_name(alias) {
            return Err(Error::new(format!(
                "invalid --as '{alias}' (use lowercase letters, digits, '.', '_', '-')"
            )));
        }
    }
    let _lock = project.lock()?;
    let existing = Manifest::read(&project.manifest_path)?;
    if existing.is_none() && specs.is_empty() {
        return Err(Error::new(format!(
            "no {MANIFEST_FILE} in {}; run `aip install <repository>/<skill>` to add a skill",
            project.root.display()
        )));
    }
    let mut manifest = existing.unwrap_or_default();
    let old = project.read_lock()?;
    let targets = targets(project, &mut manifest)?;
    let mut resolver = Resolver::new(project, &manifest);
    let mut new = old.clone();
    let mut forced = HashSet::new();
    // Like Composer without composer.lock (a fresh clone): resolve what aip.json requires.
    if !project.lock_path.exists() && !manifest.require.is_empty() {
        eprintln!("No {LOCK_FILE} found; resolving requirements from {MANIFEST_FILE}.");
        forced.extend(manifest.require.keys().cloned());
    }
    for spec in specs {
        let (name, constraint) = match spec.split_once(':') {
            Some((name, constraint)) => (name, Some(constraint.trim())),
            None => (spec.as_str(), None),
        };
        registry::split_package(name)?;
        let constraint = match constraint {
            Some(c) => {
                Constraint::parse(c).map_err(|e| Error::new(format!("{name}: {e}")))?;
                c.to_string()
            }
            None => resolver.default_constraint(name)?,
        };
        new.upsert(resolver.resolve(name, &constraint)?);
        // Naming a package again keeps its folder unless `--as` gives a new one.
        let alias = alias
            .map(str::to_string)
            .or_else(|| manifest.require.get(name).and_then(|r| r.alias.clone()));
        let requirement = Requirement {
            version: constraint,
            alias,
        };
        manifest.require.insert(name.to_string(), requirement);
        forced.insert(name.to_string());
    }
    refresh(&manifest, &mut new, &mut resolver, &forced)?;
    finish(
        project,
        &manifest,
        &old,
        &new,
        &targets,
        &mut resolver,
        options,
    )
}

/// `aip update [packages]`: re-resolves the named packages (all if none) to the newest
/// versions their constraints allow.
pub fn update(project: &Project, names: &[String], options: Options) -> Result<Report> {
    let _lock = project.lock()?;
    let mut manifest = Manifest::read(&project.manifest_path)?.ok_or_else(|| {
        Error::new(format!(
            "no {MANIFEST_FILE} in {}",
            project
                .manifest_path
                .parent()
                .unwrap_or(Path::new(""))
                .display()
        ))
    })?;
    if let Some(unknown) = names.iter().find(|n| !manifest.require.contains_key(*n)) {
        return Err(Error::new(format!(
            "package '{unknown}' is not required in {}",
            project.manifest_path.display()
        )));
    }
    let forced: HashSet<String> = if names.is_empty() {
        manifest.require.keys().cloned().collect()
    } else {
        names.iter().cloned().collect()
    };
    let old = project.read_lock()?;
    let targets = targets(project, &mut manifest)?;
    let mut resolver = Resolver::new(project, &manifest);
    let mut new = old.clone();
    refresh(&manifest, &mut new, &mut resolver, &forced)?;
    finish(
        project,
        &manifest,
        &old,
        &new,
        &targets,
        &mut resolver,
        options,
    )
}

/// `aip init [--provider id]...`: creates aip.json. Without `providers`, uses `aip detect`.
/// Returns the skill folders the providers resolve to.
pub fn init(project: &Project, providers: Vec<String>) -> Result<Vec<PathBuf>> {
    let _lock = project.lock()?;
    if project.manifest_path.exists() {
        return Err(Error::new(format!(
            "{} already exists",
            project.manifest_path.display()
        )));
    }
    let mut manifest = Manifest {
        providers,
        ..Manifest::default()
    };
    // Validates ids and, when none were given, fills them in from detection.
    let dirs = targets(project, &mut manifest)?;
    let mut seen = HashSet::new();
    manifest.providers.retain(|id| seen.insert(id.clone()));
    manifest::write_json(&project.manifest_path, &manifest)?;
    Ok(dirs)
}

/// `aip remove [packages]`: drops the packages from aip.json and deletes their skill folders.
pub fn remove(project: &Project, names: &[String], options: Options) -> Result<Report> {
    let _lock = project.lock()?;
    let mut manifest = Manifest::read(&project.manifest_path)?.ok_or_else(|| {
        Error::new(format!(
            "no {MANIFEST_FILE} in {}",
            project
                .manifest_path
                .parent()
                .unwrap_or(Path::new(""))
                .display()
        ))
    })?;
    for name in names {
        if manifest.require.remove(name).is_none() {
            return Err(Error::new(format!(
                "package '{name}' is not required in {}",
                project.manifest_path.display()
            )));
        }
    }
    let old = project.read_lock()?;
    let targets = targets(project, &mut manifest)?;
    let mut resolver = Resolver::new(project, &manifest);
    let mut new = old.clone();
    refresh(&manifest, &mut new, &mut resolver, &HashSet::new())?;
    finish(
        project,
        &manifest,
        &old,
        &new,
        &targets,
        &mut resolver,
        options,
    )
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `aip search <query>`: skills at their latest version whose name or description contains
/// `query` (case-insensitive). Unreachable repositories are reported and skipped.
pub fn search(project: &Project, query: &str) -> Result<Vec<SearchHit>> {
    let manifest = Manifest::read(&project.manifest_path)?;
    let mut resolver = Resolver::new(project, &manifest.unwrap_or_default());
    let entries = resolver.registries().entries()?;
    let query = query.to_lowercase();
    let mut hits = Vec::new();
    for entry in entries {
        let found = resolver.repo(&entry.url).and_then(|repo| {
            let branch = repo.default_branch()?;
            let head = repo
                .resolve(&format!("refs/heads/{branch}"))?
                .ok_or_else(|| Error::new(format!("default branch '{branch}' has no commits")))?;
            let mut found = Vec::new();
            for skill in repo.subdirs(&head, "skills")? {
                if registry::valid_name(&skill)
                    && let Some(hit) = skill_hit(repo, &entry.name, &skill)?
                {
                    found.push(hit);
                }
            }
            Ok(found)
        });
        match found {
            Ok(found) => hits.extend(found.into_iter().filter(|hit| {
                hit.name.contains(&query)
                    || hit
                        .description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&query))
            })),
            Err(e) => eprintln!("warning: skipping repository '{}': {e}", entry.name),
        }
    }
    Ok(hits)
}

/// `aip info <repository>/<skill>`: the skill at its latest version. Syncs only that repository.
pub fn info(project: &Project, name: &str) -> Result<SearchHit> {
    let (repository, skill) = registry::split_package(name)?;
    let manifest = Manifest::read(&project.manifest_path)?;
    let mut resolver = Resolver::new(project, &manifest.unwrap_or_default());
    let entry = resolver.registries().get(repository)?;
    let repo = resolver.repo(&entry.url)?;
    skill_hit(repo, repository, skill)?
        .ok_or_else(|| Error::new(format!("skill '{name}' not found in {}", entry.url)))
}

/// The skill at its latest version (see [`latest`]); `None` if its SKILL.md is missing there.
fn skill_hit(repo: &Repo, repository: &str, skill: &str) -> Result<Option<SearchHit>> {
    let (version, sha) = latest(repo, skill)?;
    let Some(skill_md) = repo.read(&sha, &format!("skills/{skill}/SKILL.md"))? else {
        return Ok(None);
    };
    Ok(Some(SearchHit {
        name: format!("{repository}/{skill}"),
        version,
        description: registry::description(&skill_md),
    }))
}

/// Where a required skill stands relative to aip.lock, the skill folders and the registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Status {
    UpToDate,
    /// A newer version within the constraint; `beyond` is a newer one outside it.
    UpdateAvailable {
        to: String,
        beyond: Option<String>,
    },
    /// Locked at the newest version the constraint allows, but a newer one exists.
    OutsideConstraint {
        latest: String,
    },
    /// The branch head moved; `to` is its short commit.
    NewCommits {
        to: String,
    },
    NotInstalled,
    FolderMissing,
    /// A skill folder does not hold the locked commit (or predates aip's markers).
    FolderOutdated,
    /// Files in a skill folder were edited, added or deleted since aip installed it; `files` is
    /// how many differ (in the folder with the most).
    ModifiedLocally {
        files: usize,
    },
    SourceChanged {
        registry_url: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillStatus {
    pub name: String,
    pub constraint: String,
    pub installed: Option<String>,
    pub latest: Option<String>,
    pub status: Status,
}

impl fmt::Display for SkillStatus {
    /// The status text; `OutsideConstraint` also names the constraint.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.status {
            Status::UpToDate => f.write_str("up to date"),
            Status::UpdateAvailable { to, beyond: None } => write!(f, "update available: {to}"),
            Status::UpdateAvailable {
                to,
                beyond: Some(beyond),
            } => write!(f, "update available: {to} ({beyond} outside constraint)"),
            Status::OutsideConstraint { latest } => {
                write!(f, "{latest} outside {}", self.constraint)
            }
            Status::NewCommits { to } => write!(f, "new commits: {to}"),
            Status::NotInstalled => f.write_str("not installed"),
            Status::FolderMissing => f.write_str("folder missing"),
            Status::FolderOutdated => f.write_str("folder out of date"),
            Status::ModifiedLocally { files } => {
                write!(f, "modified locally ({})", files_label(*files))
            }
            Status::SourceChanged { .. } => f.write_str("source changed"),
            Status::Error { .. } => f.write_str("error (see below)"),
        }
    }
}

/// `aip check`: read-only report of every required skill against aip.lock, the skill folders
/// and the registry. A package that cannot be resolved gets an `Error` status instead of
/// failing the whole check.
pub fn check(project: &Project) -> Result<Vec<SkillStatus>> {
    let manifest = Manifest::read(&project.manifest_path)?.ok_or_else(|| {
        Error::new(format!(
            "no {MANIFEST_FILE} in {}",
            project
                .manifest_path
                .parent()
                .unwrap_or(Path::new(""))
                .display()
        ))
    })?;
    let lock = project.read_lock()?;
    // `targets` may fill in providers; do that on a copy so nothing is written back.
    let targets = targets(project, &mut manifest.clone())?;
    let mut resolver = Resolver::new(project, &manifest);
    resolver.locked_urls = lock
        .packages
        .iter()
        .map(|p| (p.name.clone(), p.source.url.clone()))
        .collect();
    Ok(manifest
        .require
        .iter()
        .map(|(name, requirement)| {
            let constraint = &requirement.version;
            let locked = lock.get(name);
            // The folder aip.json asks for, even if aip.lock still has another `as`.
            let wanted = locked.map(|p| LockedPackage {
                alias: requirement.alias.clone(),
                ..p.clone()
            });
            let (latest, status) =
                check_package(&mut resolver, name, constraint, wanted.as_ref(), &targets)
                    .unwrap_or_else(|e| {
                        (
                            None,
                            Status::Error {
                                message: e.to_string(),
                            },
                        )
                    });
            SkillStatus {
                name: name.clone(),
                constraint: constraint.clone(),
                installed: locked.map(|p| p.version.clone()),
                latest,
                status,
            }
        })
        .collect())
}

/// Latest version label and status of one requirement.
fn check_package(
    resolver: &mut Resolver,
    name: &str,
    constraint: &str,
    locked: Option<&LockedPackage>,
    targets: &[PathBuf],
) -> Result<(Option<String>, Status)> {
    let registry_url = resolver.url_of(name)?;
    let allowed = resolver.resolve(name, constraint)?;
    let (is_branch, pinned) = match Constraint::parse(constraint) {
        Ok(Constraint::Branch { name, commit }) => (true, commit.map(|_| name)),
        _ => (false, None),
    };
    let latest = if let Some(branch) = pinned {
        // A pin is deliberate: show where the branch is, but do not report it as outdated.
        let head = resolver.resolve(name, &format!("dev-{branch}"))?;
        Some(short_sha(&head.source.reference).to_string())
    } else if is_branch {
        Some(allowed.version.clone())
    } else {
        resolver
            .resolve(name, &latest_query(constraint))
            .ok()
            .map(|p| p.version)
    };
    let (mut folder_missing, mut folder_outdated, mut modified) = (false, false, 0);
    if let Some(p) = locked {
        let folder = folder_of(p)?;
        for target in targets {
            let dir = target.join(folder);
            folder_missing |= !dir.is_dir();
            match marker(&dir) {
                Some(m) if m.name == p.name && m.reference == p.source.reference => {
                    let repo = resolver.mirror(&p.source.url)?;
                    if let Some(changed) = local_changes(&dir, &m, repo)? {
                        modified = modified.max(changed.len());
                    }
                }
                _ => folder_outdated = true,
            }
        }
    }
    let status = match locked {
        None => Status::NotInstalled,
        Some(p) if p.source.url != registry_url => Status::SourceChanged { registry_url },
        Some(_) if folder_missing => Status::FolderMissing,
        Some(_) if folder_outdated => Status::FolderOutdated,
        Some(_) if modified > 0 => Status::ModifiedLocally { files: modified },
        Some(p) if is_branch => {
            if p.source.reference == allowed.source.reference {
                Status::UpToDate
            } else {
                Status::NewCommits {
                    to: short_sha(&allowed.source.reference).to_string(),
                }
            }
        }
        Some(p) => range_status(p, &allowed, latest.as_deref()),
    };
    Ok((latest, status))
}

/// What "latest" means for `constraint`: `*`, with the constraint's stability flag if it has one,
/// so `^1.0@beta` is compared against the newest beta rather than the newest stable release.
fn latest_query(constraint: &str) -> String {
    match constraint.rsplit_once('@') {
        Some((_, flag)) => format!("*@{flag}"),
        None => "*".to_string(),
    }
}

/// Status of a locked package against a range constraint.
fn range_status(locked: &LockedPackage, allowed: &LockedPackage, latest: Option<&str>) -> Status {
    let allowed_version = constraint::parse_version(&allowed.version);
    let newer_than_allowed = latest.filter(|l| {
        matches!((constraint::parse_version(l), &allowed_version), (Some(l), Some(a)) if l > *a)
    });
    if allowed.source.reference != locked.source.reference {
        return Status::UpdateAvailable {
            to: allowed.version.clone(),
            beyond: newer_than_allowed.map(str::to_string),
        };
    }
    match newer_than_allowed {
        Some(latest) => Status::OutsideConstraint {
            latest: latest.to_string(),
        },
        None => Status::UpToDate,
    }
}

/// `1 file`, `3 files`.
fn files_label(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Drops lock entries no longer required and resolves the `forced` requirements. Any other
/// requirement that is missing from the lock or no longer satisfied by it is an error, like
/// Composer: only `aip update` (or naming the package in `aip install`) changes versions.
fn refresh(
    manifest: &Manifest,
    lock: &mut Lock,
    resolver: &mut Resolver,
    forced: &HashSet<String>,
) -> Result<()> {
    lock.packages
        .retain(|p| manifest.require.contains_key(&p.name));
    let mut stale = BTreeMap::new();
    for (name, requirement) in &manifest.require {
        let constraint = &requirement.version;
        if forced.contains(name) {
            lock.upsert(resolver.resolve(name, constraint)?);
            continue;
        }
        match lock.get(name) {
            None => {
                stale.insert(name, format!("not in {LOCK_FILE}"));
            }
            Some(p) if !constraint::satisfies(constraint, &p.version) => {
                stale.insert(
                    name,
                    format!("locked {} does not satisfy {constraint}", p.version),
                );
            }
            Some(_) => {}
        }
    }
    // A changed `"as"` only moves the folder; it needs no new resolution.
    for pkg in &mut lock.packages {
        pkg.alias = manifest.require[&pkg.name].alias.clone();
    }
    if stale.is_empty() {
        return Ok(());
    }
    let details: String = stale
        .iter()
        .map(|(name, why)| format!("\n  - {name}: {why}"))
        .collect();
    let names: Vec<_> = stale.keys().map(|n| n.as_str()).collect();
    Err(Error::new(format!(
        "{LOCK_FILE} is out of date with {MANIFEST_FILE}:{details}\nRun `aip update {}` to resolve {}.",
        names.join(" "),
        if names.len() == 1 { "it" } else { "them" }
    )))
}

/// Lock files are untrusted input: every locked URL must be the one the registries list for
/// the package's repository. `aip update <name>` re-resolves and repairs a mismatch.
fn verify_sources(lock: &Lock, resolver: &mut Resolver) -> Result<()> {
    for pkg in &lock.packages {
        let (repository, _) = registry::split_package(&pkg.name)?;
        let entry = resolver
            .registries()
            .get_locked(repository, &pkg.source.url)
            .map_err(|e| Error::new(format!("{}: {e}", pkg.name)))?;
        if entry.url != pkg.source.url {
            return Err(Error::new(format!(
                "{LOCK_FILE}: {name} is locked to {locked}, but the registries list {listed} for '{repository}'; run `aip update {name}` to re-resolve it",
                name = pkg.name,
                locked = pkg.source.url,
                listed = entry.url,
            )));
        }
    }
    Ok(())
}

fn finish(
    project: &Project,
    manifest: &Manifest,
    old: &Lock,
    new: &Lock,
    targets: &[PathBuf],
    resolver: &mut Resolver,
    options: Options,
) -> Result<Report> {
    verify_sources(new, resolver)?;
    for target in targets {
        checked_dir(&project.root, target, project.scope)?;
    }
    // Folders of providers not in aip.json, scanned for copies aip left there. One behind a
    // symbolic link is skipped rather than failing: it is not ours to write to.
    let others: Vec<PathBuf> = skill_dirs(project, detect::PROVIDERS.iter())
        .into_iter()
        .filter(|d| !targets.contains(d) && checked_dir(&project.root, d, project.scope).is_ok())
        .collect();
    unique_folders(manifest, old, new)?;
    let plan = plan(old, new, targets, &others, resolver, options.force)?;
    let report = Report {
        changes: plan.changes,
        targets: targets.to_vec(),
        pruned: plan.pruned,
    };
    if options.dry_run {
        return Ok(report);
    }
    // Stage every export first: a failure here leaves the project exactly as it was.
    let mut staged = Vec::new();
    let written = stage(&plan.writes, resolver, &mut staged).and_then(|()| {
        manifest::write_json(&project.manifest_path, manifest)?;
        project.write_lock(new)
    });
    if let Err(e) = written {
        for (dir, skill) in &staged {
            let _ = delete(&staging_path(dir, skill));
        }
        return Err(e);
    }
    // ponytail: the swaps below are one rename each but not atomic as a group; if one fails, the
    // lock is already written and the folders' markers show what is stale, so re-running
    // `aip install` completes the job. A journal would close the window if that ever matters.
    for (dir, skill) in &staged {
        swap(dir, skill)?;
    }
    for path in &plan.deletes {
        delete(path)?;
    }
    Ok(report)
}

/// Refuses a skill folder reached through a symbolic link inside a project: a committed
/// `.claude/skills -> ~/.local` would make aip write (and delete) outside the project.
/// Global installs may use symlinked dotfiles, so they are not checked.
fn checked_dir(root: &Path, dir: &Path, scope: Scope) -> Result<()> {
    if scope == Scope::Global {
        return Ok(());
    }
    let relative = dir.strip_prefix(root).map_err(|_| {
        Error::new(format!(
            "refusing to use {}: it is outside {}",
            dir.display(),
            root.display()
        ))
    })?;
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match path.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::new(format!(
                    "refusing to use {}: {} is a symbolic link",
                    dir.display(),
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(_) => break, // the rest does not exist yet
        }
    }
    Ok(())
}

/// Skill folders of `providers` for the project's scope, without duplicates.
fn skill_dirs<'a>(
    project: &Project,
    providers: impl Iterator<Item = &'a detect::Provider>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for p in providers {
        let Some((local, global)) = p.skills else {
            continue;
        };
        let rel = match project.scope {
            Scope::Project => local,
            Scope::Global => global,
        };
        let dir = detect::scoped_path(p, project.scope, &project.root, rel);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Skill folders of the manifest's providers. Fills in `providers` from `aip detect` if empty.
fn targets(project: &Project, manifest: &mut Manifest) -> Result<Vec<PathBuf>> {
    if manifest.providers.is_empty() {
        let found = detect::detect(&project.root, project.scope)
            .context(|| format!("cannot scan {}", project.root.display()))?;
        manifest.providers = found
            .iter()
            .filter(|d| detect::provider(d.id).is_some_and(|p| p.skills.is_some()))
            .map(|d| d.id.to_string())
            .collect();
        if manifest.providers.is_empty() {
            return Err(Error::new(format!(
                "no AI provider with skills support detected in {}; add \"providers\" to {} (e.g. [\"claude-code\"]) or run `aip init --provider <id>`",
                project.root.display(),
                project.manifest_path.display()
            )));
        }
    }
    let mut providers = Vec::new();
    for id in &manifest.providers {
        let provider = detect::provider(id).ok_or_else(|| {
            let known: Vec<_> = detect::PROVIDERS.iter().map(|p| p.id).collect();
            Error::new(format!(
                "unknown provider '{id}' in {} (known: {})",
                project.manifest_path.display(),
                known.join(", ")
            ))
        })?;
        if provider.skills.is_none() {
            eprintln!("warning: provider '{id}' does not support skills; skipping");
            continue;
        }
        providers.push(provider);
    }
    let dirs = skill_dirs(project, providers.into_iter());
    if dirs.is_empty() {
        return Err(Error::new(format!(
            "none of the providers in {} support skills",
            project.manifest_path.display()
        )));
    }
    Ok(dirs)
}

/// File aip writes into every skill folder it installs, naming the package and commit. It is
/// what makes a folder aip's: only folders whose marker names a package are replaced or
/// deleted for it without `--force`.
pub const MARKER_FILE: &str = ".aip-package";

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Marker {
    name: String,
    reference: String,
    /// Blob id of every file aip wrote, by `/`-separated path relative to the folder. Absent in
    /// markers of older aip versions, whose folders count as unmodified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<BTreeMap<String, String>>,
}

/// The marker of the skill folder `dir`; `None` if there is none or `dir` is a symbolic link,
/// which aip never follows.
fn marker(dir: &Path) -> Option<Marker> {
    if !dir.symlink_metadata().ok()?.is_dir() {
        return None;
    }
    let path = dir.join(MARKER_FILE);
    if !path.symlink_metadata().ok()?.is_file() {
        return None;
    }
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Paths in `dir` that differ from what its marker recorded: edited, deleted or added files.
/// `None` if that is unknown: an old marker without file ids, or ids in a format (SHA-256)
/// that needs `repo`, the package's cached mirror, which is not there.
fn local_changes(dir: &Path, marker: &Marker, repo: Option<&Repo>) -> Result<Option<Vec<String>>> {
    let Some(recorded) = &marker.files else {
        return Ok(None);
    };
    if repo.is_none() && recorded.values().any(|oid| oid.len() != 40) {
        return Ok(None);
    }
    let mut on_disk = Vec::new();
    let mut changed = Vec::new();
    walk(dir, "", &mut on_disk, &mut changed)?;
    on_disk.retain(|path| path != MARKER_FILE);
    let mut common = Vec::new();
    for path in on_disk {
        if recorded.contains_key(&path) {
            common.push(path);
        } else {
            changed.push(path); // added
        }
    }
    let deleted: Vec<String> = recorded
        .keys()
        .filter(|path| !common.contains(path) && !changed.contains(path))
        .cloned()
        .collect();
    changed.extend(deleted);
    let paths: Vec<&str> = common.iter().map(String::as_str).collect();
    let oids = match repo {
        Some(repo) => repo.hash_files(dir, &paths)?,
        None => crate::git::hash_files(None, dir, &paths)?,
    };
    for (path, oid) in common.iter().zip(oids) {
        if recorded[path] != oid {
            changed.push(path.clone());
        }
    }
    changed.sort();
    Ok(Some(changed))
}

/// Collects the `/`-separated paths of regular files under `dir` into `files`. Anything else
/// (a symbolic link) goes to `other`: aip never writes those.
fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>, other: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).context(|| format!("cannot read {}", dir.display()))?;
    for entry in entries {
        let entry = entry.context(|| format!("cannot read {}", dir.display()))?;
        let path = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let kind = entry
            .file_type()
            .context(|| format!("cannot read {}", entry.path().display()))?;
        if kind.is_dir() {
            walk(&entry.path(), &format!("{path}/"), files, other)?;
        } else if kind.is_file() {
            files.push(path);
        } else {
            other.push(path);
        }
    }
    Ok(())
}

/// Refuses to overwrite or delete aip's folder `dest` when files in it changed since aip wrote
/// it, unless `force`. `url` is the repository of the package `found` names.
fn guard_changes(
    dest: &Path,
    found: &Marker,
    url: &str,
    resolver: &mut Resolver,
    force: bool,
) -> Result<()> {
    if force {
        return Ok(());
    }
    let repo = resolver.mirror(url)?;
    match local_changes(dest, found, repo)? {
        Some(changed) if !changed.is_empty() => Err(Error::new(format!(
            "{} has local changes ({}); use --force to discard them",
            dest.display(),
            files_label(changed.len())
        ))),
        _ => Ok(()),
    }
}

/// Every package needs its own skill folder. Two skills with the same name clash; `"as"` in
/// aip.json installs one under another name; the hint suggests it for a package new in `lock`.
fn unique_folders(manifest: &Manifest, old: &Lock, lock: &Lock) -> Result<()> {
    let mut seen = HashMap::new();
    for pkg in &lock.packages {
        let folder = folder_of(pkg)?;
        let Some(other) = seen.insert(folder, pkg.name.as_str()) else {
            continue;
        };
        let (first, second) = if old.get(other).is_none() {
            (pkg.name.as_str(), other)
        } else {
            (other, pkg.name.as_str())
        };
        let (repository, skill) = registry::split_package(second)?;
        let suggest = format!(
            "{}-{skill}",
            repository.rsplit('/').next().unwrap_or(repository)
        );
        let version = manifest
            .require
            .get(second)
            .map_or("*", |r| r.version.as_str());
        return Err(Error::new(format!(
            "{first} and {second} both install a skill named '{folder}'\nhint: add \"as\" to one of them in {MANIFEST_FILE}, e.g. \"{second}\": {{\"version\": \"{version}\", \"as\": \"{suggest}\"}}"
        )));
    }
    Ok(())
}

struct Plan {
    changes: Vec<Change>,
    pruned: Vec<(String, PathBuf)>,
    /// Packages with the skill folders (parents) to write them into.
    writes: Vec<(LockedPackage, Vec<PathBuf>)>,
    /// Skill folders to delete.
    deletes: Vec<PathBuf>,
}

/// Works out what to change on disk, refusing to overwrite skills aip does not manage, and
/// (unless `force`) to overwrite or delete its own folders that have local changes.
/// `others` are skill folders of providers not in aip.json.
fn plan(
    old: &Lock,
    new: &Lock,
    targets: &[PathBuf],
    others: &[PathBuf],
    resolver: &mut Resolver,
    force: bool,
) -> Result<Plan> {
    // Folders written in this run; none of them may be deleted afterwards.
    let mut written = HashSet::new();
    let mut plan = Plan {
        changes: Vec::new(),
        pruned: Vec::new(),
        writes: Vec::new(),
        deletes: Vec::new(),
    };
    for pkg in &old.packages {
        if new.get(&pkg.name).is_some() {
            continue;
        }
        folder_of(pkg)?;
        plan.changes.push(Change::Remove {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
        });
    }
    for pkg in &new.packages {
        let folder = folder_of(pkg)?;
        for dir in others {
            let dest = dir.join(folder);
            if let Some(found) = marker(&dest).filter(|m| m.name == pkg.name) {
                guard_changes(&dest, &found, &pkg.source.url, resolver, force)?;
                plan.deletes.push(dest);
                plan.pruned.push((pkg.name.clone(), dir.clone()));
            }
        }
        // Old entries that may own the skill folder: this package, or one that leaves the folder
        // in this run (dropped from the lock, a rename, or given another `"as"`) and installed
        // it from the same repository.
        let mut owners = Vec::new();
        for o in &old.packages {
            let leaves = match new.get(&o.name) {
                None => true,
                Some(n) => folder_of(n)? != folder,
            };
            if (o.name == pkg.name || leaves)
                && o.source.url == pkg.source.url
                && folder_of(o)? == folder
            {
                owners.push(o);
            }
        }
        let mut stale = Vec::new();
        for target in targets {
            let dest = target.join(folder);
            let found = marker(&dest);
            if let Some(m) = found.as_ref().filter(|m| m.name == pkg.name) {
                if m.reference != pkg.source.reference {
                    guard_changes(&dest, m, &pkg.source.url, resolver, force)?;
                    stale.push(target.clone());
                }
                continue;
            }
            if dest.symlink_metadata().is_err() {
                stale.push(target.clone());
                continue;
            }
            let mut owner = None;
            for o in &owners {
                let owns = match &found {
                    Some(m) => m.name == o.name,
                    None => installed_before_markers(&dest, o, pkg, resolver)?,
                };
                if owns {
                    owner = Some(o);
                    break;
                }
            }
            match (owner, &found) {
                (Some(o), Some(m)) => {
                    guard_changes(&dest, m, &o.source.url, resolver, force)?;
                    stale.push(target.clone());
                }
                (Some(_), None) => stale.push(target.clone()),
                (None, _) if force => stale.push(target.clone()),
                (None, _) => {
                    return Err(Error::new(format!(
                        "refusing to overwrite {}: it was not installed by aip (use --force to replace it)",
                        dest.display()
                    )));
                }
            }
        }
        if stale.is_empty() {
            continue;
        }
        written.extend(stale.iter().map(|t| t.join(folder)));
        let prev = old.get(&pkg.name);
        let moved = match prev {
            Some(prev) => Some(folder_of(prev)?).filter(|f| *f != folder),
            None => None,
        };
        plan.changes.push(match prev {
            Some(prev) if prev.source != pkg.source || moved.is_some() => {
                let (from, to) = update_labels(prev, pkg);
                Change::Update {
                    name: pkg.name.clone(),
                    from,
                    to,
                    folder: moved.map(|f| (f.to_string(), folder.to_string())),
                }
            }
            _ => Change::Install {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            },
        });
        plan.writes.push((pkg.clone(), stale));
    }
    // Folders that old packages leave: dropped from the lock, or moved by a new `"as"`.
    for pkg in &old.packages {
        let folder = folder_of(pkg)?;
        if let Some(n) = new.get(&pkg.name)
            && folder_of(n)? == folder
        {
            continue;
        }
        for dir in targets.iter().chain(others) {
            let dest = dir.join(folder);
            if written.contains(&dest) {
                continue; // another package takes this folder over
            }
            if let Some(found) = marker(&dest).filter(|m| m.name == pkg.name) {
                guard_changes(&dest, &found, &pkg.source.url, resolver, force)?;
                plan.deletes.push(dest);
            } else if targets.contains(dir) && dest.symlink_metadata().is_ok() {
                eprintln!(
                    "warning: not removing {}: it was not installed by aip",
                    dest.display()
                );
            }
        }
    }
    Ok(plan)
}

/// Migration for folders installed before aip wrote markers: a folder without any marker
/// counts as `prev`'s when its SKILL.md is the one at the commit `prev` locked. `prev` is the old
/// lock entry for the same skill and repository as `pkg`. Used to overwrite, never to delete.
// ponytail: SKILL.md comparison only; drop this once installs without markers are gone.
fn installed_before_markers(
    dest: &Path,
    prev: &LockedPackage,
    pkg: &LockedPackage,
    resolver: &mut Resolver,
) -> Result<bool> {
    if dest.join(MARKER_FILE).symlink_metadata().is_ok()
        || !dest.symlink_metadata().is_ok_and(|m| m.is_dir())
    {
        return Ok(false);
    }
    folder_of(prev)?;
    let repo = resolver.locked_repo(pkg)?;
    if !repo.has_commit(&prev.source.reference) {
        return Ok(false);
    }
    let Some(expected) = repo.read(&prev.source.reference, &format!("{}/SKILL.md", prev.path))?
    else {
        return Ok(false);
    };
    Ok(fs::read_to_string(dest.join("SKILL.md")).is_ok_and(|actual| actual == expected))
}

fn staging_path(dir: &Path, folder: &str) -> PathBuf {
    dir.join(format!(".{folder}.aip-tmp"))
}

/// Exports every write next to its destination, with its marker. Records each staging folder
/// in `staged` as `(dir, folder)` before writing it, so the caller can clean up after a failure.
fn stage(
    writes: &[(LockedPackage, Vec<PathBuf>)],
    resolver: &mut Resolver,
    staged: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    for (pkg, dirs) in writes {
        let folder = folder_of(pkg)?;
        let repo = resolver.locked_repo(pkg)?;
        for dir in dirs {
            let staging = staging_path(dir, folder);
            delete(&staging)?;
            staged.push((dir.clone(), folder.to_string()));
            let mut files = repo.export(&pkg.source.reference, &pkg.path, &staging)?;
            if let Some(alias) = &pkg.alias {
                // The marker records what is on disk, so the rewritten file is not a local change.
                let path = staging.join("SKILL.md");
                let text = fs::read_to_string(&path)
                    .context(|| format!("cannot read {}", path.display()))?;
                fs::write(&path, rename_skill(&text, alias))
                    .context(|| format!("cannot write {}", path.display()))?;
                let oid = repo.hash_files(&staging, &["SKILL.md"])?.remove(0);
                files.insert("SKILL.md".into(), oid);
            }
            let marker = Marker {
                name: pkg.name.clone(),
                reference: pkg.source.reference.clone(),
                files: Some(files),
            };
            manifest::write_json(&staging.join(MARKER_FILE), &marker)?;
        }
    }
    Ok(())
}

/// `skill_md` with `name:` in its frontmatter set to `alias`, so the harness lists the skill
/// under its folder name. Adds the line, or a frontmatter block, if missing.
// ponytail: line-based like `registry::description`; only the first frontmatter block is touched.
fn rename_skill(skill_md: &str, alias: &str) -> String {
    let name = format!("name: {alias}");
    let mut lines = skill_md.split_inclusive('\n');
    let Some(first) = lines.next().filter(|l| l.trim() == "---") else {
        return format!("---\n{name}\n---\n{skill_md}");
    };
    let eol = if first.ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = format!("{first}{name}{eol}");
    let mut in_frontmatter = true;
    for line in lines {
        if in_frontmatter && line.trim() == "---" {
            in_frontmatter = false;
        } else if in_frontmatter && line.starts_with("name:") {
            continue; // replaced by the line above
        }
        out.push_str(line);
    }
    out
}

/// Replaces `dir/folder` with its staged export: the old folder is moved aside first, so the
/// destination is never a mix of both.
fn swap(dir: &Path, folder: &str) -> Result<()> {
    let dest = dir.join(folder);
    let aside = dir.join(format!(".{folder}.aip-old"));
    delete(&aside)?;
    if dest.symlink_metadata().is_ok() {
        fs::rename(&dest, &aside).context(|| format!("cannot replace {}", dest.display()))?;
    }
    fs::rename(staging_path(dir, folder), &dest)
        .context(|| format!("cannot write {}", dest.display()))?;
    delete(&aside)
}

/// Version labels for an update; equal versions (`dev-main`, or a re-pointed source) get the
/// short commit added, like Composer: `dev-main 1a2b3c4 => dev-main 5d6e7f8`.
fn update_labels(prev: &LockedPackage, next: &LockedPackage) -> (String, String) {
    if prev.version != next.version || prev.source == next.source {
        return (prev.version.clone(), next.version.clone());
    }
    let label = |p: &LockedPackage| format!("{} {}", p.version, short_sha(&p.source.reference));
    (label(prev), label(next))
}

/// Removes a file, symlink or folder if it exists.
fn delete(path: &Path) -> Result<()> {
    let Ok(meta) = path.symlink_metadata() else {
        return Ok(());
    };
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .context(|| format!("cannot remove {}", path.display()))
}

/// Skill folder name of a locked package (its `"as"`, else the skill's name), after validating
/// the entry: lock files are user-editable input, and `reference` is passed to git as a revision.
fn folder_of(pkg: &LockedPackage) -> Result<&str> {
    let (_, skill) = registry::split_package(&pkg.name)?;
    if let Some(alias) = pkg.alias.as_deref().filter(|a| !registry::valid_name(a)) {
        return Err(Error::new(format!(
            "invalid {LOCK_FILE}: package {} has invalid \"as\" '{alias}'",
            pkg.name
        )));
    }
    if pkg.path != format!("skills/{skill}") {
        return Err(Error::new(format!(
            "invalid {LOCK_FILE}: package {} has path '{}', expected 'skills/{skill}'",
            pkg.name, pkg.path
        )));
    }
    let reference = &pkg.source.reference;
    if pkg.source.kind != "git"
        || !matches!(reference.len(), 40 | 64)
        || !reference.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::new(format!(
            "invalid {LOCK_FILE}: package {} needs a git source with a commit SHA reference",
            pkg.name
        )));
    }
    Ok(pkg.alias.as_deref().unwrap_or(skill))
}

/// Highest stable version of `skill`, else the default branch head: `(version label, commit sha)`.
fn latest(repo: &Repo, skill: &str) -> Result<(String, String)> {
    match newest_with_skill(repo, skill, |v| v.pre.is_empty())? {
        Some(found) => Ok((found.version.to_string(), found.sha)),
        None => {
            let branch = repo.default_branch()?;
            let sha = repo
                .resolve(&format!("refs/heads/{branch}"))?
                .ok_or_else(|| Error::new(format!("default branch '{branch}' has no commits")))?;
            Ok((format!("dev-{branch}"), sha))
        }
    }
}

/// A version of a skill: its commit and SKILL.md at that commit.
struct Found {
    version: Version,
    sha: String,
    skill_md: String,
}

/// The newest version accepted by `accept` whose tag contains the skill. A repository-wide tag
/// says nothing about a skill added after it, so tags are tried newest first and the search
/// stops at the first one with `skills/<skill>/SKILL.md`.
fn newest_with_skill(
    repo: &Repo,
    skill: &str,
    accept: impl Fn(&Version) -> bool,
) -> Result<Option<Found>> {
    let mut candidates: Vec<_> = versions(repo, skill)?
        .into_iter()
        .filter(|(v, _)| accept(v))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    for (version, tag) in candidates {
        let path = format!("skills/{skill}/SKILL.md");
        if let Some(skill_md) = repo.read(&format!("refs/tags/{tag}"), &path)? {
            let sha = tag_sha(repo, &tag)?;
            return Ok(Some(Found {
                version,
                sha,
                skill_md,
            }));
        }
    }
    Ok(None)
}

/// Candidate versions of `skill` with the tag naming each, whether or not the tag contains the
/// skill (see [`newest_with_skill`]). Tags of the form `<skill>/<version>` win; without any, the
/// repository's own version tags (those without a `/`) apply to all skills.
fn versions(repo: &Repo, skill: &str) -> Result<Vec<(Version, String)>> {
    let tags = repo.tags()?;
    let prefix = format!("{skill}/");
    let own: Vec<_> = tags
        .iter()
        .filter_map(|tag| {
            let version = constraint::parse_version(tag.strip_prefix(&prefix)?)?;
            Some((version, tag.clone()))
        })
        .collect();
    if !own.is_empty() {
        return Ok(own);
    }
    Ok(tags
        .into_iter()
        .filter(|tag| !tag.contains('/'))
        .filter_map(|tag| constraint::parse_version(&tag).map(|v| (v, tag)))
        .collect())
}

/// True if the locked commit is what its version label says: the commit of a tag the resolver
/// would pick for `X`, or a commit on branch `b` for `dev-b`. Lock files are untrusted input;
/// without this a tampered lock could label any commit of the repository `1.2.0`.
fn lock_matches(repo: &Repo, pkg: &LockedPackage) -> Result<bool> {
    let reference = &pkg.source.reference;
    if !repo.has_commit(reference) {
        return Ok(false);
    }
    if let Some(branch) = pkg.version.strip_prefix("dev-") {
        let head = format!("refs/heads/{branch}");
        return Ok(repo.resolve(&head)?.is_some() && repo.is_ancestor(reference, &head)?);
    }
    let Some(version) = constraint::parse_version(&pkg.version) else {
        return Ok(false);
    };
    let (_, skill) = registry::split_package(&pkg.name)?;
    for (v, tag) in versions(repo, skill)? {
        if v == version && tag_sha(repo, &tag)? == *reference {
            return Ok(true);
        }
    }
    Ok(false)
}

fn tag_sha(repo: &Repo, tag: &str) -> Result<String> {
    repo.resolve(&format!("refs/tags/{tag}"))?
        .ok_or_else(|| Error::new(format!("tag '{tag}' does not point to a commit")))
}

/// Resolves package names against the registry; fetches each repository at most once per run.
struct Resolver {
    registries: Registries,
    cache: PathBuf,
    repos: HashMap<String, Repo>,
    /// URLs fetched in this run; other entries of `repos` are cached mirrors opened offline.
    fetched: HashSet<String>,
    /// Package name to the URL aip.lock has for it; those lookups tolerate a broken
    /// higher-priority registry when a lower one lists the locked URL.
    locked_urls: HashMap<String, String>,
}

impl Resolver {
    fn new(project: &Project, manifest: &Manifest) -> Self {
        Self {
            registries: project.registries(Some(manifest)),
            cache: project.aip_home.join("cache").join("repos"),
            repos: HashMap::new(),
            fetched: HashSet::new(),
            locked_urls: HashMap::new(),
        }
    }

    fn registries(&self) -> &Registries {
        &self.registries
    }

    // ponytail: progress goes to stderr from the library; route through a callback when the GUI needs it.
    fn repo(&mut self, url: &str) -> Result<&Repo> {
        if !self.fetched.contains(url) {
            eprintln!("Fetching {url}");
            let repo = Repo::sync(url, &self.cache)?;
            self.repos.insert(url.to_string(), repo);
            self.fetched.insert(url.to_string());
        }
        Ok(&self.repos[url])
    }

    /// The repository holding a locked package, checked against its version label (see
    /// [`lock_matches`]). Uses the cached mirror without fetching when it already has the
    /// commit, so `aip install` from a lock works offline.
    fn locked_repo(&mut self, pkg: &LockedPackage) -> Result<&Repo> {
        let url = pkg.source.url.as_str();
        let mut matches = match self.mirror(url)? {
            Some(repo) => lock_matches(repo, pkg)?,
            None => false,
        };
        if !matches && !self.fetched.contains(url) {
            matches = lock_matches(self.repo(url)?, pkg)?;
        }
        if !matches {
            let what = match pkg.version.strip_prefix("dev-") {
                Some(branch) => format!("on branch {branch}"),
                None => format!("tagged {}", pkg.version),
            };
            return Err(Error::new(format!(
                "{LOCK_FILE}: {name} {version} is locked to commit {short}, which is not {what} in {url}; run `aip update {name}`",
                name = pkg.name,
                version = pkg.version,
                short = short_sha(&pkg.source.reference),
            )));
        }
        Ok(&self.repos[url])
    }

    /// The repository of `url` as already synced or cached, without fetching; `None` if it was
    /// never fetched.
    fn mirror(&mut self, url: &str) -> Result<Option<&Repo>> {
        if !self.repos.contains_key(url)
            && let Some(repo) = Repo::open(url, &self.cache)?
        {
            self.repos.insert(url.to_string(), repo);
        }
        Ok(self.repos.get(url))
    }

    fn url_of(&mut self, name: &str) -> Result<String> {
        let (repository, _) = registry::split_package(name)?;
        let entry = match self.locked_urls.get(name) {
            Some(locked) => self.registries.get_locked(repository, locked)?,
            None => self.registries.get(repository)?,
        };
        Ok(entry.url)
    }

    /// Constraint for `aip install <name>` without one: `^major.minor` of the latest stable
    /// version, or `dev-<default branch>` for repositories without version tags.
    fn default_constraint(&mut self, name: &str) -> Result<String> {
        let (_, skill) = registry::split_package(name)?;
        let url = self.url_of(name)?;
        let repo = self.repo(&url)?;
        match newest_with_skill(repo, skill, |v| v.pre.is_empty())? {
            Some(found) => Ok(constraint::default_for(&found.version)),
            None => Ok(format!("dev-{}", repo.default_branch()?)),
        }
    }

    fn resolve(&mut self, name: &str, constraint: &str) -> Result<LockedPackage> {
        let (_, skill) = registry::split_package(name)?;
        let parsed =
            Constraint::parse(constraint).map_err(|e| Error::new(format!("{name}: {e}")))?;
        let url = self.url_of(name)?;
        let repo = self.repo(&url)?;
        let path = format!("skills/{skill}");
        let (version, sha, skill_md) = match &parsed {
            Constraint::Branch {
                name: branch,
                commit,
            } => {
                let head = format!("refs/heads/{branch}");
                let tip = repo.resolve(&head)?.ok_or_else(|| {
                    Error::new(format!("{name}: branch '{branch}' not found in {url}"))
                })?;
                let sha = match commit {
                    None => tip,
                    Some(hex) => {
                        let sha = repo.resolve(hex)?.ok_or_else(|| {
                            Error::new(format!("{name}: commit {hex} not found in {url}"))
                        })?;
                        if !repo.is_ancestor(&sha, &head)? {
                            return Err(Error::new(format!(
                                "{name}: commit {} is not on branch {branch}",
                                short_sha(&sha)
                            )));
                        }
                        sha
                    }
                };
                let version = format!("dev-{branch}");
                let skill_md = repo
                    .read(&sha, &format!("{path}/SKILL.md"))?
                    .ok_or_else(|| {
                        Error::new(format!(
                            "{name}: skill '{skill}' not found in {url} at {version} (expected {path}/SKILL.md)"
                        ))
                    })?;
                (version, sha, skill_md)
            }
            Constraint::Range { .. } => {
                let Some(found) = newest_with_skill(repo, skill, |v| parsed.matches(v))? else {
                    let mut all: Vec<_> =
                        versions(repo, skill)?.into_iter().map(|(v, _)| v).collect();
                    all.sort();
                    if all.iter().any(|v| parsed.matches(v)) {
                        return Err(Error::new(format!(
                            "{name}: skill '{skill}' not found in {url} at any version matching '{constraint}' (expected {path}/SKILL.md)"
                        )));
                    }
                    let available: Vec<_> = all.iter().map(Version::to_string).collect();
                    return Err(Error::new(format!(
                        "{name}: no version matches '{constraint}' (available: {})",
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
                    )));
                };
                (found.version.to_string(), found.sha, found.skill_md)
            }
        };
        Ok(LockedPackage {
            name: name.to_string(),
            version,
            description: registry::description(&skill_md),
            source: Source::git(url, sha),
            path,
            alias: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const FORCE: Options = Options {
        force: true,
        dry_run: false,
    };

    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        project: Project,
    }

    impl Fixture {
        /// Skill repo with tags v1.0.0, v1.1.0, v2.0.0 and a project using Claude Code.
        fn new(name: &str) -> Self {
            let root = env::temp_dir().join(format!("aip-ops-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let repo = root.join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "--quiet", "--initial-branch=main"]);
            for version in ["1.0.0", "1.1.0", "2.0.0"] {
                write_skill(&repo, "hello", version);
                commit(&repo, version);
                git(&repo, &["tag", &format!("v{version}")]);
            }
            fs::write(
                root.join("registry.json"),
                r#"{"repositories": [{"name": "demo", "url": "repo"}]}"#,
            )
            .unwrap();
            let dir = root.join("project");
            fs::create_dir_all(dir.join(".claude")).unwrap();
            fs::write(
                dir.join(MANIFEST_FILE),
                r#"{"registries": [{"type": "json", "path": "../registry.json"}]}"#,
            )
            .unwrap();
            let project = Project::local(dir, root.join("aip-home"));
            Self {
                root,
                repo,
                project,
            }
        }

        fn skill_dir(&self) -> PathBuf {
            self.project.root.join(".claude/skills/hello")
        }

        fn lock(&self) -> Lock {
            Lock::read(&self.project.lock_path).unwrap()
        }

        fn manifest(&self) -> Manifest {
            Manifest::read(&self.project.manifest_path)
                .unwrap()
                .unwrap()
        }
    }

    #[test]
    fn init_writes_given_providers_and_refuses_overwrite() {
        let f = Fixture::new("init");
        fs::remove_file(&f.project.manifest_path).unwrap();
        let dirs = init(&f.project, vec!["codex".into(), "codex".into()]).unwrap();
        assert_eq!(dirs, [f.project.root.join(".agents/skills")]);
        assert_eq!(f.manifest().providers, ["codex"]);
        assert!(
            init(&f.project, vec![])
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
    }

    #[test]
    fn init_detects_or_rejects_providers() {
        let f = Fixture::new("init-detect");
        fs::remove_file(&f.project.manifest_path).unwrap();
        let err = init(&f.project, vec!["nope".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown provider 'nope'"), "{err}");
        assert!(!f.project.manifest_path.exists());
        init(&f.project, vec![]).unwrap(); // `.claude/` exists in the fixture
        assert_eq!(f.manifest().providers, ["claude-code"]);
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=aip", "-c", "user.email=aip@example.com"])
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit(repo: &Path, message: &str) {
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "--quiet", "-m", message]);
    }

    fn write_skill(repo: &Path, skill: &str, version: &str) {
        let dir = repo.join("skills").join(skill);
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: Says hello\n---\nversion {version}\n"),
        )
        .unwrap();
        let script = dir.join("scripts/run.sh");
        fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn installed_version(f: &Fixture) -> String {
        fs::read_to_string(f.skill_dir().join("SKILL.md")).unwrap()
    }

    #[test]
    fn install_uses_name_derived_from_url() {
        let f = Fixture::new("derived-name");
        fs::write(
            f.root.join("registry.json"),
            r#"{"repositories": [{"url": "repo"}]}"#,
        )
        .unwrap();
        let report = install(&f.project, &["repo/hello".into()], Options::default()).unwrap();
        assert_eq!(
            report.changes,
            [Change::Install {
                name: "repo/hello".into(),
                version: "2.0.0".into()
            }]
        );
        assert!(f.skill_dir().exists());
    }

    #[test]
    fn install_resolves_constraint_and_writes_files() {
        let f = Fixture::new("install");
        let report = install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        assert_eq!(
            report.changes,
            [Change::Install {
                name: "demo/hello".into(),
                version: "1.1.0".into()
            }]
        );
        assert!(installed_version(&f).contains("version 1.1.0"));
        let manifest = f.manifest();
        assert_eq!(manifest.providers, ["claude-code"]);
        assert_eq!(manifest.require["demo/hello"].version, "^1.0");
        let lock = f.lock();
        assert_eq!(lock.packages[0].version, "1.1.0");
        assert_eq!(lock.packages[0].description.as_deref(), Some("Says hello"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(f.skill_dir().join("scripts/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }

        // Second run is a no-op; deleted files are restored from the lock.
        assert!(
            install(&f.project, &[], Options::default())
                .unwrap()
                .changes
                .is_empty()
        );
        fs::remove_dir_all(f.skill_dir()).unwrap();
        let report = install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert!(installed_version(&f).contains("version 1.1.0"));
    }

    #[test]
    fn default_constraint_is_caret_of_latest() {
        let f = Fixture::new("default");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert_eq!(f.manifest().require["demo/hello"].version, "^2.0");
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn update_moves_to_newest_allowed_version() {
        let f = Fixture::new("update");
        install(&f.project, &["demo/hello:~1.0".into()], Options::default()).unwrap();
        write_skill(&f.repo, "hello", "1.2.0");
        commit(&f.repo, "1.2.0");
        git(&f.repo, &["tag", "v1.2.0"]);

        // install keeps the locked version; update picks up the new tag.
        assert!(
            install(&f.project, &[], Options::default())
                .unwrap()
                .changes
                .is_empty()
        );
        let report = update(&f.project, &[], Options::default()).unwrap();
        assert_eq!(
            report.changes,
            [Change::Update {
                name: "demo/hello".into(),
                from: "1.1.0".into(),
                to: "1.2.0".into(),
                folder: None,
            }]
        );
        assert_eq!(
            report.changes[0].to_string(),
            "Upgrading demo/hello (1.1.0 => 1.2.0)"
        );
        assert!(installed_version(&f).contains("version 1.2.0"));
        assert!(update(&f.project, &["demo/nope".into()], Options::default()).is_err());
    }

    #[test]
    fn branch_constraint_pins_commit() {
        let f = Fixture::new("branch");
        write_skill(&f.repo, "hello", "main-head");
        commit(&f.repo, "main head");
        install(
            &f.project,
            &["demo/hello:dev-main".into()],
            Options::default(),
        )
        .unwrap();
        let lock = f.lock();
        assert_eq!(lock.packages[0].version, "dev-main");
        assert_eq!(lock.packages[0].source.reference.len(), 40);
        assert!(installed_version(&f).contains("version main-head"));

        // A new commit on the branch updates with short commits in the label.
        let old_sha = lock.packages[0].source.reference.clone();
        write_skill(&f.repo, "hello", "main-next");
        commit(&f.repo, "main next");
        let report = update(&f.project, &[], Options::default()).unwrap();
        let new_sha = f.lock().packages[0].source.reference.clone();
        assert_eq!(
            report.changes[0].to_string(),
            format!(
                "Updating demo/hello (dev-main {} => dev-main {})",
                &old_sha[..7],
                &new_sha[..7]
            )
        );
        assert!(installed_version(&f).contains("version main-next"));
    }

    #[test]
    fn refuses_to_overwrite_unmanaged_skill() {
        let f = Fixture::new("guard");
        fs::create_dir_all(f.skill_dir()).unwrap();
        fs::write(f.skill_dir().join("SKILL.md"), "mine").unwrap();
        let err = install(&f.project, &["demo/hello".into()], Options::default()).unwrap_err();
        assert!(err.to_string().contains("not installed by aip"), "{err}");
        assert_eq!(installed_version(&f), "mine");
        assert!(!f.project.lock_path.exists());

        install(&f.project, &["demo/hello".into()], FORCE).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn dropped_requirement_is_removed() {
        let f = Fixture::new("remove");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let mut manifest = f.manifest();
        manifest.require.clear();
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let report = install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(
            report.changes,
            [Change::Remove {
                name: "demo/hello".into(),
                version: "2.0.0".into()
            }]
        );
        assert!(!f.skill_dir().exists());
        assert!(f.lock().packages.is_empty());
    }

    #[test]
    fn rejects_tampered_lock_reference() {
        let f = Fixture::new("tampered");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let mut lock = f.lock();
        lock.packages[0].source.reference = "--output=/tmp/x".into();
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();
        let err = install(&f.project, &[], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("commit SHA"), "{err}");
    }

    #[test]
    fn readable_errors() {
        let f = Fixture::new("errors");
        let msg = |spec: &str| {
            install(&f.project, &[spec.into()], Options::default())
                .unwrap_err()
                .to_string()
        };
        assert!(msg("hello").contains("invalid package name"));
        assert!(msg("other/hello").contains("unknown repository 'other'"));
        assert!(msg("demo/missing").contains("skill 'missing' not found"));
        assert!(msg("demo/hello:^9.0").contains("available: 1.0.0, 1.1.0, 2.0.0"));
        assert!(msg("demo/hello:abc").contains("invalid version constraint"));
    }

    #[test]
    fn search_lists_matching_skills() {
        let f = Fixture::new("search");
        let hits = search(&f.project, "HELLO").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "demo/hello");
        assert_eq!(hits[0].version, "2.0.0");
        assert!(search(&f.project, "says").unwrap().len() == 1);
        assert!(search(&f.project, "zzz").unwrap().is_empty());
    }

    /// Adds skill `other`, versioned only through `other/v3.0.0` and `other/v3.1.0`.
    fn add_other(f: &Fixture) {
        for version in ["3.0.0", "3.1.0"] {
            write_skill(&f.repo, "other", version);
            commit(&f.repo, &format!("other {version}"));
            git(&f.repo, &["tag", &format!("other/v{version}")]);
        }
    }

    #[test]
    fn skill_tags_version_a_skill_on_its_own() {
        let f = Fixture::new("per-skill");
        add_other(&f);
        install(
            &f.project,
            &["demo/other".into(), "demo/hello".into()],
            Options::default(),
        )
        .unwrap();
        assert_eq!(f.manifest().require["demo/other"].version, "^3.1");
        assert_eq!(f.manifest().require["demo/hello"].version, "^2.0");
        let lock = f.lock();
        assert_eq!(lock.get("demo/other").unwrap().version, "3.1.0");
        assert_eq!(lock.get("demo/hello").unwrap().version, "2.0.0");
        let err = install(&f.project, &["demo/other:^1.0".into()], Options::default()).unwrap_err();
        assert!(err.to_string().contains("available: 3.0.0, 3.1.0"), "{err}");

        let hits = search(&f.project, "other").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].version, "3.1.0");
        let hello = search(&f.project, "hello").unwrap();
        assert_eq!(
            hello
                .iter()
                .find(|h| h.name == "demo/hello")
                .unwrap()
                .version,
            "2.0.0"
        );
    }

    #[test]
    fn remove_deletes_skill_and_requirement() {
        let f = Fixture::new("remove-cmd");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert!(f.skill_dir().exists());
        let report = remove(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert_eq!(
            report.changes,
            [Change::Remove {
                name: "demo/hello".into(),
                version: "2.0.0".into()
            }]
        );
        assert!(!f.skill_dir().exists());
        assert!(f.lock().packages.is_empty());
        assert!(f.manifest().require.is_empty());
        let err = remove(&f.project, &["demo/hello".into()], Options::default()).unwrap_err();
        assert!(
            err.to_string()
                .contains("package 'demo/hello' is not required"),
            "{err}"
        );
    }

    #[test]
    fn registries_resolve_in_priority_order() {
        let f = Fixture::new("registries");
        fs::write(
            f.project.root.join("../second.json"),
            r#"{"repositories": [{"name": "demo", "url": "elsewhere"}, {"name": "more", "url": "repo"}]}"#,
        )
        .unwrap();
        let mut manifest = f.manifest();
        manifest
            .registries
            .push(crate::registry::RegistryConfig::Json(
                crate::registry::JsonSource::Path("../second.json".into()),
            ));
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        // `more` only exists in the second registry.
        install(&f.project, &["more/hello".into()], Options::default()).unwrap();
        let url = f.lock().get("more/hello").unwrap().source.url.clone();
        assert!(url.ends_with("repo"), "{url}");
        let registries = f.project.registries(Some(&f.manifest()));
        assert!(registries.get("demo").unwrap().url.ends_with("repo"));
    }

    #[test]
    fn broken_registry_only_blocks_new_resolutions() {
        let f = Fixture::new("broken-registry");
        fs::write(
            f.root.join("registry.json"),
            r#"{"repositories": [{"name": "demo", "url": "repo"}, {"name": "new", "url": "repo"}]}"#,
        )
        .unwrap();
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        fs::write(f.root.join("broken.json"), "{not json").unwrap();
        let mut manifest = f.manifest();
        manifest.registries.insert(
            0,
            crate::registry::RegistryConfig::Json(crate::registry::JsonSource::Path(
                "../broken.json".into(),
            )),
        );
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();

        // Locked from the second registry: warns and proceeds.
        install(&f.project, &[], Options::default()).unwrap();
        assert!(f.skill_dir().exists());
        assert!(!matches!(check_one(&f).status, Status::Error { .. }));

        // A new resolution would let the broken registry's higher priority go unnoticed.
        let err = install(&f.project, &["new/hello".into()], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("has priority over"), "{err}");
    }

    #[test]
    fn rejects_lock_url_that_differs_from_registry() {
        let f = Fixture::new("lock-url");
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        let other = f.root.join("other-repo");
        fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "--quiet"]);
        let mut lock = f.lock();
        lock.packages[0].source.url = other.display().to_string();
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();

        let err = install(&f.project, &[], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("run `aip update demo/hello`"), "{err}");
        assert!(!f.skill_dir().exists());
        assert_eq!(f.lock().packages[0].source.url, other.display().to_string());

        update(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 1.1.0"));
        assert_ne!(f.lock().packages[0].source.url, other.display().to_string());
    }

    #[test]
    fn changed_constraint_requires_update() {
        let f = Fixture::new("stale-constraint");
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "^2.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let lock_before = fs::read_to_string(&f.project.lock_path).unwrap();

        let err = install(&f.project, &[], Options::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "aip.lock is out of date with aip.json:\n  - demo/hello: locked 1.1.0 does not satisfy ^2.0\nRun `aip update demo/hello` to resolve it."
        );
        assert!(installed_version(&f).contains("version 1.1.0"));
        assert_eq!(
            fs::read_to_string(&f.project.lock_path).unwrap(),
            lock_before
        );

        update(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn requirement_missing_from_lock_requires_naming_it() {
        let f = Fixture::new("not-locked");
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "^1.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        // An existing lock that does not cover the requirement keeps install strict.
        manifest::write_json(&f.project.lock_path, &Lock::default()).unwrap();

        let err = install(&f.project, &[], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("demo/hello: not in aip.lock"), "{err}");
        assert!(f.lock().packages.is_empty());

        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 1.1.0"));
    }

    #[test]
    fn install_without_lock_resolves_requirements() {
        let f = Fixture::new("fresh-clone");
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "^1.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        assert!(!f.project.lock_path.exists());

        let report = install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert_eq!(f.lock().get("demo/hello").unwrap().version, "1.1.0");
        assert!(installed_version(&f).contains("version 1.1.0"));
    }

    /// Adds skill `late` after the last repository tag (`v2.0.0`).
    fn add_late(f: &Fixture) {
        write_skill(&f.repo, "late", "head");
        commit(&f.repo, "late");
    }

    #[test]
    fn tags_without_the_skill_do_not_version_it() {
        let f = Fixture::new("late-skill");
        add_late(&f);
        install(&f.project, &["demo/late".into()], Options::default()).unwrap();
        assert_eq!(f.manifest().require["demo/late"].version, "dev-main");
        assert_eq!(f.lock().get("demo/late").unwrap().version, "dev-main");

        let hit = search(&f.project, "late").unwrap().remove(0);
        assert_eq!(
            (hit.name.as_str(), hit.version.as_str()),
            ("demo/late", "dev-main")
        );
        assert_eq!(info(&f.project, "demo/late").unwrap().version, "dev-main");
        assert_eq!(info(&f.project, "demo/hello").unwrap().version, "2.0.0");

        // Once a tag contains the skill, the newest matching one is used.
        git(&f.repo, &["tag", "v2.1.0"]);
        write_skill(&f.repo, "late", "tagged");
        commit(&f.repo, "late 2");
        git(&f.repo, &["tag", "v2.2.0"]);
        assert_eq!(info(&f.project, "demo/late").unwrap().version, "2.2.0");
        let pkg = Resolver::new(&f.project, &f.manifest())
            .resolve("demo/late", "^2.0")
            .unwrap();
        assert_eq!(pkg.version, "2.2.0");
        let err = install(&f.project, &["demo/late:^1.0".into()], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("any version matching '^1.0'"), "{err}");
        let err = install(&f.project, &["demo/late:^9.0".into()], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no version matches"), "{err}");
    }

    #[test]
    fn range_skips_tags_that_lack_the_skill() {
        let f = Fixture::new("range-skips");
        add_late(&f);
        git(&f.repo, &["tag", "v2.1.0"]); // 2.1.0 has `late`, 2.0.0 does not
        write_skill(&f.repo, "late", "next");
        commit(&f.repo, "next");
        git(&f.repo, &["tag", "v3.0.0"]);
        let mut resolver = Resolver::new(&f.project, &f.manifest());
        assert_eq!(
            resolver.resolve("demo/late", "<3").unwrap().version,
            "2.1.0"
        );
        let err = resolver
            .resolve("demo/late", "<2.1")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not found") && err.contains("any version matching '<2.1'"),
            "{err}"
        );
    }

    #[test]
    fn per_skill_tags_need_the_skill() {
        let f = Fixture::new("per-skill-missing");
        add_other(&f);
        git(&f.repo, &["tag", "other/v9.0.0", "v1.0.0"]); // commit without `other`
        assert_eq!(info(&f.project, "demo/other").unwrap().version, "3.1.0");
    }

    #[test]
    fn info_syncs_only_the_named_repository() {
        let f = Fixture::new("info");
        fs::write(
            f.root.join("registry.json"),
            r#"{"repositories": [{"name": "demo", "url": "repo"}, {"name": "gone", "url": "missing-repo"}]}"#,
        )
        .unwrap();
        let hit = info(&f.project, "demo/hello").unwrap();
        assert_eq!(
            (hit.name.as_str(), hit.version.as_str()),
            ("demo/hello", "2.0.0")
        );
        assert_eq!(hit.description.as_deref(), Some("Says hello"));
        let err = info(&f.project, "dem/hello").unwrap_err().to_string();
        assert!(err.contains("unknown repository 'dem'"), "{err}");
        let err = info(&f.project, "demo/nope").unwrap_err().to_string();
        assert!(err.starts_with("skill 'demo/nope' not found in "), "{err}");
    }

    #[test]
    fn lock_urls_are_relative_and_survive_moving() {
        let f = Fixture::new("portable");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert_eq!(f.lock().get("demo/hello").unwrap().source.url, "../repo");

        let moved_root =
            env::temp_dir().join(format!("aip-ops-{}-portable-moved", std::process::id()));
        let _ = fs::remove_dir_all(&moved_root);
        fs::rename(&f.root, &moved_root).unwrap();
        let moved = Project::local(moved_root.join("project"), moved_root.join("aip-home"));
        fs::remove_dir_all(moved_root.join("project/.claude/skills/hello")).unwrap();
        let report = install(&moved, &[], Options::default()).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert!(
            moved_root
                .join("project/.claude/skills/hello/SKILL.md")
                .exists()
        );
        assert_eq!(
            Lock::read(&moved.lock_path)
                .unwrap()
                .get("demo/hello")
                .unwrap()
                .source
                .url,
            "../repo"
        );
        assert_eq!(check(&moved).unwrap()[0].status, Status::UpToDate);
        let _ = fs::remove_dir_all(&moved_root);
    }

    #[test]
    fn absolute_lock_urls_keep_working() {
        let f = Fixture::new("absolute-lock");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let mut lock = f.lock();
        lock.packages[0].source.url = f.repo.display().to_string();
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();
        install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(check_one(&f).status, Status::UpToDate);
    }

    fn check_one(f: &Fixture) -> SkillStatus {
        let mut all = check(&f.project).unwrap();
        assert_eq!(all.len(), 1);
        all.remove(0)
    }

    #[test]
    fn latest_follows_the_constraint_stability_flag() {
        assert_eq!(latest_query("^1.0"), "*");
        assert_eq!(latest_query("^1.0@beta"), "*@beta");
        assert_eq!(latest_query("^1.0 || ^2.0@RC"), "*@RC");
    }

    #[test]
    fn check_latest_includes_prereleases_when_flagged() {
        let f = Fixture::new("check-latest-beta");
        write_skill(&f.repo, "hello", "2.1.0-beta.1");
        commit(&f.repo, "beta");
        git(&f.repo, &["tag", "v2.1.0-beta.1"]);
        install(
            &f.project,
            &["demo/hello:^2.0@beta".into()],
            Options::default(),
        )
        .unwrap();
        let row = check_one(&f);
        assert_eq!(row.latest.as_deref(), Some("2.1.0-beta.1"));
        assert_eq!(row.status, Status::UpToDate);
    }

    #[test]
    fn check_reports_update_beyond_constraint() {
        let f = Fixture::new("check-update");
        // `~1.0.0` locks 1.0.0; widening the constraint to `^1.0` then allows 1.1.0.
        install(
            &f.project,
            &["demo/hello:~1.0.0".into()],
            Options::default(),
        )
        .unwrap();
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "^1.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let row = check_one(&f);
        assert_eq!(row.installed.as_deref(), Some("1.0.0"));
        assert_eq!(row.latest.as_deref(), Some("2.0.0"));
        assert_eq!(
            row.status,
            Status::UpdateAvailable {
                to: "1.1.0".into(),
                beyond: Some("2.0.0".into())
            }
        );
        assert_eq!(
            row.to_string(),
            "update available: 1.1.0 (2.0.0 outside constraint)"
        );
    }

    #[test]
    fn check_reports_outside_constraint() {
        let f = Fixture::new("check-outside");
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        let row = check_one(&f);
        assert_eq!(
            row.status,
            Status::OutsideConstraint {
                latest: "2.0.0".into()
            }
        );
        assert_eq!(row.to_string(), "2.0.0 outside ^1.0");
    }

    #[test]
    fn check_reports_up_to_date() {
        let f = Fixture::new("check-current");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert_eq!(check_one(&f).status, Status::UpToDate);
    }

    #[test]
    fn commit_pin_locks_that_commit_and_survives_update() {
        let f = Fixture::new("pin");
        let pin = rev(&f.repo, "v1.1.0");
        let spec = format!("demo/hello:dev-main#{}", &pin[..7]);
        install(&f.project, &[spec], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 1.1.0"));
        let locked = f.lock().get("demo/hello").unwrap().clone();
        assert_eq!(locked.version, "dev-main");
        assert_eq!(locked.source.reference, pin);
        assert_eq!(
            f.manifest().require["demo/hello"].version,
            format!("dev-main#{}", &pin[..7])
        );

        write_skill(&f.repo, "hello", "3.0.0-dev");
        commit(&f.repo, "more");
        update(&f.project, &[], Options::default()).unwrap();
        assert_eq!(f.lock().get("demo/hello").unwrap().source.reference, pin);
        let row = check_one(&f);
        assert_eq!(row.status, Status::UpToDate);
        assert_eq!(row.latest.unwrap(), rev(&f.repo, "main")[..7]);
    }

    #[test]
    fn commit_pin_must_exist_and_be_on_the_branch() {
        let f = Fixture::new("pin-errors");
        git(&f.repo, &["checkout", "--quiet", "-b", "side", "v1.0.0"]);
        write_skill(&f.repo, "hello", "9.9.9-side");
        commit(&f.repo, "side");
        let side = rev(&f.repo, "side");
        git(&f.repo, &["checkout", "--quiet", "main"]);

        let off_branch = format!("demo/hello:dev-main#{}", &side[..7]);
        let err = install(&f.project, &[off_branch], Options::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("commit {} is not on branch main", &side[..7])),
            "{err}"
        );
        let err = install(
            &f.project,
            &["demo/hello:dev-main#deadbee".into()],
            Options::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("commit deadbee not found in"), "{err}");
        assert!(!f.project.lock_path.exists());
    }

    #[test]
    fn stability_flag_selects_prereleases() {
        let f = Fixture::new("stability");
        write_skill(&f.repo, "hello", "2.1.0-beta.1");
        commit(&f.repo, "beta");
        git(&f.repo, &["tag", "v2.1.0-beta.1"]);
        install(&f.project, &["demo/hello:^2.0".into()], Options::default()).unwrap();
        assert_eq!(f.lock().get("demo/hello").unwrap().version, "2.0.0");
        install(
            &f.project,
            &["demo/hello:^2.0@beta".into()],
            Options::default(),
        )
        .unwrap();
        assert_eq!(f.lock().get("demo/hello").unwrap().version, "2.1.0-beta.1");
    }

    #[test]
    fn check_tracks_branch_folder_and_lock_state() {
        let f = Fixture::new("check-branch");
        install(
            &f.project,
            &["demo/hello:dev-main".into()],
            Options::default(),
        )
        .unwrap();
        assert_eq!(check_one(&f).status, Status::UpToDate);

        write_skill(&f.repo, "hello", "3.0.0-dev");
        commit(&f.repo, "more");
        let sha = f.lock().packages[0].source.reference.clone();
        let Status::NewCommits { to } = check_one(&f).status else {
            panic!("expected new commits");
        };
        assert_eq!(to.len(), 7);
        assert_ne!(to, sha[..7]);

        fs::remove_dir_all(f.skill_dir()).unwrap();
        assert_eq!(check_one(&f).status, Status::FolderMissing);

        let mut manifest = f.manifest();
        manifest.require.insert("demo/other".into(), "^1.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let rows = check(&f.project).unwrap();
        assert_eq!(rows[0].name, "demo/hello");
        assert!(matches!(rows[1].status, Status::Error { .. }));

        fs::remove_file(&f.project.lock_path).unwrap();
        assert_eq!(check(&f.project).unwrap()[0].status, Status::NotInstalled);
    }

    #[test]
    fn check_writes_nothing() {
        let f = Fixture::new("check-readonly");
        install(
            &f.project,
            &["demo/hello:~1.0.0".into()],
            Options::default(),
        )
        .unwrap();
        let before = (
            fs::read(&f.project.manifest_path).unwrap(),
            fs::read(&f.project.lock_path).unwrap(),
        );
        check(&f.project).unwrap();
        assert_eq!(fs::read(&f.project.manifest_path).unwrap(), before.0);
        assert_eq!(fs::read(&f.project.lock_path).unwrap(), before.1);
    }

    #[test]
    fn check_requires_manifest() {
        let f = Fixture::new("check-nomanifest");
        fs::remove_file(&f.project.manifest_path).unwrap();
        let err = check(&f.project).unwrap_err().to_string();
        assert!(err.starts_with("no aip.json in "), "{err}");
    }

    impl Fixture {
        fn set_providers(&self, ids: &[&str]) {
            let mut manifest = self.manifest();
            manifest.providers = ids.iter().map(|id| id.to_string()).collect();
            manifest::write_json(&self.project.manifest_path, &manifest).unwrap();
        }

        fn agents_dir(&self) -> PathBuf {
            self.project.root.join(".agents/skills/hello")
        }
    }

    fn rev(repo: &Path, rev: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", rev])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    const DRY_RUN: Options = Options {
        force: false,
        dry_run: true,
    };

    #[test]
    fn installed_folders_carry_a_marker() {
        let f = Fixture::new("marker");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let sha = rev(&f.repo, "v2.0.0");
        let blob = |path: &str| rev(&f.repo, &format!("v2.0.0:skills/hello/{path}"));
        assert_eq!(
            read(&f.skill_dir().join(MARKER_FILE)),
            format!(
                "{{\n    \"name\": \"demo/hello\",\n    \"reference\": \"{sha}\",\n    \"files\": {{\n        \"SKILL.md\": \"{}\",\n        \"scripts/run.sh\": \"{}\"\n    }}\n}}\n",
                blob("SKILL.md"),
                blob("scripts/run.sh")
            )
        );
        assert!(
            !f.project
                .root
                .join(".claude/skills/.hello.aip-tmp")
                .exists()
        );
        assert!(
            !f.project
                .root
                .join(".claude/skills/.hello.aip-old")
                .exists()
        );
    }

    #[test]
    fn folders_without_marker_migrate_only_when_unmodified() {
        let f = Fixture::new("migrate");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let marker_path = f.skill_dir().join(MARKER_FILE);
        // An install from before markers existed is adopted and gets its marker.
        fs::remove_file(&marker_path).unwrap();
        assert_eq!(check_one(&f).status, Status::FolderOutdated);
        let report = install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert!(marker_path.exists());
        // One the user edited is not.
        fs::remove_file(&marker_path).unwrap();
        fs::write(f.skill_dir().join("SKILL.md"), "edited").unwrap();
        let err = install(&f.project, &[], Options::default()).unwrap_err();
        assert!(err.to_string().contains("not installed by aip"), "{err}");
        assert_eq!(installed_version(&f), "edited");
    }

    #[test]
    fn added_provider_does_not_overwrite_user_folder() {
        let f = Fixture::new("added-provider");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        f.set_providers(&["claude-code", "codex"]);
        fs::create_dir_all(f.agents_dir()).unwrap();
        fs::write(f.agents_dir().join("SKILL.md"), "mine").unwrap();
        for result in [
            install(&f.project, &[], Options::default()),
            update(&f.project, &[], Options::default()),
        ] {
            let err = result.unwrap_err().to_string();
            assert!(err.contains("not installed by aip"), "{err}");
            assert_eq!(read(&f.agents_dir().join("SKILL.md")), "mine");
        }
        install(&f.project, &[], FORCE).unwrap();
        assert!(read(&f.agents_dir().join("SKILL.md")).contains("version 2.0.0"));
    }

    #[test]
    fn tampered_lock_does_not_delete_user_folder() {
        let f = Fixture::new("tampered-remove");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let mut lock = f.lock();
        let mut mine = lock.packages[0].clone();
        mine.name = "demo/mine".into();
        mine.path = "skills/mine".into();
        lock.upsert(mine);
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        let folder = f.project.root.join(".claude/skills/mine");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("SKILL.md"), "mine").unwrap();

        let report = install(&f.project, &[], Options::default()).unwrap();
        assert!(
            matches!(&report.changes[..], [Change::Remove { name, .. }] if name == "demo/mine")
        );
        assert_eq!(read(&folder.join("SKILL.md")), "mine");
        assert!(f.lock().get("demo/mine").is_none());
    }

    /// Installs `demo/hello`, then renames its requirement to `other/hello`, which the registry
    /// lists at `other_url`. Returns the report of the `aip update` that applies the rename.
    fn rename_to_other(f: &Fixture, other_url: &str) -> Result<Report> {
        fs::write(
            f.root.join("registry.json"),
            format!(
                r#"{{"repositories": [{{"name": "demo", "url": "repo"}}, {{"name": "other", "url": "{other_url}"}}]}}"#
            ),
        )
        .unwrap();
        let mut manifest = f.manifest();
        let constraint = manifest.require.remove("demo/hello").unwrap();
        manifest.require.insert("other/hello".into(), constraint);
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        update(&f.project, &[], Options::default())
    }

    fn assert_renamed(f: &Fixture, report: &Report) {
        assert_eq!(
            report.changes,
            [
                Change::Remove {
                    name: "demo/hello".into(),
                    version: "2.0.0".into()
                },
                Change::Install {
                    name: "other/hello".into(),
                    version: "2.0.0".into()
                }
            ]
        );
        assert!(read(&f.skill_dir().join(MARKER_FILE)).contains("\"other/hello\""));
        assert!(installed_version(f).contains("version 2.0.0"));
        assert!(f.lock().get("demo/hello").is_none());
    }

    #[test]
    fn renamed_package_takes_over_folder_with_marker() {
        let f = Fixture::new("rename-marker");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let report = rename_to_other(&f, "repo").unwrap();
        assert_renamed(&f, &report);
    }

    #[test]
    fn renamed_package_takes_over_unmodified_markerless_folder() {
        let f = Fixture::new("rename-markerless");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        fs::remove_file(f.skill_dir().join(MARKER_FILE)).unwrap();
        let report = rename_to_other(&f, "repo").unwrap();
        assert_renamed(&f, &report);
    }

    #[test]
    fn renamed_package_does_not_take_over_edited_markerless_folder() {
        let f = Fixture::new("rename-edited");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        fs::remove_file(f.skill_dir().join(MARKER_FILE)).unwrap();
        fs::write(f.skill_dir().join("SKILL.md"), "edited").unwrap();
        let err = rename_to_other(&f, "repo").unwrap_err();
        assert!(err.to_string().contains("not installed by aip"), "{err}");
        assert_eq!(installed_version(&f), "edited");
    }

    #[test]
    fn renamed_package_from_another_url_does_not_take_over() {
        let f = Fixture::new("rename-other-url");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let copy = f.root.join("repo2");
        git(
            &f.root,
            &["clone", "--quiet", "repo", copy.to_str().unwrap()],
        );
        let err = rename_to_other(&f, "repo2").unwrap_err();
        assert!(err.to_string().contains("not installed by aip"), "{err}");
        assert!(read(&f.skill_dir().join(MARKER_FILE)).contains("\"demo/hello\""));
    }

    #[cfg(unix)]
    #[test]
    fn failed_staging_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new("readonly");
        f.set_providers(&["claude-code", "codex"]);
        install(&f.project, &["demo/hello:~1.0".into()], Options::default()).unwrap();
        write_skill(&f.repo, "hello", "1.2.0");
        commit(&f.repo, "1.2.0");
        git(&f.repo, &["tag", "v1.2.0"]);
        let agents = f.project.root.join(".agents/skills");
        let lock_before = read(&f.project.lock_path);
        fs::set_permissions(&agents, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(agents.join("probe"), "").is_ok() {
            return; // running as root: permissions are not enforced
        }

        assert!(update(&f.project, &[], Options::default()).is_err());
        fs::set_permissions(&agents, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read(&f.project.lock_path), lock_before);
        assert!(installed_version(&f).contains("version 1.1.0"));
        assert!(read(&f.agents_dir().join("SKILL.md")).contains("version 1.1.0"));
        assert!(
            !f.project
                .root
                .join(".claude/skills/.hello.aip-tmp")
                .exists()
        );

        update(&f.project, &[], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 1.2.0"));
        assert!(read(&f.agents_dir().join("SKILL.md")).contains("version 1.2.0"));
    }

    #[test]
    fn install_repairs_half_applied_update() {
        let f = Fixture::new("repair");
        f.set_providers(&["claude-code", "codex"]);
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        let old_marker = read(&f.agents_dir().join(MARKER_FILE));
        let old_skill = read(&f.agents_dir().join("SKILL.md"));
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "*".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        update(&f.project, &[], Options::default()).unwrap();
        // As if the swap into .agents/skills had failed: that folder still holds 1.1.0.
        fs::write(f.agents_dir().join(MARKER_FILE), old_marker).unwrap();
        fs::write(f.agents_dir().join("SKILL.md"), old_skill).unwrap();
        assert_eq!(check_one(&f).status, Status::FolderOutdated);

        let report = install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert!(installed_version(&f).contains("version 2.0.0"));
        assert!(read(&f.agents_dir().join("SKILL.md")).contains("version 2.0.0"));
        assert_eq!(check_one(&f).status, Status::UpToDate);
    }

    #[test]
    fn dropped_provider_folders_are_pruned() {
        let f = Fixture::new("prune");
        f.set_providers(&["claude-code", "codex"]);
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        f.set_providers(&["claude-code"]);
        let report = install(&f.project, &[], Options::default()).unwrap();
        assert!(report.changes.is_empty());
        assert_eq!(
            report.pruned,
            [(
                "demo/hello".to_string(),
                f.project.root.join(".agents/skills")
            )]
        );
        assert!(!f.agents_dir().exists() && f.skill_dir().exists());

        // `aip remove` deletes the package from every provider's folder.
        f.set_providers(&["claude-code", "codex"]);
        install(&f.project, &[], Options::default()).unwrap();
        f.set_providers(&["claude-code"]);
        let report = remove(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        assert!(report.pruned.is_empty());
        assert!(!f.agents_dir().exists() && !f.skill_dir().exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_folder_is_refused() {
        let f = Fixture::new("symlink");
        let outside = f.root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, f.project.root.join(".claude/skills")).unwrap();
        let err = install(&f.project, &["demo/hello".into()], FORCE)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is a symbolic link"), "{err}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert!(!f.project.lock_path.exists());
    }

    #[test]
    fn lock_reference_must_match_version() {
        let f = Fixture::new("relabel");
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        let mut lock = f.lock();
        lock.packages[0].source.reference = rev(&f.repo, "v2.0.0");
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        let err = install(&f.project, &[], Options::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("demo/hello 1.1.0 is locked to commit")
                && err.contains("which is not tagged 1.1.0")
                && err.ends_with("run `aip update demo/hello`"),
            "{err}"
        );
        assert!(installed_version(&f).contains("version 1.1.0"));
    }

    #[test]
    fn branch_lock_must_be_on_the_branch() {
        let f = Fixture::new("relabel-branch");
        install(
            &f.project,
            &["demo/hello:dev-main".into()],
            Options::default(),
        )
        .unwrap();
        git(&f.repo, &["checkout", "--quiet", "-b", "side"]);
        write_skill(&f.repo, "hello", "side");
        commit(&f.repo, "side");
        let mut lock = f.lock();
        lock.packages[0].source.reference = rev(&f.repo, "side");
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        let err = install(&f.project, &[], Options::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("which is not on branch main"), "{err}");
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn install_from_lock_uses_cache_without_fetching() {
        let f = Fixture::new("offline");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        fs::remove_dir_all(&f.repo).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();
        install(&f.project, &[], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let f = Fixture::new("dry-run");
        let manifest_before = read(&f.project.manifest_path);
        let report = install(&f.project, &["demo/hello".into()], DRY_RUN).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert_eq!(read(&f.project.manifest_path), manifest_before);
        assert!(!f.project.lock_path.exists());
        assert!(!f.project.root.join(".claude/skills").exists());

        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let state = || (read(&f.project.manifest_path), read(&f.project.lock_path));
        let before = state();
        let report = remove(&f.project, &["demo/hello".into()], DRY_RUN).unwrap();
        assert!(matches!(&report.changes[..], [Change::Remove { .. }]));
        fs::remove_file(f.skill_dir().join("SKILL.md")).unwrap();
        fs::write(f.skill_dir().join("SKILL.md"), "kept").unwrap();
        update(&f.project, &[], DRY_RUN).unwrap();
        assert_eq!(state(), before);
        assert_eq!(installed_version(&f), "kept");
    }

    fn modified(files: usize) -> Status {
        Status::ModifiedLocally { files }
    }

    #[test]
    fn local_edits_show_in_check_and_block_update() {
        let f = Fixture::new("local-edits");
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        assert_eq!(
            check_one(&f).status,
            Status::OutsideConstraint {
                latest: "2.0.0".into()
            }
        );
        fs::write(f.skill_dir().join("SKILL.md"), "edited").unwrap();
        let row = check_one(&f);
        assert_eq!(row.status, modified(1));
        assert_eq!(row.to_string(), "modified locally (1 file)");
        fs::write(f.skill_dir().join("notes.md"), "mine").unwrap();
        fs::remove_file(f.skill_dir().join("scripts/run.sh")).unwrap();
        assert_eq!(check_one(&f).status, modified(3));

        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "*".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let lock_before = read(&f.project.lock_path);
        let expected = format!(
            "{} has local changes (3 files); use --force to discard them",
            f.skill_dir().display()
        );
        for options in [Options::default(), DRY_RUN] {
            let err = update(&f.project, &[], options).unwrap_err().to_string();
            assert_eq!(err, expected);
        }
        assert_eq!(read(&f.project.lock_path), lock_before);
        assert_eq!(installed_version(&f), "edited");

        update(&f.project, &[], FORCE).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
        assert!(!f.skill_dir().join("notes.md").exists());
        assert_eq!(check_one(&f).status, Status::UpToDate);
    }

    #[test]
    fn local_edits_block_remove_and_pruning() {
        let f = Fixture::new("local-edits-remove");
        f.set_providers(&["claude-code", "codex"]);
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        fs::write(f.agents_dir().join("extra.md"), "mine").unwrap();
        f.set_providers(&["claude-code"]);
        let err = install(&f.project, &[], Options::default()).unwrap_err();
        assert!(
            err.to_string()
                .ends_with("has local changes (1 file); use --force to discard them"),
            "{err}"
        );
        assert!(f.agents_dir().join("extra.md").exists());

        fs::write(f.skill_dir().join("SKILL.md"), "edited").unwrap();
        let err = remove(&f.project, &["demo/hello".into()], Options::default()).unwrap_err();
        assert!(err.to_string().contains("has local changes"), "{err}");
        assert!(f.skill_dir().exists() && f.agents_dir().exists());
        assert!(f.manifest().require.contains_key("demo/hello"));

        remove(&f.project, &["demo/hello".into()], FORCE).unwrap();
        assert!(!f.skill_dir().exists() && !f.agents_dir().exists());
    }

    #[test]
    fn old_markers_without_files_are_not_modified() {
        let f = Fixture::new("old-marker");
        install(&f.project, &["demo/hello:^1.0".into()], Options::default()).unwrap();
        let path = f.skill_dir().join(MARKER_FILE);
        let mut marker: Marker = serde_json::from_str(&read(&path)).unwrap();
        marker.files = None;
        manifest::write_json(&path, &marker).unwrap();
        fs::write(f.skill_dir().join("SKILL.md"), "edited").unwrap();
        assert!(matches!(
            check_one(&f).status,
            Status::OutsideConstraint { .. }
        ));
        update(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        // Unmodified folders with file ids are replaced without --force too.
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "*".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        update(&f.project, &[], Options::default()).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    /// A second repository `other` that also has a skill `hello`.
    fn add_other_repo(f: &Fixture) {
        let other = f.root.join("other");
        fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "--quiet", "--initial-branch=main"]);
        write_skill(&other, "hello", "other-1.0.0");
        commit(&other, "other");
        git(&other, &["tag", "v1.0.0"]);
        fs::write(
            f.root.join("registry.json"),
            r#"{"repositories": [{"name": "demo", "url": "repo"}, {"name": "acme/other", "url": "other"}]}"#,
        )
        .unwrap();
    }

    #[test]
    fn clashing_skill_names_need_an_alias() {
        let f = Fixture::new("alias");
        add_other_repo(&f);
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let err = install(&f.project, &["acme/other/hello".into()], Options::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "demo/hello and acme/other/hello both install a skill named 'hello'\nhint: add \"as\" to one of them in aip.json, e.g. \"acme/other/hello\": {\"version\": \"^1.0\", \"as\": \"other-hello\"}"
        );
        assert!(!f.manifest().require.contains_key("acme/other/hello"));

        let report = install_as(
            &f.project,
            &["acme/other/hello".into()],
            Some("other-hello"),
            Options::default(),
        )
        .unwrap();
        assert_eq!(report.changes.len(), 1);
        let folder = f.project.root.join(".claude/skills/other-hello");
        let skill_md = read(&folder.join("SKILL.md"));
        assert!(
            skill_md.starts_with("---\nname: other-hello\ndescription: Says hello\n---\n"),
            "{skill_md}"
        );
        assert!(skill_md.contains("version other-1.0.0"));
        assert!(installed_version(&f).contains("version 2.0.0"));
        assert_eq!(
            f.manifest().require["acme/other/hello"],
            Requirement {
                version: "^1.0".into(),
                alias: Some("other-hello".into())
            }
        );
        assert!(read(&f.project.manifest_path).contains(
            "\"acme/other/hello\": {\n            \"version\": \"^1.0\",\n            \"as\": \"other-hello\"\n        },"
        ));
        assert_eq!(
            f.lock().get("acme/other/hello").unwrap().alias.as_deref(),
            Some("other-hello")
        );
        assert!(
            check(&f.project)
                .unwrap()
                .iter()
                .all(|r| r.status == Status::UpToDate)
        );
        assert!(
            install(&f.project, &[], Options::default())
                .unwrap()
                .changes
                .is_empty()
        );
        // Naming the package again keeps its alias.
        install(&f.project, &["acme/other/hello".into()], Options::default()).unwrap();
        assert!(f.manifest().require["acme/other/hello"].alias.is_some());
    }

    #[test]
    fn as_needs_one_valid_package() {
        let f = Fixture::new("alias-args");
        let err = |specs: &[&str], alias: &str| {
            let specs: Vec<String> = specs.iter().map(|s| s.to_string()).collect();
            install_as(&f.project, &specs, Some(alias), Options::default())
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            err(&["demo/hello", "demo/other"], "x"),
            "--as needs exactly one package"
        );
        assert_eq!(err(&[], "x"), "--as needs exactly one package");
        assert!(err(&["demo/hello"], "../x").starts_with("invalid --as '../x'"));
    }

    #[test]
    fn changing_alias_moves_the_folder() {
        let f = Fixture::new("alias-move");
        install(&f.project, &["demo/hello".into()], Options::default()).unwrap();
        let moved = f.project.root.join(".claude/skills/greet");
        let set_alias = |alias: Option<&str>| {
            let mut manifest = f.manifest();
            manifest.require.get_mut("demo/hello").unwrap().alias = alias.map(str::to_string);
            manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        };
        set_alias(Some("greet"));
        assert_eq!(check_one(&f).status, Status::FolderMissing);
        let report = install(&f.project, &[], Options::default()).unwrap();
        assert_eq!(
            report.changes[0].to_string(),
            "Updating demo/hello (2.0.0 => 2.0.0, folder hello => greet)"
        );
        assert!(!f.skill_dir().exists());
        assert!(read(&moved.join("SKILL.md")).contains("name: greet\n"));
        assert_eq!(check_one(&f).status, Status::UpToDate);

        // Moving back refuses to drop local changes.
        fs::write(moved.join("notes.md"), "mine").unwrap();
        set_alias(None);
        let err = install(&f.project, &[], Options::default()).unwrap_err();
        assert!(err.to_string().contains("has local changes"), "{err}");
        assert!(moved.exists() && !f.skill_dir().exists());
        install(&f.project, &[], FORCE).unwrap();
        assert!(!moved.exists());
        assert!(installed_version(&f).contains("name: hello\n"));
    }

    #[test]
    fn rename_skill_sets_the_frontmatter_name() {
        assert_eq!(
            rename_skill("---\nname: a\nx: 1\n---\nname: body\n", "b"),
            "---\nname: b\nx: 1\n---\nname: body\n"
        );
        assert_eq!(
            rename_skill("---\r\nx: 1\r\n---\r\n", "b"),
            "---\r\nname: b\r\nx: 1\r\n---\r\n"
        );
        assert_eq!(rename_skill("plain", "b"), "---\nname: b\n---\nplain");
    }
}
