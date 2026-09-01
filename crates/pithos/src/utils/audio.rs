//! Optional host-audio passthrough for sessions.
//!
//! OpenCode renders attention sounds through miniaudio, which speaks the
//! PulseAudio protocol by way of `libpulse`. On Linux desktops that protocol
//! is served by PipeWire's compatibility layer at
//! `$XDG_RUNTIME_DIR/pulse/native`, so enabling `audio` bind-mounts that
//! socket into the session and points clients at it.

use std::path::Path;

pub(crate) struct Passthrough {
    pub(crate) volume: String,
    pub(crate) env: Vec<(String, String)>,
}

/// Resolves the audio passthrough for a session.
///
/// Returns `None` when the feature is disabled or unavailable; unavailability
/// is reported on stderr and never blocks the session.
pub(crate) fn passthrough(enabled: bool) -> Option<Passthrough> {
    if !enabled {
        return None;
    }
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
    passthrough_in(runtime_dir.as_deref().map(Path::new))
}

#[cfg(target_os = "linux")]
fn passthrough_in(runtime_dir: Option<&Path>) -> Option<Passthrough> {
    let host_socket = match runtime_dir.map(|dir| dir.join("pulse/native")) {
        Some(socket) if socket.exists() => socket,
        _ => {
            eprintln!(
                "warning: audio = true but no PulseAudio socket was found under \
                 $XDG_RUNTIME_DIR; running without sound"
            );
            return None;
        }
    };
    let uid = crate::utils::platform::current_uid();
    let guest_dir = format!("/run/user/{uid}");
    let guest_socket = format!("{guest_dir}/pulse/native");
    Some(Passthrough {
        volume: format!("{}:{guest_socket}:rw,Z", host_socket.display()),
        env: vec![
            ("XDG_RUNTIME_DIR".to_string(), guest_dir),
            ("PULSE_SERVER".to_string(), format!("unix:{guest_socket}")),
        ],
    })
}

#[cfg(not(target_os = "linux"))]
fn passthrough_in(_runtime_dir: Option<&Path>) -> Option<Passthrough> {
    eprintln!("warning: audio = true requires a Linux host; running without sound");
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::sandbox::TempDir;

    #[test]
    fn builds_mount_and_env_for_existing_socket() {
        let dir = TempDir::create("pithos-test-audio-socket").unwrap();
        let socket = dir.path().join("pulse/native");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::fs::write(&socket, "").unwrap();

        let passthrough =
            passthrough_in(Some(dir.path())).expect("passthrough for an existing socket");
        let uid = crate::utils::platform::current_uid();
        assert_eq!(
            passthrough.volume,
            format!("{}:/run/user/{uid}/pulse/native:rw,Z", socket.display())
        );
        assert_eq!(
            passthrough.env,
            vec![
                ("XDG_RUNTIME_DIR".to_string(), format!("/run/user/{uid}")),
                (
                    "PULSE_SERVER".to_string(),
                    format!("unix:/run/user/{uid}/pulse/native")
                ),
            ]
        );
    }

    #[test]
    fn skips_when_the_socket_is_missing() {
        let dir = TempDir::create("pithos-test-audio-empty").unwrap();
        assert!(passthrough_in(Some(dir.path())).is_none());
    }

    #[test]
    fn skips_without_a_runtime_directory() {
        assert!(passthrough_in(None).is_none());
    }
}
