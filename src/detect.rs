//! Detection of AI providers / agent harnesses by their on-disk configuration markers.

use serde::Serialize;
use std::env;
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
    /// Environment variable that relocates the provider's global config directory.
    pub global_env: Option<&'static str>,
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
        global_env: Some("CLAUDE_CONFIG_DIR"),
        skills: Some((".claude/skills", ".claude/skills")),
    },
    Provider {
        id: "codex",
        name: "OpenAI Codex",
        project: &[".codex/"],
        global: &[".codex/"],
        global_env: Some("CODEX_HOME"),
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
        global_env: None,
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
        global_env: None,
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
            let mut markers: Vec<String> = candidates
                .iter()
                .filter(|m| marker_path(root, m).exists())
                .map(|m| m.to_string())
                .collect();
            if scope == Scope::Global
                && let Some(var) = p.global_env
                && env::var_os(var).is_some_and(|dir| Path::new(&dir).is_dir())
            {
                markers.push(format!("${var}"));
            }
            (!markers.is_empty()).then_some(Detection {
                id: p.id,
                name: p.name,
                markers,
            })
        })
        .collect())
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
}
