//! A registry that lists exactly one repository, written inline in aip.json.
//!
//! ```json
//! { "type": "git", "url": "git@github.com:acme/private-skills.git" }
//! ```
//!
//! The repository name is derived from the URL (`acme/private-skills`); add `"name"` to override it.

use super::{Entry, RawEntry, Registry, validated_entry};
use crate::Result;
use std::path::PathBuf;

/// Single repository declared in aip.json (like a Composer `vcs` repository).
pub struct GitRepositoryRegistry {
    /// Optional override; derived from `url` when absent.
    pub name: Option<String>,
    pub url: String,
    /// Folder of aip.json; relative local URLs resolve against it.
    pub base: PathBuf,
}

impl Registry for GitRepositoryRegistry {
    fn location(&self) -> String {
        self.base
            .join(crate::manifest::MANIFEST_FILE)
            .display()
            .to_string()
    }

    fn repositories(&self) -> Result<Vec<Entry>> {
        let entry = RawEntry {
            name: self.name.clone(),
            url: self.url.clone(),
        };
        Ok(vec![validated_entry(entry, &self.base, &self.location())?])
    }
}
