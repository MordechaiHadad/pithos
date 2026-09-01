use eyre::Result;

pub(crate) fn ps() -> Result<()> {
    let sessions = crate::registry::prune()?;
    if sessions.is_empty() {
        println!("no running pithos sessions");
        return Ok(());
    }
    for session in &sessions {
        println!(
            "{:<30} {:<24} {:>8}  {}",
            session.identity.id,
            repo_label(session),
            uptime(unix_now(), session.lifecycle.started_at),
            session.paths.sandbox_path.display()
        );
    }
    Ok(())
}

fn repo_label(session: &crate::registry::SessionRecord) -> String {
    session
        .paths
        .repo_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| session.paths.repo_path.display().to_string())
}

fn uptime(now: u64, started_at: u64) -> String {
    let elapsed = now.saturating_sub(started_at);
    let hours = elapsed / 3600;
    let minutes = (elapsed % 3600) / 60;
    let seconds = elapsed % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_formats_seconds_minutes_hours() {
        assert_eq!(uptime(1000, 1000), "0s");
        assert_eq!(uptime(1061, 1000), "1m01s");
        assert_eq!(uptime(4723, 1000), "1h02m");
    }
}
