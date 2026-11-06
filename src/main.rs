use aip::detect::{self, Detection, Scope};
use aip::ops::{self, Options, Project, Report, SearchHit, SkillStatus, Status};
use aip::table::{self, Cell, Color};
use clap::{Parser, Subcommand};
use std::env;
use std::io::IsTerminal;
use std::path::{self, Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "aip",
    version,
    about = "Package and skill manager for AI agent harnesses"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Detect AI providers and agent harnesses configured in a project
    Detect {
        /// Project folder to scan [default: current directory]
        path: Option<PathBuf>,
        /// Scan user-level configuration in the home folder instead of a project
        #[arg(short, long, conflicts_with = "path")]
        global: bool,
        /// Print results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create aip.json, choosing which providers receive skills
    #[command(
        after_help = "Examples:\n  aip init\n  aip init --provider claude-code --provider codex"
    )]
    Init {
        /// Provider id from `aip detect` [default: detect from the project]
        #[arg(short, long = "provider", value_name = "ID")]
        providers: Vec<String>,
        /// Create the global aip.json in $AIP_HOME
        #[arg(short, long)]
        global: bool,
    },
    /// Search the registry for skills by name or description
    Search {
        /// Text to look for (case-insensitive)
        query: String,
        /// Use the registries of the global aip.json in $AIP_HOME
        #[arg(short, long)]
        global: bool,
        /// Print results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the full description of a skill in the registry
    Info {
        /// Skill as <repository>/<skill>
        name: String,
        /// Use the registries of the global aip.json in $AIP_HOME
        #[arg(short, long)]
        global: bool,
        /// Print result as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show installed skills and whether newer versions are available
    #[command(
        after_help = "Exit status: 1 if a skill has an error; with --strict, also if any skill is not up to date."
    )]
    Check {
        /// Use the global aip.json in $AIP_HOME and check your home folder
        #[arg(short, long)]
        global: bool,
        /// Exit with status 1 unless every skill is up to date (for CI)
        #[arg(long)]
        strict: bool,
        /// Print results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add skills to aip.json and install everything locked in aip.lock
    #[command(
        after_help = "Examples:\n  aip install anthropics/skills/pdf\n  aip install anthropics/skills/pdf:^1.2 anthropics/skills/xlsx:dev-main\n  aip install acme/skills/pdf --as acme-pdf\n  aip install            # install from aip.lock"
    )]
    Install {
        /// Skills to add, as <repository>/<skill>[:<constraint>]
        packages: Vec<String>,
        /// Install the one named skill into this folder instead of its own name
        #[arg(long = "as", value_name = "FOLDER")]
        alias: Option<String>,
        /// Use the global aip.json in $AIP_HOME and install into your home folder
        #[arg(short, long)]
        global: bool,
        /// Replace skill folders that were not installed by aip or have local changes
        #[arg(long)]
        force: bool,
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Update skills to the newest versions allowed by aip.json
    Update {
        /// Skills to update [default: all]
        packages: Vec<String>,
        /// Use the global aip.json in $AIP_HOME and install into your home folder
        #[arg(short, long)]
        global: bool,
        /// Replace skill folders that were not installed by aip or have local changes
        #[arg(long)]
        force: bool,
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove skills from aip.json and delete their folders
    Remove {
        /// Skills to remove, as <repository>/<skill>
        #[arg(required = true)]
        packages: Vec<String>,
        /// Use the global aip.json in $AIP_HOME and remove from your home folder
        #[arg(short, long)]
        global: bool,
        /// Delete skill folders even if they have local changes
        #[arg(long)]
        force: bool,
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> ExitCode {
    let mut code = ExitCode::SUCCESS;
    let result = match Cli::parse().command {
        Command::Detect { path, global, json } => run_detect(path, global, json),
        Command::Init { providers, global } => {
            project(global).and_then(|p| run_init(&p, providers))
        }
        Command::Search {
            query,
            global,
            json,
        } => project(global).and_then(|p| run_search(&p, &query, json)),
        Command::Info { name, global, json } => {
            project(global).and_then(|p| run_info(&p, &name, json))
        }
        Command::Check {
            global,
            strict,
            json,
        } => project(global)
            .and_then(|p| run_check(&p, json))
            .map(|rows| {
                if check_failed(&rows, strict) {
                    code = ExitCode::FAILURE;
                }
            }),
        Command::Install {
            packages,
            alias,
            global,
            force,
            dry_run,
        } => project(global).and_then(|p| {
            let options = Options { force, dry_run };
            let report = ops::install_as(&p, &packages, alias.as_deref(), options)?;
            show(&p, report, dry_run)
        }),
        Command::Update {
            packages,
            global,
            force,
            dry_run,
        } => project(global).and_then(|p| {
            let options = Options { force, dry_run };
            show(&p, ops::update(&p, &packages, options)?, dry_run)
        }),
        Command::Remove {
            packages,
            global,
            force,
            dry_run,
        } => project(global).and_then(|p| {
            let options = Options { force, dry_run };
            show(&p, ops::remove(&p, &packages, options)?, dry_run)
        }),
    };
    match result {
        Ok(()) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn project(global: bool) -> aip::Result<Project> {
    let aip_home = ops::aip_home()?;
    if global {
        let home = detect::global_root()
            .ok_or_else(|| aip::Error::new("cannot determine home directory"))?;
        return Ok(Project::global(home, aip_home));
    }
    let cwd = env::current_dir()
        .map_err(|e| aip::Error::new(format!("cannot determine current directory: {e}")))?;
    Ok(Project::local(cwd, aip_home))
}

fn show(project: &Project, report: Report, dry_run: bool) -> aip::Result<()> {
    if dry_run {
        println!("Dry run: nothing was changed.");
    }
    if report.changes.is_empty() && report.pruned.is_empty() {
        println!("Nothing to install, update or remove");
        return Ok(());
    }
    let shown = |dir: &Path| shown_dir(dir, project);
    let count = |pred: fn(&ops::Change) -> bool| report.changes.iter().filter(|c| pred(c)).count();
    let installs = count(|c| matches!(c, ops::Change::Install { .. }));
    let updates = count(|c| matches!(c, ops::Change::Update { .. }));
    let removals = count(|c| matches!(c, ops::Change::Remove { .. })) + report.pruned.len();
    println!(
        "Package operations: {installs} install{}, {updates} update{}, {removals} removal{}",
        plural(installs),
        plural(updates),
        plural(removals)
    );
    for change in &report.changes {
        println!("  - {change}");
    }
    for (name, dir) in &report.pruned {
        println!("  - Removing {name} from {}", shown(dir));
    }
    let targets: Vec<_> = report.targets.iter().map(|t| shown(t)).collect();
    println!("Skill folders: {}", targets.join(", "));
    Ok(())
}

fn run_init(project: &Project, providers: Vec<String>) -> aip::Result<()> {
    let dirs = ops::init(project, providers)?;
    println!("Created {}", project.manifest_path.display());
    let dirs: Vec<_> = dirs.iter().map(|d| shown_dir(d, project)).collect();
    println!("Skill folders: {}", dirs.join(", "));
    Ok(())
}

/// Prints the check report and returns the rows for the exit status.
fn run_check(project: &Project, json: bool) -> aip::Result<Vec<SkillStatus>> {
    let rows = ops::check(project)?;
    print_check(&rows, json);
    Ok(rows)
}

/// `aip check` fails on any error row; `--strict` also on anything not up to date.
fn check_failed(rows: &[SkillStatus], strict: bool) -> bool {
    rows.iter().any(|r| match r.status {
        Status::UpToDate => false,
        Status::Error { .. } => true,
        _ => strict,
    })
}

fn print_check(rows: &[SkillStatus], json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(rows).expect("skill statuses serialize")
        );
        return;
    }
    if rows.is_empty() {
        println!("No skills required in aip.json.");
        return;
    }
    let color = use_color();
    let cells: Vec<Vec<Cell>> = rows
        .iter()
        .map(|row| {
            let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
            vec![
                Cell::plain(&row.name),
                Cell::plain(dash(&row.installed)),
                Cell::plain(&row.constraint),
                Cell::plain(dash(&row.latest)),
                Cell::colored(row.to_string(), color.then(|| status_color(&row.status))),
            ]
        })
        .collect();
    let headers = ["Package", "Installed", "Constraint", "Latest", "Status"];
    println!("{}", table::render_table(&headers, &cells));
    for line in check_hints(rows) {
        println!("{line}");
    }
}

/// Color only for terminals, and not when `NO_COLOR` is set (https://no-color.org).
fn use_color() -> bool {
    std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

fn status_color(status: &Status) -> Color {
    match status {
        Status::UpToDate => Color::Green,
        Status::UpdateAvailable { .. } | Status::NewCommits { .. } => Color::Yellow,
        _ => Color::Red,
    }
}

/// Lines after the check table: error details first, then every next step that applies.
fn check_hints(rows: &[SkillStatus]) -> Vec<String> {
    let mut lines: Vec<String> = rows
        .iter()
        .filter_map(|r| match &r.status {
            Status::Error { message } => {
                let message = message
                    .strip_prefix(&format!("{}: ", r.name))
                    .unwrap_or(message);
                Some(format!("error: {}: {message}", r.name))
            }
            _ => None,
        })
        .collect();
    let names = |pred: fn(&Status) -> bool| -> Vec<&str> {
        rows.iter()
            .filter(|r| pred(&r.status))
            .map(|r| r.name.as_str())
            .collect()
    };
    // `install` refuses requirements the lock lacks, so those need `update`.
    let unlocked = names(|s| matches!(s, Status::NotInstalled));
    let missing = names(|s| matches!(s, Status::FolderMissing | Status::FolderOutdated));
    let updates = names(|s| {
        matches!(
            s,
            Status::UpdateAvailable { .. } | Status::NewCommits { .. }
        )
    });
    let modified = names(|s| matches!(s, Status::ModifiedLocally { .. }));
    let moved = names(|s| matches!(s, Status::SourceChanged { .. }));
    let outside = names(|s| {
        matches!(
            s,
            Status::OutsideConstraint { .. }
                | Status::UpdateAvailable {
                    beyond: Some(_),
                    ..
                }
        )
    });
    if !unlocked.is_empty() {
        lines.push(format!(
            "Run `aip update {}` to resolve skills missing from aip.lock.",
            unlocked.join(" ")
        ));
    }
    if !missing.is_empty() {
        lines.push("Run `aip install` to install missing or out-of-date skills.".into());
    }
    if !modified.is_empty() {
        lines.push(format!(
            "Local changes in {} will block update/remove; use --force to discard them.",
            modified.join(", ")
        ));
    }
    if !updates.is_empty() {
        lines.push(format!(
            "Run `aip update` to apply {} available update{}.",
            updates.len(),
            plural(updates.len())
        ));
    }
    if !moved.is_empty() {
        lines.push(format!(
            "Run `aip update {}` to repair changed sources.",
            moved.join(" ")
        ));
    }
    if !outside.is_empty() {
        lines.push(format!(
            "Newer versions of {} need a wider constraint in aip.json, then `aip update`.",
            outside.join(", ")
        ));
    }
    if rows.iter().all(|r| r.status == Status::UpToDate) {
        lines.push("All skills are up to date.".into());
    }
    lines
}

fn run_search(project: &Project, query: &str, json: bool) -> aip::Result<()> {
    let hits = ops::search(project, query)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&hits).expect("search hits serialize")
        );
        return Ok(());
    }
    if hits.is_empty() {
        println!("No skills found matching '{query}'.");
        return Ok(());
    }
    let width = |f: fn(&SearchHit) -> usize| hits.iter().map(f).max().unwrap_or(0);
    let name_width = width(|h| h.name.len());
    let version_width = width(|h| h.version.len());
    for hit in &hits {
        let summary = hit
            .description
            .as_deref()
            .map(summarize)
            .unwrap_or_default();
        println!(
            "{:name_width$}   {:version_width$}   {summary}",
            hit.name, hit.version
        );
    }
    println!("\nRun `aip info <repository>/<skill>` for the full description.");
    Ok(())
}

fn run_info(project: &Project, name: &str, json: bool) -> aip::Result<()> {
    let hit = ops::info(project, name)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&hit).expect("search hit serializes")
        );
        return Ok(());
    }
    println!("{} ({})\n", hit.name, hit.version);
    for line in wrap(hit.description.as_deref().unwrap_or("(no description)"), 80) {
        println!("{line}");
    }
    Ok(())
}

const SUMMARY_WIDTH: usize = 90;

/// First sentence of `text`, cut to [`SUMMARY_WIDTH`] characters with an ellipsis.
fn summarize(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = text
        .match_indices(". ")
        .next()
        .map_or(text.len(), |(i, _)| i + 1);
    let first = &text[..end];
    if first.chars().count() <= SUMMARY_WIDTH {
        return first.to_string();
    }
    let cut: String = first.chars().take(SUMMARY_WIDTH - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Greedy word wrap; words longer than `width` stay on their own line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

fn run_detect(path: Option<PathBuf>, global: bool, json: bool) -> aip::Result<()> {
    let (root, scope) = if global {
        let home = detect::global_root()
            .ok_or_else(|| aip::Error::new("cannot determine home directory"))?;
        (home, Scope::Global)
    } else {
        (path.unwrap_or_else(|| PathBuf::from(".")), Scope::Project)
    };

    let found = detect::detect(&root, scope)
        .map_err(|err| aip::Error::new(format!("{}: {err}", root.display())))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&found).expect("detections serialize")
        );
        return Ok(());
    }

    let shown = path::absolute(&root).unwrap_or(root);
    let location = match scope {
        Scope::Project => shown.display().to_string(),
        Scope::Global => format!("global config ({})", shown.display()),
    };
    print_table(&found, &location);
    Ok(())
}

fn print_table(found: &[Detection], location: &str) {
    if found.is_empty() {
        println!("No AI providers detected in {location}.");
        return;
    }
    println!(
        "Detected {} provider{} in {location}:",
        found.len(),
        plural(found.len())
    );
    let name_width = found.iter().map(|d| d.name.len()).max().unwrap_or(0);
    let id_width = found.iter().map(|d| d.id.len() + 2).max().unwrap_or(0);
    for d in found {
        let id = format!("({})", d.id);
        println!(
            "  {:name_width$}   {id:id_width$}   {}",
            d.name,
            d.markers.join(", ")
        );
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A skill folder as printed: relative to the project, or `~/…` for the home folder in global
/// scope. A global folder outside home (e.g. `CLAUDE_CONFIG_DIR`) prints in full.
fn shown_dir(dir: &Path, project: &Project) -> String {
    match project.scope {
        Scope::Project => relative(dir, &project.root),
        Scope::Global => match dir.strip_prefix(&project.root) {
            Ok(rel) => format!("~{}{}", path::MAIN_SEPARATOR, rel.display()),
            Err(_) => dir.display().to_string(),
        },
    }
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_folders_print_under_home_only_when_inside_it() {
        let project = Project::global(PathBuf::from("/home/u"), PathBuf::from("/aip"));
        let sep = path::MAIN_SEPARATOR;
        assert_eq!(
            shown_dir(Path::new("/home/u/.claude/skills"), &project),
            format!("~{sep}{}", Path::new(".claude/skills").display())
        );
        assert_eq!(
            shown_dir(Path::new("/tmp/cfg/skills"), &project),
            Path::new("/tmp/cfg/skills").display().to_string()
        );
    }

    #[test]
    fn summarize_takes_first_sentence_and_truncates() {
        assert_eq!(summarize("Does X.  Then Y."), "Does X.");
        let long = "word ".repeat(40);
        let s = summarize(&long);
        assert!(s.chars().count() <= SUMMARY_WIDTH);
        assert!(s.ends_with('…'));
    }

    fn row(name: &str, status: Status) -> SkillStatus {
        SkillStatus {
            name: name.into(),
            constraint: "*".into(),
            installed: None,
            latest: None,
            status,
        }
    }

    #[test]
    fn hints_send_unlocked_skills_to_update() {
        let rows = [
            row("a/x", Status::NotInstalled),
            row("a/y", Status::FolderMissing),
        ];
        let hints = check_hints(&rows);
        assert!(hints.contains(
            &"Run `aip update a/x` to resolve skills missing from aip.lock.".to_string()
        ));
        assert!(
            hints.contains(
                &"Run `aip install` to install missing or out-of-date skills.".to_string()
            )
        );
    }

    #[test]
    fn hints_warn_about_local_changes() {
        let rows = [row("a/x", Status::ModifiedLocally { files: 2 })];
        assert_eq!(rows[0].to_string(), "modified locally (2 files)");
        assert_eq!(status_color(&rows[0].status), Color::Red);
        assert!(check_failed(&rows, true) && !check_failed(&rows, false));
        assert_eq!(
            check_hints(&rows),
            ["Local changes in a/x will block update/remove; use --force to discard them."]
        );
    }

    #[test]
    fn check_exit_status() {
        let error = Status::Error {
            message: "boom".into(),
        };
        let stale = Status::FolderMissing;
        assert!(check_failed(&[row("a/x", error)], false));
        assert!(!check_failed(&[row("a/x", stale.clone())], false));
        assert!(check_failed(&[row("a/x", stale)], true));
        assert!(!check_failed(&[row("a/x", Status::UpToDate)], true));
    }

    #[test]
    fn wrap_respects_width() {
        let lines = wrap("aaa bbb ccc ddd", 7);
        assert_eq!(lines, ["aaa bbb", "ccc ddd"]);
    }
}
