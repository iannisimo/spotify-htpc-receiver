//! `htpc-nowplaying watch`: stays connected to Soloist and launches the UI when a cast starts.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::config::{Config, PairingMode};
use crate::instance;
use crate::model::PlayerEvent;
use crate::pairing::{HelperCommand, HelperStatus, Pairing};
use crate::player;

/// How a UI the watcher launched ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiExit {
    /// Exit code 0: it was closed.
    Clean,
    /// A failure exit code or a signal.
    Crashed,
}

/// Called once, from any thread, when a launched UI exits.
pub type OnUiExit = Box<dyn FnOnce(UiExit) + Send>;

/// Starts the UI and tells whether one is running.
pub trait Launcher {
    fn launch_ui(&self, on_exit: OnUiExit) -> io::Result<()>;
    fn ui_running(&self) -> bool;
}

/// Wait before relaunching a crashed UI.
pub const RELAUNCH_DELAY: Duration = Duration::from_secs(1);
/// At most this many relaunches within [`CRASH_WINDOW`]; then the watcher waits for the next
/// cast, so a UI that cannot start does not loop forever.
pub const MAX_RELAUNCHES: usize = 3;
pub const CRASH_WINDOW: Duration = Duration::from_secs(60);

/// Counts relaunches after crashes, to stop a crash loop.
#[derive(Debug, Default)]
pub struct CrashGuard {
    relaunches: VecDeque<Instant>,
}

impl CrashGuard {
    /// Whether a crash at `now` may be followed by a relaunch (and counts it if so).
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .relaunches
            .front()
            .is_some_and(|&t| now.duration_since(t) >= CRASH_WINDOW)
        {
            self.relaunches.pop_front();
        }
        if self.relaunches.len() >= MAX_RELAUNCHES {
            return false;
        }
        self.relaunches.push_back(now);
        true
    }
}

/// Spots the start of a cast session: it starts when the device becomes active and ends when
/// it goes inactive. Each session is reported once. Playback events alone never start one:
/// while inactive, Soloist relays the playback of the active device (the phone) too.
#[derive(Debug, Default)]
pub struct CastDetector {
    active: bool,
}

impl CastDetector {
    /// True when `ev` starts a new cast session.
    pub fn on_event(&mut self, ev: &PlayerEvent) -> bool {
        // Connection changes say nothing about the cast; the reconnect's state decides.
        let PlayerEvent::DeviceActive(active) = *ev else {
            return false;
        };
        let started = active && !self.active;
        self.active = active;
        started
    }

    /// Whether a cast session is going on.
    pub fn active(&self) -> bool {
        self.active
    }
}

/// Watches Soloist forever (reconnecting as needed) and launches the UI at the start of each
/// cast session, unless one is already running. A failed launch is logged; the next session
/// tries again. If a UI it launched crashes while the cast goes on, it is launched again after
/// [`RELAUNCH_DELAY`], at most [`MAX_RELAUNCHES`] times per [`CRASH_WINDOW`]. A UI that was
/// closed (exit code 0) stays closed until the next cast.
///
/// It also keeps the pairing port open for a Soloist started by hand (the UI may be closed):
/// on each connection change it asks `pairing_mode` (the config file's current setting) and
/// `unit_active` (whether the systemd unit runs Soloist; the unit manages the port itself)
/// and runs `helper` as needed. Both `helper` and `unit_active` block, so they run on
/// blocking threads, one event at a time and in order.
pub async fn watch(
    cfg: Config,
    launcher: impl Launcher,
    helper: impl Fn(HelperCommand) -> HelperStatus + Send + Sync + 'static,
    unit_active: impl Fn() -> bool + Send + Sync + 'static,
    pairing_mode: impl Fn() -> PairingMode + Send + Sync + 'static,
) {
    let (events_tx, mut events) = mpsc::channel(64);
    let _client = player::spawn(cfg.daemon, events_tx);
    let (pairing_tx, pairing_rx) = mpsc::unbounded_channel();
    tokio::spawn(keep_pairing_port(
        pairing_rx,
        Arc::new(helper),
        Arc::new(unit_active),
        Arc::new(pairing_mode),
    ));
    let (exit_tx, mut exits) = mpsc::unbounded_channel();
    let mut detector = CastDetector::default();
    let mut crashes = CrashGuard::default();
    let mut relaunch_at: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                if matches!(ev, PlayerEvent::Connected | PlayerEvent::Disconnected) {
                    // The worker lives as long as this loop does.
                    let _ = pairing_tx.send(ev.clone());
                }
                if detector.on_event(&ev) {
                    crashes = CrashGuard::default();
                    relaunch_at = None;
                    start_ui(&launcher, &exit_tx);
                }
                if !detector.active() {
                    relaunch_at = None;
                }
            }
            Some(exit) = exits.recv() => {
                if exit == UiExit::Crashed && detector.active() {
                    if crashes.allow(Instant::now()) {
                        log::warn!("the UI crashed during a cast; relaunching it");
                        relaunch_at = Some(tokio::time::Instant::now() + RELAUNCH_DELAY);
                    } else {
                        log::warn!(
                            "the UI keeps crashing; not relaunching it until the next cast"
                        );
                    }
                }
            }
            () = sleep_until(relaunch_at), if relaunch_at.is_some() => {
                relaunch_at = None;
                start_ui(&launcher, &exit_tx);
            }
        }
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    if let Some(at) = at {
        tokio::time::sleep_until(at).await;
    }
}

type Helper = Arc<dyn Fn(HelperCommand) -> HelperStatus + Send + Sync>;

/// Handles connection changes one after the other, so an Open is never overtaken by the Close
/// before it.
async fn keep_pairing_port(
    mut events: mpsc::UnboundedReceiver<PlayerEvent>,
    helper: Helper,
    unit_active: Arc<dyn Fn() -> bool + Send + Sync>,
    pairing_mode: Arc<dyn Fn() -> PairingMode + Send + Sync>,
) {
    while let Some(ev) = events.recv().await {
        let (helper, unit_active, pairing_mode) =
            (helper.clone(), unit_active.clone(), pairing_mode.clone());
        let step = tokio::task::spawn_blocking(move || {
            // Re-read on every change: the settings panel may have toggled it since.
            let mode = pairing_mode();
            if mode == PairingMode::Off {
                return None;
            }
            let command = Pairing::new(mode).on_event(&ev, unit_active())?;
            Some((command, helper(command)))
        });
        match step.await {
            Ok(Some((command, status))) => log::info!("pairing: {command:?}: {status:?}"),
            Ok(None) => {}
            Err(e) => log::warn!("pairing helper task failed: {e}"),
        }
    }
}

fn start_ui(launcher: &impl Launcher, exits: &mpsc::UnboundedSender<UiExit>) {
    if launcher.ui_running() {
        log::info!("the UI is already running");
        return;
    }
    let exits = exits.clone();
    let on_exit: OnUiExit = Box::new(move |exit| {
        // The watcher outlives its UIs; a send fails only while it shuts down.
        let _ = exits.send(exit);
    });
    match launcher.launch_ui(on_exit) {
        Ok(()) => log::info!("launched the UI"),
        Err(e) => log::warn!("the UI could not be launched: {e}"),
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
    fn launch_ui(&self, on_exit: OnUiExit) -> io::Result<()> {
        let mut child = Command::new(&self.exe)
            .arg("run")
            .stdin(Stdio::null())
            .spawn()?;
        // Reap it when it exits, so no zombie is left behind, and report how it ended.
        std::thread::Builder::new()
            .name("ui-reaper".to_owned())
            .spawn(move || {
                let exit = match child.wait() {
                    Ok(status) if status.success() => UiExit::Clean,
                    Ok(status) => {
                        log::warn!("the UI exited with {status}");
                        UiExit::Crashed
                    }
                    Err(e) => {
                        // Its fate is unknown: do not relaunch on a guess.
                        log::warn!("cannot wait for the UI: {e}");
                        UiExit::Clean
                    }
                };
                on_exit(exit);
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
    use crate::model::{Playback, Track};

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
    fn playback_on_another_device_does_not_start_a_session() {
        // Seen live: with playback on the phone, Soloist relays its playing/paused/track
        // events while reporting is_active false.
        let mut d = CastDetector::default();
        let seen = starts(
            &mut d,
            &[
                PlayerEvent::DeviceActive(false),
                PlayerEvent::PlaybackChanged(Playback::Playing),
                PlayerEvent::TrackChanged(Some(track())),
                PlayerEvent::DeviceActive(false),
                PlayerEvent::PlaybackChanged(Playback::Paused),
                PlayerEvent::PlaybackChanged(Playback::Buffering),
                PlayerEvent::VolumeChanged(40),
            ],
        );
        assert_eq!(seen, [false; 7]);
        assert!(
            d.on_event(&PlayerEvent::DeviceActive(true)),
            "a real cast still counts"
        );
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

    #[test]
    fn crash_guard_allows_three_relaunches_a_minute() {
        let mut guard = CrashGuard::default();
        let t = Instant::now();
        let s = Duration::from_secs;
        assert!(guard.allow(t));
        assert!(guard.allow(t + s(1)));
        assert!(guard.allow(t + s(2)));
        assert!(!guard.allow(t + s(3)), "a crash loop");
        assert!(!guard.allow(t + s(59)));
        assert!(guard.allow(t + CRASH_WINDOW), "the first one has aged out");
    }
}
