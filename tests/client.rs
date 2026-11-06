//! The Soloist client against an in-process fake Soloist that replays recorded messages.

mod common;

use std::time::Duration;

use common::{FakeSoloist, WAIT, Ws, fixture, send_text};
use futures_util::{SinkExt, StreamExt};
use htpc_nowplaying::config::{Config, DaemonConfig};
use htpc_nowplaying::model::{Command, PlayerEvent};
use htpc_nowplaying::player::{self, PlayerClient, parse_message};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};

fn fixture_events(name: &str) -> Vec<PlayerEvent> {
    parse_message(&fixture(name)).unwrap()
}

/// The next command frame the client sent, as JSON.
async fn next_command(ws: &mut Ws) -> Value {
    loop {
        let msg = timeout(WAIT, ws.next())
            .await
            .expect("client sends a command")
            .expect("connection open")
            .unwrap();
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn next_event(events: &mut mpsc::Receiver<PlayerEvent>) -> PlayerEvent {
    timeout(WAIT, events.recv())
        .await
        .expect("an event arrives")
        .expect("event channel open")
}

async fn expect_events(events: &mut mpsc::Receiver<PlayerEvent>, expected: &[PlayerEvent]) {
    for want in expected {
        assert_eq!(&next_event(events).await, want);
    }
}

async fn assert_no_event(events: &mut mpsc::Receiver<PlayerEvent>, within: Duration) {
    if let Ok(ev) = timeout(within, events.recv()).await {
        panic!("unexpected event {ev:?}");
    }
}

/// Starts a fake Soloist and a client, completes the connection and replays `initial`.
async fn connected(
    initial: &[&str],
) -> (FakeSoloist, Ws, PlayerClient, mpsc::Receiver<PlayerEvent>) {
    let (fake, cfg) = FakeSoloist::start().await;
    let (tx, mut events) = mpsc::channel(64);
    let client = player::spawn(cfg, tx);
    let mut ws = fake.accept().await;
    expect_events(&mut events, &[PlayerEvent::Connected]).await;
    for name in initial {
        send_text(&mut ws, &fixture(name)).await;
        expect_events(&mut events, &fixture_events(name)).await;
    }
    (fake, ws, client, events)
}

#[tokio::test]
async fn emits_connected_then_initial_state() {
    // `connected` asserts Connected followed by exactly the parsed initial messages.
    connected(&["auth_state.json", "playback_state_playing.json"]).await;
}

#[tokio::test]
async fn forwards_events() {
    let (_fake, mut ws, _client, mut events) = connected(&["auth_state.json"]).await;
    for name in [
        "volume_changed.json",
        "track_changed.json",
        "playback_changed_paused.json",
        "position_sync.json",
        "device_changed.json",
    ] {
        send_text(&mut ws, &fixture(name)).await;
        expect_events(&mut events, &fixture_events(name)).await;
    }
}

#[tokio::test]
async fn ignored_messages_produce_no_events() {
    let (_fake, mut ws, _client, mut events) = connected(&[]).await;
    send_text(&mut ws, &fixture("queue_changed.json")).await;
    send_text(&mut ws, &fixture("context_changed.json")).await;
    send_text(&mut ws, &fixture("volume_changed.json")).await;
    expect_events(&mut events, &fixture_events("volume_changed.json")).await;
}

#[tokio::test]
async fn reassembles_fragmented_playback_state() {
    let (_fake, mut ws, _client, mut events) = connected(&[]).await;
    let text = fixture("playback_state_playing.json");
    let bytes = text.as_bytes();
    let (a, rest) = bytes.split_at(700);
    let (b, c) = rest.split_at(900);
    for (chunk, opcode, last) in [
        (a, Data::Text, false),
        (b, Data::Continue, false),
        (c, Data::Continue, true),
    ] {
        let frame = Frame::message(chunk.to_vec(), OpCode::Data(opcode), last);
        ws.send(Message::Frame(frame)).await.unwrap();
    }
    expect_events(&mut events, &fixture_events("playback_state_playing.json")).await;
}

#[tokio::test]
async fn reconnects_after_server_restart() {
    let (fake, mut ws, _client, mut events) = connected(&["auth_state.json"]).await;
    ws.close(None).await.unwrap();
    drop(ws);
    expect_events(&mut events, &[PlayerEvent::Disconnected]).await;

    let mut ws = fake.accept().await;
    expect_events(&mut events, &[PlayerEvent::Connected]).await;
    send_text(&mut ws, &fixture("auth_state.json")).await;
    expect_events(&mut events, &fixture_events("auth_state.json")).await;
}

#[tokio::test]
async fn reconnects_after_connection_drops_without_close() {
    let (fake, ws, _client, mut events) = connected(&[]).await;
    drop(ws);
    expect_events(&mut events, &[PlayerEvent::Disconnected]).await;
    let _ws = fake.accept().await;
    expect_events(&mut events, &[PlayerEvent::Connected]).await;
}

#[tokio::test]
async fn starts_while_daemon_down_then_connects_when_it_appears() {
    // A free port left closed, so the first attempts fail.
    let address = FakeSoloist::free_address();
    let (tx, mut events) = mpsc::channel(64);
    let _client = player::spawn(
        DaemonConfig {
            address: address.clone(),
            ..Config::default().daemon
        },
        tx,
    );
    assert_no_event(&mut events, Duration::from_millis(300)).await;

    let fake = FakeSoloist::start_at(&address).await;
    let _ws = fake.accept().await;
    // Failed attempts are silent: the first event is Connected, not Disconnected.
    expect_events(&mut events, &[PlayerEvent::Connected]).await;
}

#[tokio::test]
async fn play_pause_sends_pause_when_playing() {
    let (_fake, mut ws, client, _events) = connected(&["playback_state_playing.json"]).await;
    client.send(Command::PlayPause);
    assert_eq!(
        next_command(&mut ws).await,
        json!({"type": "command", "command": "pause"})
    );
}

#[tokio::test]
async fn play_pause_sends_pause_when_buffering() {
    let (_fake, mut ws, client, _events) = connected(&["playback_state_buffering.json"]).await;
    client.send(Command::PlayPause);
    assert_eq!(next_command(&mut ws).await["command"], "pause");
}

#[tokio::test]
async fn play_pause_sends_play_when_paused() {
    let (_fake, mut ws, client, _events) = connected(&["playback_state_paused.json"]).await;
    client.send(Command::PlayPause);
    assert_eq!(
        next_command(&mut ws).await,
        json!({"type": "command", "command": "play"})
    );
}

#[tokio::test]
async fn play_pause_follows_status_changes() {
    let (_fake, mut ws, client, mut events) = connected(&["playback_state_playing.json"]).await;
    send_text(&mut ws, &fixture("playback_changed_paused.json")).await;
    expect_events(&mut events, &fixture_events("playback_changed_paused.json")).await;
    client.send(Command::PlayPause);
    assert_eq!(next_command(&mut ws).await["command"], "play");
}

#[tokio::test]
async fn previous_and_next_send_skips() {
    let (_fake, mut ws, client, _events) = connected(&["playback_state_playing.json"]).await;
    client.send(Command::Previous);
    assert_eq!(
        next_command(&mut ws).await,
        json!({"type": "command", "command": "skip_prev"})
    );
    client.send(Command::Next);
    assert_eq!(
        next_command(&mut ws).await,
        json!({"type": "command", "command": "skip_next"})
    );
}

#[tokio::test]
async fn volume_delta_uses_last_volume_and_clamps_0_100() {
    // playback_state_playing has volume 100.
    let (_fake, mut ws, client, mut events) = connected(&["playback_state_playing.json"]).await;
    let set_volume = |v: u8| json!({"type": "command", "command": "set_volume", "volume": v});

    client.send(Command::VolumeDelta(5));
    assert_eq!(next_command(&mut ws).await, set_volume(100));
    client.send(Command::VolumeDelta(-30));
    assert_eq!(next_command(&mut ws).await, set_volume(70));
    // Applied optimistically: a second press does not wait for Soloist's echo.
    client.send(Command::VolumeDelta(-30));
    assert_eq!(next_command(&mut ws).await, set_volume(40));

    send_text(&mut ws, r#"{"type":"volume_changed","volume":3}"#).await;
    expect_events(&mut events, &[PlayerEvent::VolumeChanged(3)]).await;
    client.send(Command::VolumeDelta(-10));
    assert_eq!(next_command(&mut ws).await, set_volume(0));
}

#[tokio::test]
async fn server_error_message_does_not_close_connection() {
    let (_fake, mut ws, _client, mut events) = connected(&[]).await;
    send_text(&mut ws, r#"{"type":"error","message":"unknown command"}"#).await;
    send_text(&mut ws, &fixture("volume_changed.json")).await;
    expect_events(&mut events, &fixture_events("volume_changed.json")).await;
}

#[tokio::test]
async fn malformed_message_is_skipped() {
    let (_fake, mut ws, _client, mut events) = connected(&[]).await;
    send_text(&mut ws, "{not json").await;
    ws.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    send_text(&mut ws, &fixture("volume_changed.json")).await;
    expect_events(&mut events, &fixture_events("volume_changed.json")).await;
}

#[tokio::test]
async fn command_failure_does_not_panic_or_stop_events() {
    // No volume known yet: the volume command cannot be computed and is dropped.
    let (_fake, mut ws, client, mut events) = connected(&[]).await;
    client.send(Command::VolumeDelta(5));
    send_text(&mut ws, &fixture("playback_state_playing.json")).await;
    expect_events(&mut events, &fixture_events("playback_state_playing.json")).await;
    // Soloist rejects the next command; events and commands keep working.
    client.send(Command::PlayPause);
    assert_eq!(next_command(&mut ws).await["command"], "pause");
    send_text(&mut ws, r#"{"type":"error","message":"pause failed"}"#).await;
    send_text(&mut ws, &fixture("volume_changed.json")).await;
    expect_events(&mut events, &fixture_events("volume_changed.json")).await;
    client.send(Command::Next);
    assert_eq!(next_command(&mut ws).await["command"], "skip_next");
}

#[tokio::test]
async fn commands_while_disconnected_are_dropped() {
    let (fake, ws, client, mut events) = connected(&["playback_state_playing.json"]).await;
    drop(ws);
    expect_events(&mut events, &[PlayerEvent::Disconnected]).await;
    client.send(Command::Next);

    let mut ws = fake.accept().await;
    expect_events(&mut events, &[PlayerEvent::Connected]).await;
    send_text(&mut ws, &fixture("playback_state_paused.json")).await;
    expect_events(&mut events, &fixture_events("playback_state_paused.json")).await;
    client.send(Command::PlayPause);
    assert_eq!(
        next_command(&mut ws).await["command"],
        "play",
        "the stale skip was not sent after reconnecting"
    );
}

#[tokio::test]
async fn dropping_the_client_closes_the_connection() {
    let (_fake, mut ws, client, _events) = connected(&[]).await;
    drop(client);
    let end = timeout(WAIT, async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(
        end.is_ok(),
        "connection closed after the client was dropped"
    );
}

/// Needs a running Soloist (`soloist --ws 127.0.0.1:9090 …`). Run manually:
/// `cargo test --test client -- --ignored --nocapture real_soloist`
#[tokio::test]
#[ignore = "needs a running Soloist"]
async fn real_soloist_sends_initial_state() {
    let (tx, mut events) = mpsc::channel(64);
    let _client = player::spawn(Config::default().daemon, tx);
    assert_eq!(next_event(&mut events).await, PlayerEvent::Connected);
    let mut saw_device_name = false;
    while let Ok(Some(ev)) = timeout(Duration::from_secs(2), events.recv()).await {
        println!("{ev:?}");
        saw_device_name |= matches!(ev, PlayerEvent::DeviceName(_));
    }
    assert!(saw_device_name, "auth_state arrives right after connecting");
}
