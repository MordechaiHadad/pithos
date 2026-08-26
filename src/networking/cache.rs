use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

pub(crate) const TTL: Duration = Duration::from_secs(5 * 60);
pub(crate) const REFRESH_WINDOW: Duration = Duration::from_secs(60);

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub(crate) hosts: Vec<String>,
    pub(crate) addresses: Vec<IpAddr>,
    pub(crate) resolved_at: u64,
}

impl Entry {
    pub(crate) fn is_fresh(&self, current_time: u64) -> bool {
        current_time.saturating_sub(self.resolved_at) < TTL.as_secs()
    }

    pub(crate) fn needs_refresh(&self, current_time: u64) -> bool {
        current_time.saturating_sub(self.resolved_at)
            >= TTL.saturating_sub(REFRESH_WINDOW).as_secs()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct File {
    version: u32,
    entry: Entry,
}

pub(crate) fn load(hosts: &[String]) -> Option<Entry> {
    let path = cache_path()?;
    let contents = fs::read_to_string(path).ok()?;
    let file = serde_json::from_str::<File>(&contents).ok()?;
    if file.version == VERSION && file.entry.hosts == hosts {
        Some(file.entry)
    } else {
        None
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn save(entry: Entry) -> Result<()> {
    let path = cache_path().ok_or_else(|| eyre::eyre!("cannot determine pithos data directory"))?;
    let directory = path
        .parent()
        .ok_or_else(|| eyre::eyre!("pithos cache path has no parent directory"))?;
    fs::create_dir_all(directory).wrap_err("cannot create pithos data directory")?;
    let temporary = directory.join(format!("net_cache.json.{}.tmp", std::process::id()));
    let contents = serde_json::to_vec_pretty(&File {
        version: VERSION,
        entry,
    })
    .wrap_err("cannot serialize network cache")?;
    fs::write(&temporary, contents).wrap_err("cannot write temporary network cache")?;
    fs::rename(&temporary, path).wrap_err("cannot install network cache")?;
    Ok(())
}

fn cache_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|directory| directory.join("pithos").join("net_cache.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(resolved_at: u64) -> Entry {
        Entry {
            hosts: vec!["example.com".into()],
            addresses: vec!["192.0.2.1".parse().unwrap()],
            resolved_at,
        }
    }

    #[test]
    fn fresh_entries_become_refreshable_before_expiring() {
        let resolved = entry(1_000);
        assert!(resolved.is_fresh(1_000 + TTL.as_secs() - 1));
        assert!(resolved.needs_refresh(1_000 + TTL.as_secs() - REFRESH_WINDOW.as_secs()));
    }

    #[test]
    fn expired_entries_are_not_fresh() {
        let resolved = entry(1_000);
        assert!(!resolved.is_fresh(1_000 + TTL.as_secs()));
    }
}
