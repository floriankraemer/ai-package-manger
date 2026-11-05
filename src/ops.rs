//! Package operations: `search`, `check`, `install`, `update` and `remove`.

use crate::auth::Auth;
use crate::constraint::{self, Constraint};
use crate::detect::{self, Scope};
use crate::filelock::{self, FileLock};
use crate::git::Repo;
use crate::manifest::{self, LOCK_FILE, Lock, LockedPackage, MANIFEST_FILE, Manifest, Source};
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
            Self::Update { name, from, to } => {
                let verb = match (
                    constraint::parse_version(from),
                    constraint::parse_version(to),
                ) {
                    (Some(a), Some(b)) if b > a => "Upgrading",
                    (Some(a), Some(b)) if b < a => "Downgrading",
                    _ => "Updating",
                };
                write!(f, "{verb} {name} ({from} => {to})")
            }
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub changes: Vec<Change>,
    /// Skill folders written to.
    pub targets: Vec<PathBuf>,
}

/// `aip install [packages]`: adds `<name>[:<constraint>]` specs to aip.json, then installs
/// everything from aip.lock. Requirements the lock does not cover are an error unless named.
pub fn install(project: &Project, specs: &[String], force: bool) -> Result<Report> {
    let _lock = project.lock()?;
    let existing = Manifest::read(&project.manifest_path)?;
    if existing.is_none() && specs.is_empty() {
        return Err(Error::new(format!(
            "no {MANIFEST_FILE} in {}; run `aip install <repository>/<skill>` to add a skill",
            project.root.display()
        )));
    }
    let mut manifest = existing.unwrap_or_default();
    let old = Lock::read(&project.lock_path)?;
    let targets = targets(project, &mut manifest)?;
    let mut resolver = Resolver::new(project, &manifest);
    let mut new = old.clone();
    let mut forced = HashSet::new();
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
        manifest.require.insert(name.to_string(), constraint);
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
        force,
    )
}

/// `aip update [packages]`: re-resolves the named packages (all if none) to the newest
/// versions their constraints allow.
pub fn update(project: &Project, names: &[String], force: bool) -> Result<Report> {
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
    let old = Lock::read(&project.lock_path)?;
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
        force,
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
pub fn remove(project: &Project, names: &[String]) -> Result<Report> {
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
    let old = Lock::read(&project.lock_path)?;
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
        false,
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
                if !registry::valid_name(&skill) {
                    continue;
                }
                let (version, sha) = latest(repo, &skill)?;
                let Some(skill_md) = repo.read(&sha, &format!("skills/{skill}/SKILL.md"))? else {
                    continue;
                };
                found.push(SearchHit {
                    name: format!("{}/{skill}", entry.name),
                    version,
                    description: registry::description(&skill_md),
                });
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
    let lock = Lock::read(&project.lock_path)?;
    // `targets` may fill in providers; do that on a copy so nothing is written back.
    let targets = targets(project, &mut manifest.clone())?;
    let mut resolver = Resolver::new(project, &manifest);
    Ok(manifest
        .require
        .iter()
        .map(|(name, constraint)| {
            let locked = lock.get(name);
            let (latest, status) = check_package(&mut resolver, name, constraint, locked, &targets)
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
    let is_branch = matches!(Constraint::parse(constraint), Ok(Constraint::Branch(_)));
    let latest = if is_branch {
        Some(allowed.version.clone())
    } else {
        resolver.resolve(name, "*").ok().map(|p| p.version)
    };
    let folder_missing = match locked {
        Some(p) => {
            let skill = skill_of(p)?;
            targets.iter().any(|t| !t.join(skill).is_dir())
        }
        None => false,
    };
    let status = match locked {
        None => Status::NotInstalled,
        Some(p) if p.source.url != registry_url => Status::SourceChanged { registry_url },
        Some(_) if folder_missing => Status::FolderMissing,
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
    for (name, constraint) in &manifest.require {
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
            .get(repository)
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
    force: bool,
) -> Result<Report> {
    verify_sources(new, resolver)?;
    let plan = plan(old, new, targets, force)?;
    // Lock first: if applying fails midway, re-running `aip install` completes the job.
    manifest::write_json(&project.manifest_path, manifest)?;
    manifest::write_json(&project.lock_path, new)?;
    apply(&plan, targets, resolver)?;
    Ok(Report {
        changes: plan.changes,
        targets: targets.to_vec(),
    })
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
    let mut dirs = Vec::new();
    for id in &manifest.providers {
        let provider = detect::provider(id).ok_or_else(|| {
            let known: Vec<_> = detect::PROVIDERS.iter().map(|p| p.id).collect();
            Error::new(format!(
                "unknown provider '{id}' in {} (known: {})",
                project.manifest_path.display(),
                known.join(", ")
            ))
        })?;
        let Some((local, global)) = provider.skills else {
            eprintln!("warning: provider '{id}' does not support skills; skipping");
            continue;
        };
        let rel = match project.scope {
            Scope::Project => local,
            Scope::Global => global,
        };
        let dir = detect::marker_path(&project.root, rel);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if dirs.is_empty() {
        return Err(Error::new(format!(
            "none of the providers in {} support skills",
            project.manifest_path.display()
        )));
    }
    Ok(dirs)
}

struct Plan {
    changes: Vec<Change>,
    /// Skill folder names to delete.
    removes: Vec<String>,
    writes: Vec<LockedPackage>,
}

/// Works out what to change on disk, refusing to overwrite skills aip does not manage.
fn plan(old: &Lock, new: &Lock, targets: &[PathBuf], force: bool) -> Result<Plan> {
    let mut managed = HashSet::new();
    for pkg in &old.packages {
        managed.insert(skill_of(pkg)?);
    }
    let mut seen = HashMap::new();
    let mut plan = Plan {
        changes: Vec::new(),
        removes: Vec::new(),
        writes: Vec::new(),
    };
    for pkg in &old.packages {
        if new.get(&pkg.name).is_none() {
            plan.changes.push(Change::Remove {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
            plan.removes.push(skill_of(pkg)?.to_string());
        }
    }
    for pkg in &new.packages {
        let skill = skill_of(pkg)?;
        if let Some(other) = seen.insert(skill, &pkg.name) {
            return Err(Error::new(format!(
                "{other} and {} both install a skill named '{skill}'",
                pkg.name
            )));
        }
        let present = targets.iter().all(|t| t.join(skill).is_dir());
        let change = match old.get(&pkg.name) {
            Some(prev) if prev.source == pkg.source && present => continue,
            Some(prev) if prev.source != pkg.source => {
                let (from, to) = update_labels(prev, pkg);
                Change::Update {
                    name: pkg.name.clone(),
                    from,
                    to,
                }
            }
            _ => Change::Install {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            },
        };
        if !force
            && !managed.contains(skill)
            && let Some(dest) = targets
                .iter()
                .map(|t| t.join(skill))
                .find(|d| d.symlink_metadata().is_ok())
        {
            return Err(Error::new(format!(
                "refusing to overwrite {}: it was not installed by aip (use --force to replace it)",
                dest.display()
            )));
        }
        plan.changes.push(change);
        plan.writes.push(pkg.clone());
    }
    Ok(plan)
}

fn apply(plan: &Plan, targets: &[PathBuf], resolver: &mut Resolver) -> Result<()> {
    for skill in &plan.removes {
        for target in targets {
            delete(&target.join(skill))?;
        }
    }
    for pkg in &plan.writes {
        let skill = skill_of(pkg)?;
        let repo = resolver.repo(&pkg.source.url)?;
        for target in targets {
            // Export next to the destination, then swap, so a failure never leaves half a skill.
            let staging = target.join(format!(".{skill}.aip-tmp"));
            delete(&staging)?;
            repo.export(&pkg.source.reference, &pkg.path, &staging)?;
            let dest = target.join(skill);
            delete(&dest)?;
            fs::rename(&staging, &dest).context(|| format!("cannot write {}", dest.display()))?;
        }
    }
    Ok(())
}

/// Version labels for an update; equal versions (`dev-main`, or a re-pointed source) get the
/// short commit added, like Composer: `dev-main 1a2b3c4 => dev-main 5d6e7f8`.
fn update_labels(prev: &LockedPackage, next: &LockedPackage) -> (String, String) {
    if prev.version != next.version {
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

/// Skill folder name of a locked package, after validating the entry: lock files are
/// user-editable input, and `reference` is passed to git as a revision.
fn skill_of(pkg: &LockedPackage) -> Result<&str> {
    let (_, skill) = registry::split_package(&pkg.name)?;
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
    Ok(skill)
}

/// Highest stable version of `skill`, else the default branch head: `(version label, commit sha)`.
fn latest(repo: &Repo, skill: &str) -> Result<(String, String)> {
    let newest = versions(repo, skill)?
        .into_iter()
        .filter(|(v, _)| v.pre.is_empty())
        .max_by(|a, b| a.0.cmp(&b.0));
    match newest {
        Some((version, tag)) => Ok((version.to_string(), tag_sha(repo, &tag)?)),
        None => {
            let branch = repo.default_branch()?;
            let sha = repo
                .resolve(&format!("refs/heads/{branch}"))?
                .ok_or_else(|| Error::new(format!("default branch '{branch}' has no commits")))?;
            Ok((format!("dev-{branch}"), sha))
        }
    }
}

/// Versions of `skill` with the tag naming each. Tags of the form `<skill>/<version>` win;
/// without any, the repository's own version tags (those without a `/`) apply to all skills.
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

fn tag_sha(repo: &Repo, tag: &str) -> Result<String> {
    repo.resolve(&format!("refs/tags/{tag}"))?
        .ok_or_else(|| Error::new(format!("tag '{tag}' does not point to a commit")))
}

/// Resolves package names against the registry; fetches each repository at most once per run.
struct Resolver {
    registries: Registries,
    cache: PathBuf,
    repos: HashMap<String, Repo>,
}

impl Resolver {
    fn new(project: &Project, manifest: &Manifest) -> Self {
        Self {
            registries: project.registries(Some(manifest)),
            cache: project.aip_home.join("cache").join("repos"),
            repos: HashMap::new(),
        }
    }

    fn registries(&self) -> &Registries {
        &self.registries
    }

    // ponytail: progress goes to stderr from the library; route through a callback when the GUI needs it.
    fn repo(&mut self, url: &str) -> Result<&Repo> {
        if !self.repos.contains_key(url) {
            eprintln!("Fetching {url}");
            let repo = Repo::sync(url, &self.cache)?;
            self.repos.insert(url.to_string(), repo);
        }
        Ok(&self.repos[url])
    }

    fn url_of(&mut self, name: &str) -> Result<String> {
        let (repository, _) = registry::split_package(name)?;
        Ok(self.registries.get(repository)?.url)
    }

    /// Constraint for `aip install <name>` without one: `^major.minor` of the latest stable
    /// version, or `dev-<default branch>` for repositories without version tags.
    fn default_constraint(&mut self, name: &str) -> Result<String> {
        let (_, skill) = registry::split_package(name)?;
        let url = self.url_of(name)?;
        let repo = self.repo(&url)?;
        match versions(repo, skill)?
            .into_iter()
            .map(|(v, _)| v)
            .filter(|v| v.pre.is_empty())
            .max()
        {
            Some(version) => Ok(constraint::default_for(&version)),
            None => Ok(format!("dev-{}", repo.default_branch()?)),
        }
    }

    fn resolve(&mut self, name: &str, constraint: &str) -> Result<LockedPackage> {
        let (_, skill) = registry::split_package(name)?;
        let parsed =
            Constraint::parse(constraint).map_err(|e| Error::new(format!("{name}: {e}")))?;
        let url = self.url_of(name)?;
        let repo = self.repo(&url)?;
        let (version, sha) = match &parsed {
            Constraint::Branch(branch) => {
                let sha = repo
                    .resolve(&format!("refs/heads/{branch}"))?
                    .ok_or_else(|| {
                        Error::new(format!("{name}: branch '{branch}' not found in {url}"))
                    })?;
                (format!("dev-{branch}"), sha)
            }
            Constraint::Range { .. } => {
                let mut all = versions(repo, skill)?;
                all.sort_by(|a, b| a.0.cmp(&b.0));
                let Some((version, tag)) = all.iter().rev().find(|(v, _)| parsed.matches(v)) else {
                    let available: Vec<_> = all.iter().map(|(v, _)| v.to_string()).collect();
                    return Err(Error::new(format!(
                        "{name}: no version matches '{constraint}' (available: {})",
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
                    )));
                };
                (version.to_string(), tag_sha(repo, tag)?)
            }
        };
        let path = format!("skills/{skill}");
        let skill_md = repo
            .read(&sha, &format!("{path}/SKILL.md"))?
            .ok_or_else(|| {
                Error::new(format!(
                    "{name}: skill '{skill}' not found in {url} at {version} (expected {path}/SKILL.md)"
                ))
            })?;
        Ok(LockedPackage {
            name: name.to_string(),
            version,
            description: registry::description(&skill_md),
            source: Source::git(url, sha),
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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
    fn install_resolves_constraint_and_writes_files() {
        let f = Fixture::new("install");
        let report = install(&f.project, &["demo/hello:^1.0".into()], false).unwrap();
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
        assert_eq!(manifest.require["demo/hello"], "^1.0");
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
        assert!(install(&f.project, &[], false).unwrap().changes.is_empty());
        fs::remove_dir_all(f.skill_dir()).unwrap();
        let report = install(&f.project, &[], false).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert!(installed_version(&f).contains("version 1.1.0"));
    }

    #[test]
    fn default_constraint_is_caret_of_latest() {
        let f = Fixture::new("default");
        install(&f.project, &["demo/hello".into()], false).unwrap();
        assert_eq!(f.manifest().require["demo/hello"], "^2.0");
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn update_moves_to_newest_allowed_version() {
        let f = Fixture::new("update");
        install(&f.project, &["demo/hello:~1.0".into()], false).unwrap();
        write_skill(&f.repo, "hello", "1.2.0");
        commit(&f.repo, "1.2.0");
        git(&f.repo, &["tag", "v1.2.0"]);

        // install keeps the locked version; update picks up the new tag.
        assert!(install(&f.project, &[], false).unwrap().changes.is_empty());
        let report = update(&f.project, &[], false).unwrap();
        assert_eq!(
            report.changes,
            [Change::Update {
                name: "demo/hello".into(),
                from: "1.1.0".into(),
                to: "1.2.0".into()
            }]
        );
        assert_eq!(
            report.changes[0].to_string(),
            "Upgrading demo/hello (1.1.0 => 1.2.0)"
        );
        assert!(installed_version(&f).contains("version 1.2.0"));
        assert!(update(&f.project, &["demo/nope".into()], false).is_err());
    }

    #[test]
    fn branch_constraint_pins_commit() {
        let f = Fixture::new("branch");
        write_skill(&f.repo, "hello", "main-head");
        commit(&f.repo, "main head");
        install(&f.project, &["demo/hello:dev-main".into()], false).unwrap();
        let lock = f.lock();
        assert_eq!(lock.packages[0].version, "dev-main");
        assert_eq!(lock.packages[0].source.reference.len(), 40);
        assert!(installed_version(&f).contains("version main-head"));

        // A new commit on the branch updates with short commits in the label.
        let old_sha = lock.packages[0].source.reference.clone();
        write_skill(&f.repo, "hello", "main-next");
        commit(&f.repo, "main next");
        let report = update(&f.project, &[], false).unwrap();
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
        let err = install(&f.project, &["demo/hello".into()], false).unwrap_err();
        assert!(err.to_string().contains("not installed by aip"), "{err}");
        assert_eq!(installed_version(&f), "mine");
        assert!(!f.project.lock_path.exists());

        install(&f.project, &["demo/hello".into()], true).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn dropped_requirement_is_removed() {
        let f = Fixture::new("remove");
        install(&f.project, &["demo/hello".into()], false).unwrap();
        let mut manifest = f.manifest();
        manifest.require.clear();
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let report = install(&f.project, &[], false).unwrap();
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
        install(&f.project, &["demo/hello".into()], false).unwrap();
        let mut lock = f.lock();
        lock.packages[0].source.reference = "--output=/tmp/x".into();
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();
        let err = install(&f.project, &[], false).unwrap_err().to_string();
        assert!(err.contains("commit SHA"), "{err}");
    }

    #[test]
    fn readable_errors() {
        let f = Fixture::new("errors");
        let msg = |spec: &str| {
            install(&f.project, &[spec.into()], false)
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
            false,
        )
        .unwrap();
        assert_eq!(f.manifest().require["demo/other"], "^3.1");
        assert_eq!(f.manifest().require["demo/hello"], "^2.0");
        let lock = f.lock();
        assert_eq!(lock.get("demo/other").unwrap().version, "3.1.0");
        assert_eq!(lock.get("demo/hello").unwrap().version, "2.0.0");
        let err = install(&f.project, &["demo/other:^1.0".into()], false).unwrap_err();
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
        install(&f.project, &["demo/hello".into()], false).unwrap();
        assert!(f.skill_dir().exists());
        let report = remove(&f.project, &["demo/hello".into()]).unwrap();
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
        let err = remove(&f.project, &["demo/hello".into()]).unwrap_err();
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
        install(&f.project, &["more/hello".into()], false).unwrap();
        let url = f.lock().get("more/hello").unwrap().source.url.clone();
        assert!(url.ends_with("repo"), "{url}");
        let registries = f.project.registries(Some(&f.manifest()));
        assert!(registries.get("demo").unwrap().url.ends_with("repo"));
    }

    #[test]
    fn rejects_lock_url_that_differs_from_registry() {
        let f = Fixture::new("lock-url");
        install(&f.project, &["demo/hello:^1.0".into()], false).unwrap();
        let other = f.root.join("other-repo");
        fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "--quiet"]);
        let mut lock = f.lock();
        lock.packages[0].source.url = other.display().to_string();
        manifest::write_json(&f.project.lock_path, &lock).unwrap();
        fs::remove_dir_all(f.skill_dir()).unwrap();

        let err = install(&f.project, &[], false).unwrap_err().to_string();
        assert!(err.contains("run `aip update demo/hello`"), "{err}");
        assert!(!f.skill_dir().exists());
        assert_eq!(f.lock().packages[0].source.url, other.display().to_string());

        update(&f.project, &["demo/hello".into()], false).unwrap();
        assert!(installed_version(&f).contains("version 1.1.0"));
        assert_ne!(f.lock().packages[0].source.url, other.display().to_string());
    }

    #[test]
    fn changed_constraint_requires_update() {
        let f = Fixture::new("stale-constraint");
        install(&f.project, &["demo/hello:^1.0".into()], false).unwrap();
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "^2.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();
        let lock_before = fs::read_to_string(&f.project.lock_path).unwrap();

        let err = install(&f.project, &[], false).unwrap_err().to_string();
        assert_eq!(
            err,
            "aip.lock is out of date with aip.json:\n  - demo/hello: locked 1.1.0 does not satisfy ^2.0\nRun `aip update demo/hello` to resolve it."
        );
        assert!(installed_version(&f).contains("version 1.1.0"));
        assert_eq!(
            fs::read_to_string(&f.project.lock_path).unwrap(),
            lock_before
        );

        update(&f.project, &["demo/hello".into()], false).unwrap();
        assert!(installed_version(&f).contains("version 2.0.0"));
    }

    #[test]
    fn requirement_missing_from_lock_requires_naming_it() {
        let f = Fixture::new("not-locked");
        let mut manifest = f.manifest();
        manifest.require.insert("demo/hello".into(), "^1.0".into());
        manifest::write_json(&f.project.manifest_path, &manifest).unwrap();

        let err = install(&f.project, &[], false).unwrap_err().to_string();
        assert!(err.contains("demo/hello: not in aip.lock"), "{err}");
        assert!(!f.project.lock_path.exists());

        install(&f.project, &["demo/hello:^1.0".into()], false).unwrap();
        assert!(installed_version(&f).contains("version 1.1.0"));
    }

    fn check_one(f: &Fixture) -> SkillStatus {
        let mut all = check(&f.project).unwrap();
        assert_eq!(all.len(), 1);
        all.remove(0)
    }

    #[test]
    fn check_reports_update_beyond_constraint() {
        let f = Fixture::new("check-update");
        // `~1.0.0` locks 1.0.0; widening the constraint to `^1.0` then allows 1.1.0.
        install(&f.project, &["demo/hello:~1.0.0".into()], false).unwrap();
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
        install(&f.project, &["demo/hello:^1.0".into()], false).unwrap();
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
        install(&f.project, &["demo/hello".into()], false).unwrap();
        assert_eq!(check_one(&f).status, Status::UpToDate);
    }

    #[test]
    fn check_tracks_branch_folder_and_lock_state() {
        let f = Fixture::new("check-branch");
        install(&f.project, &["demo/hello:dev-main".into()], false).unwrap();
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
        install(&f.project, &["demo/hello:~1.0.0".into()], false).unwrap();
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
}
