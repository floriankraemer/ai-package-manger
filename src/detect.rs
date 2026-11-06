//! Detection of AI providers / agent harnesses by their on-disk configuration markers.

use serde::Serialize;
use std::env;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Where to look for provider configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// A project folder.
    Project,
    /// The user's home folder (user-level configuration).
    Global,
}

/// A known provider and the paths that reveal its presence.
///
/// Markers are `/`-separated paths relative to the scanned root; a trailing `/` marks a directory
/// (only for display — any existing file or directory matches).
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub project: &'static [&'static str],
    pub global: &'static [&'static str],
    /// `(variable, prefix)`: when the variable names an absolute directory, it replaces the
    /// home-relative `prefix` (e.g. `.claude`) of every global path under that prefix.
    /// An empty prefix means the variable replaces the home folder itself.
    pub global_env: Option<(&'static str, &'static str)>,
    /// Skill folders `(project, global)`, relative like markers; `None` if skills are unsupported.
    pub skills: Option<(&'static str, &'static str)>,
}

/// Looks up a provider by id.
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "claude-code",
        name: "Claude Code",
        project: &[".claude/", "CLAUDE.md", "CLAUDE.local.md", ".mcp.json"],
        global: &[".claude/", ".claude.json"],
        // https://code.claude.com/docs/en/settings ("set CLAUDE_CONFIG_DIR" to keep settings,
        // history and plugins elsewhere); skills live in `$CLAUDE_CONFIG_DIR/skills`.
        global_env: Some(("CLAUDE_CONFIG_DIR", ".claude")),
        skills: Some((".claude/skills", ".claude/skills")),
    },
    Provider {
        id: "codex",
        name: "OpenAI Codex",
        project: &[".codex/"],
        global: &[".codex/"],
        // Detection only: Codex reads user skills from `~/.agents/skills`, not `$CODEX_HOME`.
        // https://learn.chatgpt.com/docs/build-skills
        global_env: Some(("CODEX_HOME", ".codex")),
        skills: Some((".agents/skills", ".agents/skills")),
    },
    Provider {
        id: "agents-md",
        name: "AGENTS.md (cross-tool)",
        project: &["AGENTS.md", ".agents/"],
        global: &[".agents/"],
        global_env: None,
        skills: Some((".agents/skills", ".agents/skills")),
    },
    Provider {
        id: "gemini-cli",
        name: "Gemini CLI",
        project: &[".gemini/", "GEMINI.md"],
        global: &[".gemini/"],
        // `GEMINI_CLI_HOME` replaces the home folder; `.gemini` is created inside it.
        // https://geminicli.com/docs/reference/configuration
        global_env: Some(("GEMINI_CLI_HOME", "")),
        skills: Some((".gemini/skills", ".gemini/skills")),
    },
    Provider {
        id: "cursor",
        name: "Cursor",
        project: &[".cursor/", ".cursorrules"],
        global: &[".cursor/"],
        global_env: None,
        skills: Some((".cursor/skills", ".cursor/skills")),
    },
    Provider {
        id: "copilot",
        name: "GitHub Copilot",
        project: &[
            ".github/copilot-instructions.md",
            ".github/instructions/",
            ".github/prompts/",
            ".github/agents/",
        ],
        global: &[".copilot/"],
        // `COPILOT_HOME` replaces `~/.copilot`, which holds `skills/`.
        // https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference
        global_env: Some(("COPILOT_HOME", ".copilot")),
        skills: Some((".github/skills", ".copilot/skills")),
    },
    Provider {
        id: "windsurf",
        name: "Windsurf",
        project: &[".windsurf/", ".windsurfrules"],
        global: &[".codeium/windsurf/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "cline",
        name: "Cline",
        project: &[".clinerules"],
        global: &["Documents/Cline/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "roo",
        name: "Roo Code",
        project: &[".roo/", ".roomodes"],
        global: &[".roo/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "kiro",
        name: "Kiro",
        project: &[".kiro/"],
        global: &[".kiro/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "continue",
        name: "Continue",
        project: &[".continue/"],
        global: &[".continue/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "junie",
        name: "JetBrains Junie",
        project: &[".junie/"],
        global: &[".junie/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "amazon-q",
        name: "Amazon Q",
        project: &[".amazonq/"],
        global: &[".aws/amazonq/"],
        global_env: None,
        skills: None,
    },
    Provider {
        id: "opencode",
        name: "OpenCode",
        project: &[".opencode/", "opencode.json"],
        global: &[".config/opencode/"],
        global_env: None,
        skills: Some((".opencode/skills", ".config/opencode/skills")),
    },
    Provider {
        id: "aider",
        name: "Aider",
        project: &[".aider.conf.yml"],
        global: &[".aider.conf.yml"],
        global_env: None,
        skills: None,
    },
];

/// A provider found under the scanned root, with the markers that matched.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Detection {
    pub id: &'static str,
    pub name: &'static str,
    pub markers: Vec<String>,
}

/// Root folder for [`Scope::Global`]: the user's home (`HOME` on Unix, `USERPROFILE` on Windows).
pub fn global_root() -> Option<PathBuf> {
    env::home_dir().filter(|p| !p.as_os_str().is_empty())
}

/// Detects providers configured directly under `root`, in table order.
pub fn detect(root: &Path, scope: Scope) -> io::Result<Vec<Detection>> {
    detect_with(root, scope, &|k| env::var_os(k))
}

fn detect_with(
    root: &Path,
    scope: Scope,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> io::Result<Vec<Detection>> {
    if !root.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "not a directory",
        ));
    }
    Ok(PROVIDERS
        .iter()
        .filter_map(|p| {
            let candidates = match scope {
                Scope::Project => p.project,
                Scope::Global => p.global,
            };
            let markers: Vec<String> = candidates
                .iter()
                .filter(|m| scoped_path_with(p, scope, root, m, lookup).exists())
                .map(|m| m.to_string())
                .collect();
            (!markers.is_empty()).then_some(Detection {
                id: p.id,
                name: p.name,
                markers,
            })
        })
        .collect())
}

/// Resolves `rel` (a marker or skill folder of `provider`) for `scope` under `root`.
/// In global scope, the provider's config-home variable (see [`Provider::global_env`]) wins.
pub fn scoped_path(provider: &Provider, scope: Scope, root: &Path, rel: &str) -> PathBuf {
    scoped_path_with(provider, scope, root, rel, &|k| env::var_os(k))
}

fn scoped_path_with(
    provider: &Provider,
    scope: Scope,
    root: &Path,
    rel: &str,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> PathBuf {
    if scope == Scope::Global
        && let Some((var, prefix)) = provider.global_env
        && let Some(base) = override_dir(lookup, var)
        && let Some(rest) = strip_components(rel, prefix)
    {
        return marker_path(&base, rest);
    }
    marker_path(root, rel)
}

/// The variable's value if set, non-empty and absolute.
/// ponytail: relative values are ignored as unset because the tools' behaviour is undocumented.
fn override_dir(lookup: &dyn Fn(&str) -> Option<OsString>, var: &str) -> Option<PathBuf> {
    lookup(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `rel` without the leading `prefix` components, or `None` if it does not start with them.
fn strip_components<'a>(rel: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return Some(rel);
    }
    let rest = rel.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// Joins a `/`-separated marker onto `root` component-wise, so paths are native on every OS.
pub fn marker_path(root: &Path, marker: &str) -> PathBuf {
    marker
        .split('/')
        .filter(|s| !s.is_empty())
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Fresh, empty temp dir unique to this process and test.
    fn fixture(name: &str, entries: &[&str]) -> PathBuf {
        let root = env::temp_dir().join(format!("aip-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for entry in entries {
            let path = marker_path(&root, entry);
            if entry.ends_with('/') {
                fs::create_dir_all(path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "").unwrap();
            }
        }
        root
    }

    fn ids(found: &[Detection]) -> Vec<&str> {
        found.iter().map(|d| d.id).collect()
    }

    #[test]
    fn detects_project_markers_in_table_order() {
        let root = fixture("project", &[".cursorrules", "CLAUDE.md", ".claude/"]);
        let found = detect(&root, Scope::Project).unwrap();
        assert_eq!(ids(&found), ["claude-code", "cursor"]);
        assert_eq!(found[0].markers, [".claude/", "CLAUDE.md"]);
        assert_eq!(found[1].markers, [".cursorrules"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_nested_markers() {
        let root = fixture("nested", &[".github/prompts/", ".github/workflows/"]);
        let found = detect(&root, Scope::Project).unwrap();
        assert_eq!(ids(&found), ["copilot"]);
        assert_eq!(found[0].markers, [".github/prompts/"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn global_scope_uses_global_markers() {
        let root = fixture("global", &[".codex/", ".config/opencode/", "CLAUDE.md"]);
        let found = detect(&root, Scope::Global).unwrap();
        assert_eq!(ids(&found), ["codex", "opencode"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_dir_detects_nothing() {
        let root = fixture("empty", &[]);
        assert!(detect(&root, Scope::Project).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_file_root_is_an_error() {
        let root = fixture("errors", &["file.txt"]);
        assert!(detect(&root.join("missing"), Scope::Project).is_err());
        let err = detect(&root.join("file.txt"), Scope::Project).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
        fs::remove_dir_all(root).unwrap();
    }

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            vars.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    fn skills_dir(id: &str, vars: &[(&str, &str)]) -> PathBuf {
        let p = provider(id).unwrap();
        let rel = p.skills.unwrap().1;
        scoped_path_with(p, Scope::Global, Path::new("/home/u"), rel, &lookup(vars))
    }

    #[test]
    fn global_skills_follow_config_home_variables() {
        let claude = skills_dir("claude-code", &[("CLAUDE_CONFIG_DIR", "/cfg/claude")]);
        assert_eq!(claude, Path::new("/cfg/claude/skills"));
        let copilot = skills_dir("copilot", &[("COPILOT_HOME", "/cfg/cp")]);
        assert_eq!(copilot, Path::new("/cfg/cp/skills"));
        let gemini = skills_dir("gemini-cli", &[("GEMINI_CLI_HOME", "/cfg/g")]);
        assert_eq!(gemini, Path::new("/cfg/g/.gemini/skills"));
    }

    #[test]
    fn codex_home_does_not_move_agents_skills() {
        let dir = skills_dir("codex", &[("CODEX_HOME", "/cfg/codex")]);
        assert_eq!(dir, Path::new("/home/u/.agents/skills"));
    }

    #[test]
    fn unset_empty_or_relative_variable_falls_back_to_home() {
        for vars in [
            &[][..],
            &[("CLAUDE_CONFIG_DIR", "")],
            &[("CLAUDE_CONFIG_DIR", "rel/dir")],
        ] {
            assert_eq!(
                skills_dir("claude-code", vars),
                Path::new("/home/u/.claude/skills")
            );
        }
    }

    #[test]
    fn project_scope_ignores_variables() {
        let p = provider("claude-code").unwrap();
        let path = scoped_path_with(
            p,
            Scope::Project,
            Path::new("/proj"),
            ".claude/skills",
            &lookup(&[("CLAUDE_CONFIG_DIR", "/cfg")]),
        );
        assert_eq!(path, Path::new("/proj/.claude/skills"));
    }

    #[test]
    fn global_detection_looks_in_overridden_location() {
        let home = fixture("env-home", &[]);
        let cfg = fixture("env-cfg", &[".claude/"]);
        let cfg_dir = cfg.join(".claude");
        let vars = [("CLAUDE_CONFIG_DIR", cfg_dir.to_str().unwrap())];
        let found = detect_with(&home, Scope::Global, &lookup(&vars)).unwrap();
        assert_eq!(ids(&found), ["claude-code"]);
        assert_eq!(found[0].markers, [".claude/"]);
        fs::remove_dir_all(home).unwrap();
        fs::remove_dir_all(cfg).unwrap();
    }

    #[test]
    fn strip_components_matches_whole_components_only() {
        assert_eq!(
            strip_components(".claude/skills", ".claude"),
            Some("/skills")
        );
        assert_eq!(strip_components(".claude.json", ".claude"), None);
        assert_eq!(strip_components("x/y", ""), Some("x/y"));
    }
}
