//! Post-start verification that egress rules are actually enforced.
//!
//! The nftables ruleset is loaded by an OCI hook outside pithos' control, and
//! podman can silently skip hooks it does not know about. After the container
//! starts, pithos reads the live ruleset from its network namespace through
//! the same `podman unshare` path the hook itself uses. A table without the
//! private range drops means the hook never ran, and the session is killed
//! instead of running unprotected.
//!
//! The check runs once on a fire-and-forget thread: it is bounded by the
//! startup poll timeout plus a single nft query, and being abandoned at
//! process exit is harmless because every failure path already leaves
//! nothing to protect.

use std::process::Command;
use std::process::Output;
use std::thread;
use std::time::Duration;

use super::{PRIVATE_V4_RANGES, TABLE};
use crate::config::Networking;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

enum Verdict {
    Compliant,
    Inconclusive(String),
    Violated,
}

/// Schedules the one-shot enforcement check unless there is nothing to
/// verify. Returns immediately; the caller never waits on this thread.
///
/// Without `block_private` there is no cheap observable difference between an
/// enforced and an unenforced netns, so nothing is checked.
pub(crate) fn spawn_check(networking: &Networking, container_name: String) {
    if !networking.enabled {
        tracing::debug!("networking disabled; skipping enforcement check");
        return;
    }
    if !networking.block_private {
        tracing::debug!("block_private disabled; skipping enforcement check");
        return;
    }
    thread::spawn(move || check(container_name));
}

fn check(container_name: String) {
    let Some(pid) = wait_for_pid(&container_name) else {
        tracing::debug!("container vanished or did not start; skipping enforcement check");
        return;
    };
    match verdict(&pid) {
        Verdict::Compliant => {
            tracing::info!("egress enforcement verified inside the sandbox netns");
        }
        Verdict::Inconclusive(reason) => {
            tracing::warn!(
                reason,
                "could not verify egress enforcement; the session continues without this guarantee"
            );
        }
        Verdict::Violated => {
            tracing::error!(
                container = %container_name,
                "egress rules are not enforced while block_private is configured; most likely \
                 podman never invoked the OCI hook; debug with 'podman --log-level=debug run \
                 --rm alpine true 2>&1 | grep -i hook'; stopping the session; bypass \
                 deliberately with [networking] enabled = false"
            );
            let _ = Command::new("podman")
                .args(["kill", &container_name])
                .status();
        }
    }
}

/// Blocks until the container reports `running`, then returns its pid.
fn wait_for_pid(container_name: &str) -> Option<String> {
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(pid) = running_pid(container_name) {
            return Some(pid);
        }
        if !container_exists(container_name) {
            return None;
        }
        if std::time::Instant::now() >= deadline {
            tracing::debug!("container did not start before the enforcement check timed out");
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn inspect_state(container_name: &str, format: &str) -> Option<String> {
    let output = Command::new("podman")
        .args(["inspect", "--format", format, container_name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn running_pid(container_name: &str) -> Option<String> {
    match inspect_state(container_name, "{{.State.Status}}")?.as_str() {
        "running" => {
            let pid = inspect_state(container_name, "{{.State.Pid}}")?;
            (pid != "0").then_some(pid)
        }
        _ => None,
    }
}

fn container_exists(container_name: &str) -> bool {
    inspect_state(container_name, "{{.State.Status}}").is_some()
}

fn verdict(pid: &str) -> Verdict {
    let output = Command::new("podman")
        .args([
            "unshare", "nsenter", "-t", pid, "-n", "nft", "list", "table", "inet", TABLE,
        ])
        .output();
    match output {
        Ok(output) => classify(&output),
        Err(error) => Verdict::Inconclusive(format!("nft query could not run: {error}")),
    }
}

fn classify(output: &Output) -> Verdict {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        if private_drops_present(&stdout) {
            Verdict::Compliant
        } else {
            Verdict::Violated
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if stderr.contains("no such") {
            Verdict::Violated
        } else {
            Verdict::Inconclusive(format!("nft list failed: {}", stderr.trim()))
        }
    }
}

fn private_drops_present(ruleset: &str) -> bool {
    ruleset.contains("drop")
        && PRIVATE_V4_RANGES
            .iter()
            .all(|range| ruleset.contains(range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn output(status_code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(status_code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn loaded_ruleset() -> String {
        format!(
            "table inet {TABLE} {{\n  chain output {{\n    ip daddr {{ {} }} drop\n  }}\n}}\n",
            PRIVATE_V4_RANGES.join(", ")
        )
    }

    #[test]
    fn loaded_table_with_private_drops_is_compliant() {
        let result = classify(&output(0, &loaded_ruleset(), ""));
        assert!(matches!(result, Verdict::Compliant));
    }

    #[test]
    fn missing_table_is_a_violation() {
        let result = classify(&output(
            1,
            "",
            "Error: No such file or directory (os error 2)",
        ));
        assert!(matches!(result, Verdict::Violated));
    }

    #[test]
    fn table_without_the_private_ranges_is_a_violation() {
        let result = classify(&output(
            0,
            "table inet pithos-egress {\n  chain output {\n  }\n}\n",
            "",
        ));
        assert!(matches!(result, Verdict::Violated));
    }

    #[test]
    fn broken_nft_query_is_inconclusive() {
        let result = classify(&output(127, "", "nsenter: failed to setns"));
        assert!(matches!(result, Verdict::Inconclusive(_)));
    }
}
