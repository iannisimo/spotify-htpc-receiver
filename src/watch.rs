//! `htpc-nowplaying watch`: stays connected to Soloist and launches the UI when a cast starts.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tokio::sync::mpsc;

use crate::config::Config;
use crate::instance;
use crate::model::{Playback, PlayerEvent};
use crate::player;

/// Starts the UI and tells whether one is running.
pub trait Launcher {
    fn launch_ui(&self) -> io::Result<()>;
    fn ui_running(&self) -> bool;
}

/// Spots the start of a cast session. A session starts when the device becomes active (or,
/// should that event be missed, when playback starts while it was not) and ends when the
/// device goes inactive. Each session is reported once.
#[derive(Debug, Default)]
pub struct CastDetector {
    active: bool,
}

impl CastDetector {
    /// True when `ev` starts a new cast session.
    pub fn on_event(&mut self, ev: &PlayerEvent) -> bool {
        let active = match ev {
            PlayerEvent::DeviceActive(active) => *active,
            PlayerEvent::PlaybackChanged(Playback::Playing) => true,
            // Connection changes say nothing about the cast; the reconnect's state decides.
            _ => return false,
        };
        let started = active && !self.active;
        self.active = active;
        started
    }
}

/// Watches Soloist forever (reconnecting as needed) and launches the UI at the start of each
/// cast session, unless one is already running. A failed launch is logged; the next session
/// tries again.
pub async fn watch(cfg: Config, launcher: impl Launcher) {
    let (events_tx, mut events) = mpsc::channel(64);
    let _client = player::spawn(cfg.daemon, events_tx);
    let mut detector = CastDetector::default();
    while let Some(ev) = events.recv().await {
        if detector.on_event(&ev) {
            start_ui(&launcher);
        }
    }
}

fn start_ui(launcher: &impl Launcher) {
    if launcher.ui_running() {
        log::info!("cast started; the UI is already running");
        return;
    }
    match launcher.launch_ui() {
        Ok(()) => log::info!("cast started; launched the UI"),
        Err(e) => log::warn!("cast started but the UI could not be launched: {e}"),
    }
}

/// Runs `htpc-nowplaying run` (this same binary) as a child that inherits the watcher's
/// environment, so it reaches the same Wayland/X display. Run the watcher inside the graphical
/// session (the systemd user unit is bound to `graphical-session.target`).
pub struct ProcessLauncher {
    exe: PathBuf,
    lock: PathBuf,
}

impl ProcessLauncher {
    pub fn new() -> io::Result<ProcessLauncher> {
        Ok(ProcessLauncher {
            exe: std::env::current_exe()?,
            lock: instance::default_lock_path(),
        })
    }
}

impl Launcher for ProcessLauncher {
    fn launch_ui(&self) -> io::Result<()> {
        let mut child = Command::new(&self.exe)
            .arg("run")
            .stdin(Stdio::null())
            .spawn()?;
        // Reap it when it exits, so no zombie is left behind.
        std::thread::Builder::new()
            .name("ui-reaper".to_owned())
            .spawn(move || match child.wait() {
                Ok(status) if !status.success() => log::warn!("the UI exited with {status}"),
                Ok(_) => {}
                Err(e) => log::warn!("cannot wait for the UI: {e}"),
            })?;
        Ok(())
    }

    fn ui_running(&self) -> bool {
        instance::is_held(&self.lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn track() -> Track {
        Track {
            title: "Heat Waves".to_owned(),
            artist: "Glass Animals".to_owned(),
            album: None,
            cover_url: None,
            duration_ms: 1000,
        }
    }

    /// Which of `events` start a session.
    fn starts(d: &mut CastDetector, events: &[PlayerEvent]) -> Vec<bool> {
        events.iter().map(|ev| d.on_event(ev)).collect()
    }

    #[test]
    fn device_becoming_active_starts_a_session_once() {
        let mut d = CastDetector::default();
        let seen = starts(
            &mut d,
            &[
                PlayerEvent::Connected,
                PlayerEvent::DeviceActive(false),
                PlayerEvent::DeviceActive(true),
                // Soloist repeats is_active in every playback_state.
                PlayerEvent::DeviceActive(true),
                PlayerEvent::TrackChanged(Some(track())),
                PlayerEvent::PlaybackChanged(Playback::Playing),
                PlayerEvent::PlaybackChanged(Playback::Paused),
                PlayerEvent::PlaybackChanged(Playback::Playing),
            ],
        );
        assert_eq!(
            seen,
            [false, false, true, false, false, false, false, false]
        );
    }

    #[test]
    fn active_at_connect_is_a_session() {
        // The watcher started while a cast was already going on.
        let mut d = CastDetector::default();
        assert_eq!(
            starts(
                &mut d,
                &[PlayerEvent::Connected, PlayerEvent::DeviceActive(true)]
            ),
            [false, true]
        );
    }

    #[test]
    fn playing_while_inactive_starts_a_session() {
        let mut d = CastDetector::default();
        assert!(d.on_event(&PlayerEvent::PlaybackChanged(Playback::Playing)));
        assert!(
            !d.on_event(&PlayerEvent::DeviceActive(true)),
            "same session"
        );
    }

    #[test]
    fn paused_or_track_while_inactive_does_not() {
        let mut d = CastDetector::default();
        let seen = starts(
            &mut d,
            &[
                PlayerEvent::PlaybackChanged(Playback::Paused),
                PlayerEvent::PlaybackChanged(Playback::Buffering),
                PlayerEvent::TrackChanged(Some(track())),
                PlayerEvent::VolumeChanged(40),
            ],
        );
        assert_eq!(seen, [false; 4]);
    }

    #[test]
    fn inactive_ends_the_session_and_the_next_cast_counts() {
        let mut d = CastDetector::default();
        assert!(d.on_event(&PlayerEvent::DeviceActive(true)));
        assert!(!d.on_event(&PlayerEvent::DeviceActive(false)));
        assert!(d.on_event(&PlayerEvent::DeviceActive(true)));
    }

    #[test]
    fn losing_the_connection_does_not_end_the_session() {
        // A dropped WebSocket says nothing about the cast; the reconnect's state decides.
        let mut d = CastDetector::default();
        let seen = starts(
            &mut d,
            &[
                PlayerEvent::DeviceActive(true),
                PlayerEvent::Disconnected,
                PlayerEvent::Connected,
                PlayerEvent::DeviceActive(true),
            ],
        );
        assert_eq!(seen, [true, false, false, false]);
    }
}
