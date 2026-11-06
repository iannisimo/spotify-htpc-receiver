//! The watcher against the fake Soloist, with a fake launcher.

mod common;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{FakeSoloist, WAIT, Ws, fixture, send_text};
use htpc_nowplaying::config::{Config, DaemonConfig};
use htpc_nowplaying::watch::{Launcher, watch};
use tokio::task::JoinHandle;

const INACTIVE_AUTH: &str =
    r#"{"type":"auth_state","logged_in":true,"is_active":false,"device_name":"HTPC"}"#;
const BECOMES_ACTIVE: &str = r#"{"type":"device_changed","is_active":true}"#;
/// Long enough for an event to have gone through the watcher.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Default)]
struct State {
    running: bool,
    fail: bool,
    launches: u32,
    checks: u32,
}

#[derive(Clone, Default)]
struct FakeLauncher(Arc<Mutex<State>>);

impl FakeLauncher {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
}

impl Launcher for FakeLauncher {
    fn launch_ui(&self) -> io::Result<()> {
        let mut s = self.state();
        s.launches += 1;
        if s.fail {
            return Err(io::Error::other("no display"));
        }
        s.running = true;
        Ok(())
    }

    fn ui_running(&self) -> bool {
        let mut s = self.state();
        s.checks += 1;
        s.running
    }
}

/// Polls until `cond` holds, failing the test after [`WAIT`].
async fn until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn cfg(daemon: DaemonConfig) -> Config {
    Config {
        daemon,
        ..Config::default()
    }
}

/// A watcher connected to a fake Soloist that reports an idle device.
async fn idle_watcher(launcher: &FakeLauncher) -> (FakeSoloist, Ws, JoinHandle<()>) {
    let (fake, daemon) = FakeSoloist::start().await;
    let task = tokio::spawn(watch(cfg(daemon), launcher.clone()));
    let mut ws = fake.accept().await;
    send_text(&mut ws, INACTIVE_AUTH).await;
    (fake, ws, task)
}

async fn cast(ws: &mut Ws) {
    send_text(ws, BECOMES_ACTIVE).await;
    send_text(ws, &fixture("playback_state_playing.json")).await;
    send_text(ws, &fixture("playback_changed_playing.json")).await;
}

async fn end_session(ws: &mut Ws) {
    send_text(ws, &fixture("device_changed.json")).await; // is_active: false
}

#[tokio::test]
async fn launches_ui_once_when_cast_starts() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(launcher.state().launches, 0, "idle: nothing to launch");

    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(launcher.state().launches, 1);
    task.abort();
}

#[tokio::test]
async fn playback_on_the_phone_does_not_launch() {
    // Recorded live with playback on the phone: Soloist relays play, pause and skips while
    // reporting itself inactive.
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    for name in [
        "playback_changed_playing.json",
        "remote_playback_state_playing.json",
        "position_sync.json",
        "playback_changed_paused.json",
        "remote_track_changed.json",
        "playback_changed_playing.json",
        "remote_playback_state_playing.json",
    ] {
        send_text(&mut ws, &fixture(name)).await;
    }
    tokio::time::sleep(SETTLE).await;
    assert_eq!(launcher.state().launches, 0);
    assert_eq!(launcher.state().checks, 0, "not even considered a cast");

    // Picking HTPC afterwards still launches.
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;
    task.abort();
}

#[tokio::test]
async fn does_not_launch_when_ui_already_running() {
    let launcher = FakeLauncher::default();
    launcher.state().running = true;
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("the running check", || launcher.state().checks == 1).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(launcher.state().launches, 0);
    task.abort();
}

#[tokio::test]
async fn does_not_relaunch_within_same_session() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    // The user closes the UI; the cast goes on (pause, resume, next track).
    launcher.state().running = false;
    send_text(&mut ws, &fixture("playback_changed_paused.json")).await;
    cast(&mut ws).await;
    send_text(&mut ws, &fixture("track_changed.json")).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(launcher.state().launches, 1);
    task.abort();
}

#[tokio::test]
async fn launches_again_after_session_ended_and_new_cast() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    launcher.state().running = false;
    end_session(&mut ws).await;
    cast(&mut ws).await;
    until("a second launch", || launcher.state().launches == 2).await;
    task.abort();
}

#[tokio::test]
async fn launch_failure_is_logged_and_watcher_keeps_running() {
    let launcher = FakeLauncher::default();
    launcher.state().fail = true;
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch attempt", || launcher.state().launches == 1).await;
    tokio::time::sleep(SETTLE).await;
    assert!(!task.is_finished(), "watcher survives the failure");
    assert_eq!(
        launcher.state().launches,
        1,
        "no retry loop within the session"
    );

    launcher.state().fail = false;
    end_session(&mut ws).await;
    cast(&mut ws).await;
    until("the next session's launch", || {
        launcher.state().launches == 2
    })
    .await;
    assert!(launcher.state().running);
    task.abort();
}

#[tokio::test]
async fn daemon_down_at_start_watcher_waits() {
    let launcher = FakeLauncher::default();
    let address = FakeSoloist::free_address();
    let task = tokio::spawn(watch(
        cfg(DaemonConfig {
            address: address.clone(),
            ..Config::default().daemon
        }),
        launcher.clone(),
    ));
    tokio::time::sleep(SETTLE).await;
    assert!(!task.is_finished(), "keeps waiting for Soloist");

    let fake = FakeSoloist::start_at(&address).await;
    let mut ws = fake.accept().await;
    send_text(&mut ws, INACTIVE_AUTH).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;
    task.abort();
}
