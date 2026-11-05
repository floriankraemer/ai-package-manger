use aip::detect::{self, Detection, Scope};
use aip::ops::{self, Project, Report, SearchHit, SkillStatus, Status};
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
        /// Print results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the full description of a skill in the registry
    Info {
        /// Skill as <repository>/<skill>
        name: String,
        /// Print result as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show installed skills and whether newer versions are available
    Check {
        /// Use the global aip.json in $AIP_HOME and check your home folder
        #[arg(short, long)]
        global: bool,
        /// Print results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add skills to aip.json and install everything locked in aip.lock
    #[command(
        after_help = "Examples:\n  aip install anthropic/pdf\n  aip install anthropic/pdf:^1.2 anthropic/xlsx:dev-main\n  aip install            # install from aip.lock"
    )]
    Install {
        /// Skills to add, as <repository>/<skill>[:<constraint>]
        packages: Vec<String>,
        /// Use the global aip.json in $AIP_HOME and install into your home folder
        #[arg(short, long)]
        global: bool,
        /// Replace skill folders that were not installed by aip
        #[arg(long)]
        force: bool,
    },
    /// Update skills to the newest versions allowed by aip.json
    Update {
        /// Skills to update [default: all]
        packages: Vec<String>,
        /// Use the global aip.json in $AIP_HOME and install into your home folder
        #[arg(short, long)]
        global: bool,
        /// Replace skill folders that were not installed by aip
        #[arg(long)]
        force: bool,
    },
    /// Remove skills from aip.json and delete their folders
    Remove {
        /// Skills to remove, as <repository>/<skill>
        #[arg(required = true)]
        packages: Vec<String>,
        /// Use the global aip.json in $AIP_HOME and remove from your home folder
        #[arg(short, long)]
        global: bool,
    },
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Detect { path, global, json } => run_detect(path, global, json),
        Command::Init { providers, global } => {
            project(global).and_then(|p| run_init(&p, providers))
        }
        Command::Search { query, json } => run_search(&query, json),
        Command::Info { name, json } => run_info(&name, json),
        Command::Check { global, json } => project(global).and_then(|p| run_check(&p, json)),
        Command::Install {
            packages,
            global,
            force,
        } => project(global).and_then(|p| show(&p, ops::install(&p, &packages, force)?)),
        Command::Update {
            packages,
            global,
            force,
        } => project(global).and_then(|p| show(&p, ops::update(&p, &packages, force)?)),
        Command::Remove { packages, global } => {
            project(global).and_then(|p| show(&p, ops::remove(&p, &packages)?))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
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

fn show(project: &Project, report: Report) -> aip::Result<()> {
    if report.changes.is_empty() {
        println!("Nothing to install, update or remove");
        return Ok(());
    }
    let count = |pred: fn(&ops::Change) -> bool| report.changes.iter().filter(|c| pred(c)).count();
    let installs = count(|c| matches!(c, ops::Change::Install { .. }));
    let updates = count(|c| matches!(c, ops::Change::Update { .. }));
    let removals = count(|c| matches!(c, ops::Change::Remove { .. }));
    println!(
        "Package operations: {installs} install{}, {updates} update{}, {removals} removal{}",
        plural(installs),
        plural(updates),
        plural(removals)
    );
    for change in &report.changes {
        println!("  - {change}");
    }
    let targets: Vec<_> = report
        .targets
        .iter()
        .map(|t| match project.scope {
            Scope::Project => relative(t, &project.root),
            Scope::Global => format!("~{}{}", path::MAIN_SEPARATOR, relative(t, &project.root)),
        })
        .collect();
    println!("Skill folders: {}", targets.join(", "));
    Ok(())
}

fn run_init(project: &Project, providers: Vec<String>) -> aip::Result<()> {
    let dirs = ops::init(project, providers)?;
    println!("Created {}", project.manifest_path.display());
    let dirs: Vec<_> = dirs.iter().map(|d| relative(d, &project.root)).collect();
    println!("Skill folders: {}", dirs.join(", "));
    Ok(())
}

fn run_check(project: &Project, json: bool) -> aip::Result<()> {
    let rows = ops::check(project)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).expect("skill statuses serialize")
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("No skills required in aip.json.");
        return Ok(());
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
    for line in check_hints(&rows) {
        println!("{line}");
    }
    Ok(())
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
    let missing = names(|s| matches!(s, Status::NotInstalled | Status::FolderMissing));
    let updates = names(|s| {
        matches!(
            s,
            Status::UpdateAvailable { .. } | Status::NewCommits { .. }
        )
    });
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
    if !missing.is_empty() {
        lines.push("Run `aip install` to install missing skills.".into());
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

fn run_search(query: &str, json: bool) -> aip::Result<()> {
    let hits = ops::search(&project(false)?, query)?;
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

fn run_info(name: &str, json: bool) -> aip::Result<()> {
    let hit = ops::search(&project(false)?, name)?
        .into_iter()
        .find(|h| h.name == name)
        .ok_or_else(|| aip::Error::new(format!("skill '{name}' not found in the registry")))?;
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
    fn summarize_takes_first_sentence_and_truncates() {
        assert_eq!(summarize("Does X.  Then Y."), "Does X.");
        let long = "word ".repeat(40);
        let s = summarize(&long);
        assert!(s.chars().count() <= SUMMARY_WIDTH);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn wrap_respects_width() {
        let lines = wrap("aaa bbb ccc ddd", 7);
        assert_eq!(lines, ["aaa bbb", "ccc ddd"]);
    }
}
