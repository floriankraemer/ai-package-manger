//! On-disk copy of downloaded registry documents, for revalidation and offline use.
//!
//! One file per URL: `<cache dir>/<fnv1a(url) as 016x>.json`. It never holds credentials.

use crate::{Context, Result, fnv1a};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Unix seconds of the last successful download or revalidation.
    pub fetched_at: u64,
    /// The registry document exactly as the server sent it.
    pub body: String,
}

fn path(dir: &Path, url: &str) -> PathBuf {
    dir.join(format!("{:016x}.json", fnv1a(url.as_bytes())))
}

/// The entry for exactly this URL; a missing, unreadable or foreign file counts as no entry.
pub fn load(dir: &Path, url: &str) -> Option<CacheEntry> {
    let text = fs::read_to_string(path(dir, url)).ok()?;
    let entry: CacheEntry = serde_json::from_str(&text).ok()?;
    (entry.url == url).then_some(entry)
}

/// Writes the entry atomically: temp file in the same folder, then rename.
pub fn store(dir: &Path, entry: &CacheEntry) -> Result<()> {
    fs::create_dir_all(dir).context(|| format!("cannot create {}", dir.display()))?;
    let target = path(dir, &entry.url);
    let temp = target.with_extension(format!("tmp-{}", std::process::id()));
    let json = serde_json::to_string(entry).context(|| "cannot serialize cache entry".into())?;
    fs::write(&temp, json).context(|| format!("cannot write {}", temp.display()))?;
    fs::rename(&temp, &target).context(|| format!("cannot write {}", target.display()))
}

/// Current time in Unix seconds.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `5 minutes`, `1 hour`, `3 days`; under a minute reads `less than a minute`.
pub fn describe_age(seconds: u64) -> String {
    let plural = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match seconds {
        0..60 => "less than a minute".into(),
        60..3600 => plural(seconds / 60, "minute"),
        3600..86_400 => plural(seconds / 3600, "hour"),
        _ => plural(seconds / 86_400, "day"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::temp_dir;

    fn entry(url: &str, body: &str) -> CacheEntry {
        CacheEntry {
            url: url.into(),
            etag: Some("\"v1\"".into()),
            last_modified: None,
            fetched_at: 42,
            body: body.into(),
        }
    }

    #[test]
    fn stores_and_loads_by_exact_url() {
        let dir = temp_dir("cache");
        assert_eq!(load(&dir, "https://h/a.json"), None);
        store(&dir, &entry("https://h/a.json", "{}")).unwrap();
        assert_eq!(
            load(&dir, "https://h/a.json"),
            Some(entry("https://h/a.json", "{}"))
        );
        assert_eq!(load(&dir, "https://h/b.json"), None);
        // No temp files are left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // A file for another URL under the same name (hash collision) is ignored.
        fs::write(
            path(&dir, "https://h/b.json"),
            serde_json::to_string(&entry("https://x", "{}")).unwrap(),
        )
        .unwrap();
        assert_eq!(load(&dir, "https://h/b.json"), None);
    }

    #[test]
    fn describes_age() {
        assert_eq!(describe_age(5), "less than a minute");
        assert_eq!(describe_age(60), "1 minute");
        assert_eq!(describe_age(300), "5 minutes");
        assert_eq!(describe_age(7200), "2 hours");
        assert_eq!(describe_age(3 * 86_400 + 5), "3 days");
    }
}
