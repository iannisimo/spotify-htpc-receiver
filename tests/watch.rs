//! The watcher against the fake Soloist, with a fake launcher.

mod common;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{FakeSoloist, WAIT, Ws, fixture, send_text};
use htpc_nowplaying::config::{Config, DaemonConfig, PairingMode};
use htpc_nowplaying::pairing::{HelperCommand, HelperStatus};
use htpc_nowplaying::watch::{Launcher, MAX_RELAUNCHES, OnUiExit, RELAUNCH_DELAY, UiExit, watch};
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
    /// How to report the running UI's exit.
    on_exit: Option<OnUiExit>,
}

#[derive(Clone, Default)]
struct FakeLauncher(Arc<Mutex<State>>);

impl FakeLauncher {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// The running UI exits.
    fn exit(&self, how: UiExit) {
        let on_exit = {
            let mut s = self.state();
            s.running = false;
            s.on_exit.take().expect("a UI is running")
        };
        on_exit(how);
    }
}

impl Launcher for FakeLauncher {
    fn launch_ui(&self, on_exit: OnUiExit) -> io::Result<()> {
        let mut s = self.state();
        s.launches += 1;
        if s.fail {
            return Err(io::Error::other("no display"));
        }
        s.running = true;
        s.on_exit = Some(on_exit);
        Ok(())
    }

    fn ui_running(&self) -> bool {
        let mut s = self.state();
        s.checks += 1;
        s.running
    }
}

/// The pairing side of the watcher: what the config file says, whether the systemd unit runs
/// Soloist, and a record of the helper's calls.
#[derive(Clone)]
struct FakePairing {
    mode: PairingMode,
    unit_active: bool,
    calls: Arc<Mutex<Vec<HelperCommand>>>,
}

impl Default for FakePairing {
    /// Pairing off, and the unit running Soloist: the watcher has nothing to do.
    fn default() -> Self {
        FakePairing {
            mode: PairingMode::Off,
            unit_active: true,
            calls: Arc::default(),
        }
    }
}

impl FakePairing {
    fn calls(&self) -> Vec<HelperCommand> {
        self.calls.lock().unwrap().clone()
    }
}

fn spawn_watch(
    daemon: DaemonConfig,
    launcher: &FakeLauncher,
    pairing: &FakePairing,
) -> JoinHandle<()> {
    let calls = pairing.calls.clone();
    let (unit_active, mode) = (pairing.unit_active, pairing.mode);
    tokio::spawn(watch(
        cfg(daemon),
        launcher.clone(),
        move |cmd| {
            calls.lock().unwrap().push(cmd);
            HelperStatus::Closed
        },
        move || unit_active,
        move || mode,
    ))
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
    idle_watcher_with(launcher, &FakePairing::default()).await
}

async fn idle_watcher_with(
    launcher: &FakeLauncher,
    pairing: &FakePairing,
) -> (FakeSoloist, Ws, JoinHandle<()>) {
    let (fake, daemon) = FakeSoloist::start().await;
    let task = spawn_watch(daemon, launcher, pairing);
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
    launcher.exit(UiExit::Clean);
    send_text(&mut ws, &fixture("playback_changed_paused.json")).await;
    cast(&mut ws).await;
    send_text(&mut ws, &fixture("track_changed.json")).await;
    tokio::time::sleep(RELAUNCH_DELAY + SETTLE).await;
    assert_eq!(launcher.state().launches, 1);
    task.abort();
}

#[tokio::test]
async fn relaunches_after_a_crash_during_the_cast() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    launcher.exit(UiExit::Crashed);
    tokio::time::sleep(RELAUNCH_DELAY / 2).await;
    assert_eq!(launcher.state().launches, 1, "waits a moment first");
    until("the relaunch", || launcher.state().launches == 2).await;
    assert!(launcher.state().running);
    task.abort();
}

#[tokio::test]
async fn a_crash_after_the_cast_ended_is_not_relaunched() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    end_session(&mut ws).await;
    tokio::time::sleep(SETTLE).await;
    launcher.exit(UiExit::Crashed);
    tokio::time::sleep(RELAUNCH_DELAY + SETTLE).await;
    assert_eq!(launcher.state().launches, 1);
    task.abort();
}

#[tokio::test]
async fn the_cast_ending_cancels_a_pending_relaunch() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    launcher.exit(UiExit::Crashed);
    end_session(&mut ws).await;
    tokio::time::sleep(RELAUNCH_DELAY + SETTLE).await;
    assert_eq!(launcher.state().launches, 1);
    task.abort();
}

#[tokio::test]
async fn stops_relaunching_a_ui_that_keeps_crashing() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    let max = u32::try_from(MAX_RELAUNCHES).unwrap();
    for n in 1..=max {
        launcher.exit(UiExit::Crashed);
        until("a relaunch", || launcher.state().launches == 1 + n).await;
    }
    launcher.exit(UiExit::Crashed);
    tokio::time::sleep(RELAUNCH_DELAY + SETTLE).await;
    assert_eq!(launcher.state().launches, 1 + max, "gave up");

    // The next cast starts afresh.
    end_session(&mut ws).await;
    cast(&mut ws).await;
    until("the next cast's launch", || {
        launcher.state().launches == 2 + max
    })
    .await;
    task.abort();
}

#[tokio::test]
async fn launches_again_after_session_ended_and_new_cast() {
    let launcher = FakeLauncher::default();
    let (_fake, mut ws, task) = idle_watcher(&launcher).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;

    launcher.exit(UiExit::Clean);
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
async fn hand_run_soloist_opens_on_connect_and_closes_on_disconnect() {
    let launcher = FakeLauncher::default();
    let pairing = FakePairing {
        mode: PairingMode::On,
        unit_active: false,
        ..FakePairing::default()
    };
    let (fake, ws, task) = idle_watcher_with(&launcher, &pairing).await;
    until("the first open", || {
        pairing.calls() == [HelperCommand::Open]
    })
    .await;

    // Soloist stops and comes back (its port changes), as a hand-run one does.
    drop(ws);
    until("the close", || pairing.calls().len() == 2).await;
    let mut ws = fake.accept().await;
    send_text(&mut ws, INACTIVE_AUTH).await;
    until("the second open", || pairing.calls().len() == 3).await;
    assert_eq!(
        pairing.calls(),
        [
            HelperCommand::Open,
            HelperCommand::Close,
            HelperCommand::Open
        ]
    );
    task.abort();
}

#[tokio::test]
async fn unit_managed_soloist_is_left_to_the_unit() {
    let launcher = FakeLauncher::default();
    let pairing = FakePairing {
        mode: PairingMode::On,
        unit_active: true,
        ..FakePairing::default()
    };
    let (_fake, ws, task) = idle_watcher_with(&launcher, &pairing).await;
    tokio::time::sleep(SETTLE).await;
    drop(ws);
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        pairing.calls(),
        [],
        "the unit opens and closes the port itself"
    );
    task.abort();
}

#[tokio::test]
async fn pairing_off_does_not_call_helper() {
    let launcher = FakeLauncher::default();
    let pairing = FakePairing {
        mode: PairingMode::Off,
        unit_active: false,
        ..FakePairing::default()
    };
    let (_fake, ws, task) = idle_watcher_with(&launcher, &pairing).await;
    tokio::time::sleep(SETTLE).await;
    drop(ws);
    tokio::time::sleep(SETTLE).await;
    assert_eq!(pairing.calls(), []);
    task.abort();
}

#[tokio::test]
async fn daemon_down_at_start_watcher_waits() {
    let launcher = FakeLauncher::default();
    let address = FakeSoloist::free_address();
    let task = spawn_watch(
        DaemonConfig {
            address: address.clone(),
            ..Config::default().daemon
        },
        &launcher,
        &FakePairing::default(),
    );
    tokio::time::sleep(SETTLE).await;
    assert!(!task.is_finished(), "keeps waiting for Soloist");

    let fake = FakeSoloist::start_at(&address).await;
    let mut ws = fake.accept().await;
    send_text(&mut ws, INACTIVE_AUTH).await;
    cast(&mut ws).await;
    until("a launch", || launcher.state().launches == 1).await;
    task.abort();
}
