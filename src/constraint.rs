//! Composer-style version constraints (`^1.2`, `~1.2.3`, `1.2.*`, `>=1.0 <2.0 || ^3.0`, `dev-main`).
//!
//! Semantics follow Composer, which differs from Cargo for `~1.2` (`>=1.2.0 <2.0.0`), so
//! `semver::VersionReq` is not reused; `semver::Version` provides parsing and ordering.

use semver::{BuildMetadata, Prerelease, Version};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    /// Head of a branch: `dev-<branch>`.
    Branch(String),
    /// Version range: any alternative (`||`) whose comparators all match.
    Range {
        alternatives: Vec<Vec<Comparator>>,
        /// Pre-release versions only match when the constraint itself names one.
        prerelease: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparator {
    op: Op,
    version: Version,
}

impl Comparator {
    fn matches(&self, v: &Version) -> bool {
        let target = &self.version;
        match self.op {
            Op::Eq => v == target,
            Op::Ne => v != target,
            Op::Gt => v > target,
            Op::Ge => v >= target,
            Op::Lt => v < target,
            Op::Le => v <= target,
        }
    }
}

impl Constraint {
    pub fn parse(input: &str) -> Result<Self, String> {
        let invalid = |reason: String| format!("invalid version constraint '{input}': {reason}");
        let s = input.trim();
        if let Some(branch) = s.strip_prefix("dev-") {
            if !valid_branch(branch) {
                return Err(invalid(format!("bad branch name '{branch}'")));
            }
            return Ok(Self::Branch(branch.to_string()));
        }
        let mut alternatives = Vec::new();
        for alt in s.split('|').map(str::trim).filter(|a| !a.is_empty()) {
            let comparators = match alt.split_once(" - ") {
                Some((low, high)) => hyphen(low.trim(), high.trim()),
                None => and_list(alt),
            }
            .map_err(invalid)?;
            alternatives.push(comparators);
        }
        if alternatives.is_empty() {
            return Err(invalid("empty".into()));
        }
        let prerelease = alternatives
            .iter()
            .flatten()
            .any(|c| !c.version.pre.is_empty());
        Ok(Self::Range {
            alternatives,
            prerelease,
        })
    }

    /// Whether a tagged version satisfies this constraint. Branch constraints match no tag.
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Self::Branch(_) => false,
            Self::Range {
                alternatives,
                prerelease,
            } => {
                (v.pre.is_empty() || *prerelease)
                    && alternatives
                        .iter()
                        .any(|all| all.iter().all(|c| c.matches(v)))
            }
        }
    }
}

/// Whether a locked version label (`1.2.3` or `dev-main`) satisfies `constraint`.
pub fn satisfies(constraint: &str, version: &str) -> bool {
    match Constraint::parse(constraint) {
        Ok(Constraint::Branch(branch)) => version.strip_prefix("dev-") == Some(branch.as_str()),
        Ok(range) => parse_version(version).is_some_and(|v| range.matches(&v)),
        Err(_) => false,
    }
}

/// Parses a git tag such as `v1.2` or `1.2.3-beta.1` into a version; `None` if it is not one.
pub fn parse_version(tag: &str) -> Option<Version> {
    Partial::parse(tag).ok().map(|p| p.floor())
}

/// Constraint written to aip.json when the user gives none: `^major.minor` of the latest version.
pub fn default_for(v: &Version) -> String {
    match (v.major, v.minor) {
        (0, 0) => format!("^0.0.{}", v.patch),
        (major, minor) => format!("^{major}.{minor}"),
    }
}

fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
}

/// `>=1.0 <2.0`, `>=1.0,<2.0`, `>= 1.0`.
fn and_list(alt: &str) -> Result<Vec<Comparator>, String> {
    let mut comparators = Vec::new();
    let mut pending_op = String::new();
    for token in alt
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
    {
        if token.chars().all(|c| "<>=!".contains(c)) {
            pending_op = token.to_string();
            continue;
        }
        comparators.extend(atom(&format!("{pending_op}{token}"))?);
        pending_op.clear();
    }
    if !pending_op.is_empty() {
        return Err(format!("'{pending_op}' without a version"));
    }
    Ok(comparators)
}

fn atom(token: &str) -> Result<Vec<Comparator>, String> {
    if token == "*" {
        return Ok(Vec::new());
    }
    if let Some(rest) = token.strip_prefix('^') {
        let p = Partial::parse(rest)?;
        let upper = match (p.major, p.minor, p.patch) {
            (major, _, _) if major > 0 => version(major + 1, 0, 0),
            (_, Some(minor), _) if minor > 0 => version(0, minor + 1, 0),
            (_, Some(_), Some(patch)) => version(0, 0, patch + 1),
            (_, Some(_), None) => version(0, 1, 0),
            (_, None, _) => version(1, 0, 0),
        };
        return Ok(range(p.floor(), upper));
    }
    if let Some(rest) = token.strip_prefix('~') {
        let p = Partial::parse(rest)?;
        let upper = match (p.minor, p.patch) {
            (Some(minor), Some(_)) => version(p.major, minor + 1, 0),
            _ => version(p.major + 1, 0, 0),
        };
        return Ok(range(p.floor(), upper));
    }
    if let Some(prefix) = token
        .strip_suffix(".*")
        .or_else(|| token.strip_suffix(".x"))
    {
        let p = Partial::parse(prefix)?;
        let upper = match (p.minor, p.patch) {
            (_, Some(_)) => return Err(format!("too many version parts in '{token}'")),
            (Some(minor), None) => version(p.major, minor + 1, 0),
            (None, None) => version(p.major + 1, 0, 0),
        };
        return Ok(range(p.floor(), upper));
    }
    const OPS: [(&str, Op); 8] = [
        (">=", Op::Ge),
        ("<=", Op::Le),
        ("!=", Op::Ne),
        ("<>", Op::Ne),
        ("==", Op::Eq),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Eq),
    ];
    let (op, rest) = OPS
        .iter()
        .find_map(|(sym, op)| token.strip_prefix(sym).map(|rest| (*op, rest)))
        .unwrap_or((Op::Eq, token));
    Ok(vec![Comparator {
        op,
        version: Partial::parse(rest)?.floor(),
    }])
}

/// `1.0 - 2.0`: inclusive; a partial upper bound includes everything it prefixes (`< 2.1.0`).
fn hyphen(low: &str, high: &str) -> Result<Vec<Comparator>, String> {
    let low = Partial::parse(low)?;
    let high = Partial::parse(high)?;
    let upper = match (high.minor, high.patch) {
        (Some(_), Some(_)) => Comparator {
            op: Op::Le,
            version: high.floor(),
        },
        (Some(minor), None) => Comparator {
            op: Op::Lt,
            version: version(high.major, minor + 1, 0),
        },
        (None, _) => Comparator {
            op: Op::Lt,
            version: version(high.major + 1, 0, 0),
        },
    };
    Ok(vec![
        Comparator {
            op: Op::Ge,
            version: low.floor(),
        },
        upper,
    ])
}

fn range(low: Version, high: Version) -> Vec<Comparator> {
    vec![
        Comparator {
            op: Op::Ge,
            version: low,
        },
        Comparator {
            op: Op::Lt,
            version: high,
        },
    ]
}

fn version(major: u64, minor: u64, patch: u64) -> Version {
    Version::new(major, minor, patch)
}

/// A possibly incomplete version such as `1`, `1.2` or `v1.2.3-beta`.
struct Partial {
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Prerelease,
}

impl Partial {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let s = s.split('+').next().unwrap_or(s);
        let (numbers, pre) = match s.split_once('-') {
            Some((numbers, pre)) => (
                numbers,
                Prerelease::new(pre).map_err(|_| format!("bad pre-release in '{s}'"))?,
            ),
            None => (s, Prerelease::EMPTY),
        };
        let parts = numbers
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| format!("'{s}' is not a version"))?;
        match parts[..] {
            [major] => Ok(Self::new(major, None, None, pre)),
            [major, minor] => Ok(Self::new(major, Some(minor), None, pre)),
            [major, minor, patch] => Ok(Self::new(major, Some(minor), Some(patch), pre)),
            _ => Err(format!("'{s}' is not a version")),
        }
    }

    fn new(major: u64, minor: Option<u64>, patch: Option<u64>, pre: Prerelease) -> Self {
        Self {
            major,
            minor,
            patch,
            pre,
        }
    }

    fn floor(&self) -> Version {
        Version {
            major: self.major,
            minor: self.minor.unwrap_or(0),
            patch: self.patch.unwrap_or(0),
            pre: self.pre.clone(),
            build: BuildMetadata::EMPTY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(constraint: &str, matching: &[&str], rejected: &[&str]) {
        let c = Constraint::parse(constraint).unwrap();
        for v in matching {
            assert!(
                c.matches(&parse_version(v).unwrap()),
                "{constraint} should match {v}"
            );
        }
        for v in rejected {
            assert!(
                !c.matches(&parse_version(v).unwrap()),
                "{constraint} should reject {v}"
            );
        }
    }

    #[test]
    fn caret() {
        check("^1.2.3", &["1.2.3", "1.9.0"], &["1.2.2", "2.0.0"]);
        check("^1.2", &["1.2.0", "1.99.0"], &["1.1.9", "2.0.0"]);
        check("^0.3", &["0.3.0", "0.3.9"], &["0.2.9", "0.4.0"]);
        check("^0.0.3", &["0.0.3"], &["0.0.4", "0.0.2"]);
    }

    #[test]
    fn tilde_follows_composer_not_cargo() {
        check("~1.2", &["1.2.0", "1.9.0"], &["1.1.0", "2.0.0"]);
        check("~1.2.3", &["1.2.3", "1.2.9"], &["1.3.0", "1.2.2"]);
        check("~1", &["1.0.0", "1.5.0"], &["2.0.0"]);
    }

    #[test]
    fn wildcards_and_exact() {
        check("1.2.*", &["1.2.0", "1.2.9"], &["1.3.0", "1.1.0"]);
        check("1.*", &["1.0.0", "1.9.9"], &["2.0.0"]);
        check("*", &["0.0.1", "9.0.0"], &[]);
        check("1.2.3", &["1.2.3"], &["1.2.4"]);
        check("=1.2", &["1.2.0"], &["1.2.1"]);
    }

    #[test]
    fn operators_and_combinations() {
        check(">=1.0 <2.0", &["1.0.0", "1.9.9"], &["0.9.0", "2.0.0"]);
        check(">=1.0,<2.0", &["1.5.0"], &["2.0.0"]);
        check(">= 1.0", &["1.0.0"], &["0.9.0"]);
        check("!=1.5.0 ^1.0", &["1.4.0", "1.6.0"], &["1.5.0"]);
        check(">1.0 <=1.2", &["1.1.0", "1.2.0"], &["1.0.0", "1.2.1"]);
        check("^1.0 || ^3.0", &["1.1.0", "3.2.0"], &["2.0.0"]);
        check("^1.0 | ^3.0", &["3.0.0"], &["2.5.0"]);
    }

    #[test]
    fn hyphen_ranges() {
        check("1.0 - 2.0", &["1.0.0", "2.0.9"], &["2.1.0", "0.9.0"]);
        check("1.0.0 - 2.1.0", &["2.1.0"], &["2.1.1"]);
        check("1 - 2", &["2.9.0"], &["3.0.0"]);
    }

    #[test]
    fn prereleases_need_explicit_opt_in() {
        check("^1.0", &["1.1.0"], &["1.1.0-beta"]);
        check(
            ">=2.0.0-beta",
            &["2.0.0-beta", "2.0.0-rc.1", "2.0.0"],
            &["2.0.0-alpha"],
        );
    }

    #[test]
    fn tags_and_branches() {
        assert_eq!(parse_version("v1.2"), Some(Version::new(1, 2, 0)));
        assert_eq!(parse_version("V3"), Some(Version::new(3, 0, 0)));
        assert_eq!(parse_version("release-1"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("hello/v1.0.0"), None);
        assert_eq!(
            Constraint::parse("dev-main"),
            Ok(Constraint::Branch("main".into()))
        );
        assert!(Constraint::parse("dev--x").is_err());
        assert!(Constraint::parse("dev-a..b").is_err());
    }

    #[test]
    fn invalid_constraints() {
        for bad in ["", "abc", "^", ">=", "1.2.3.*", "~x", "1.2.3.4"] {
            assert!(Constraint::parse(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn locked_versions() {
        assert!(satisfies("^1.0", "1.4.0"));
        assert!(!satisfies("^2.0", "1.4.0"));
        assert!(satisfies("dev-main", "dev-main"));
        assert!(!satisfies("dev-main", "dev-next"));
        assert!(!satisfies("^1.0", "dev-main"));
    }

    #[test]
    fn defaults() {
        assert_eq!(default_for(&Version::new(1, 2, 3)), "^1.2");
        assert_eq!(default_for(&Version::new(0, 3, 1)), "^0.3");
        assert_eq!(default_for(&Version::new(0, 0, 4)), "^0.0.4");
    }
}
