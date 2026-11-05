//! Soloist message parsing, checked against messages recorded from a real Soloist.

use std::path::Path;

use htpc_nowplaying::model::{Playback, PlayerEvent, Track};
use htpc_nowplaying::player::{WireCommand, encode_command, parse_message};
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/soloist")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn parse(name: &str) -> Vec<PlayerEvent> {
    parse_message(&fixture(name)).unwrap()
}

fn parse_value(value: &Value) -> Vec<PlayerEvent> {
    parse_message(&value.to_string()).unwrap()
}

/// The track in `track_changed` events.
fn changed_track(events: &[PlayerEvent]) -> Track {
    match events {
        [PlayerEvent::TrackChanged(Some(track))] => track.clone(),
        other => panic!("expected one TrackChanged(Some), got {other:?}"),
    }
}

fn wonderful_nothing() -> Track {
    Track {
        title: "Wonderful Nothing".to_owned(),
        artist: "Glass Animals".to_owned(),
        album: Some("I Love You So F***ing Much".to_owned()),
        cover_url: Some(
            "https://i.scdn.co/image/ab67616d0000b2736dfbd2654e5c961d1126ad9c".to_owned(),
        ),
        duration_ms: 264214,
    }
}

fn tangerine() -> Track {
    Track {
        title: "Tangerine".to_owned(),
        artist: "Glass Animals".to_owned(),
        album: Some("Dreamland".to_owned()),
        cover_url: Some(
            "https://i.scdn.co/image/ab67616d0000b273712701c5e263efc8726b1464".to_owned(),
        ),
        duration_ms: 200583,
    }
}

#[test]
fn auth_state_active() {
    assert_eq!(
        parse("auth_state.json"),
        [
            PlayerEvent::DeviceName("HTPC".to_owned()),
            PlayerEvent::DeviceActive(true),
        ]
    );
}

#[test]
fn auth_state_logged_out_is_inactive() {
    let msg =
        json!({"type": "auth_state", "logged_in": false, "is_active": true, "device_name": "HTPC"});
    assert_eq!(
        parse_value(&msg),
        [
            PlayerEvent::DeviceName("HTPC".to_owned()),
            PlayerEvent::DeviceActive(false),
        ]
    );
}

#[test]
fn playback_state_playing_gives_track_position_volume() {
    assert_eq!(
        parse("playback_state_playing.json"),
        [
            PlayerEvent::DeviceActive(true),
            PlayerEvent::TrackChanged(Some(wonderful_nothing())),
            PlayerEvent::PlaybackChanged(Playback::Playing),
            PlayerEvent::Position {
                position_ms: 34772,
                timestamp_ms: 1793864513250,
                speed: 1.0,
            },
            PlayerEvent::VolumeChanged(100),
        ]
    );
}

#[test]
fn playback_state_paused() {
    assert_eq!(
        parse("playback_state_paused.json"),
        [
            PlayerEvent::DeviceActive(true),
            PlayerEvent::TrackChanged(Some(wonderful_nothing())),
            PlayerEvent::PlaybackChanged(Playback::Paused),
            PlayerEvent::Position {
                position_ms: 108180,
                timestamp_ms: 1793864586483,
                speed: 0.0,
            },
            PlayerEvent::VolumeChanged(100),
        ]
    );
}

#[test]
fn playback_state_buffering() {
    assert_eq!(
        parse("playback_state_buffering.json"),
        [
            PlayerEvent::DeviceActive(true),
            PlayerEvent::TrackChanged(Some(tangerine())),
            PlayerEvent::PlaybackChanged(Playback::Buffering),
            PlayerEvent::Position {
                position_ms: 0,
                timestamp_ms: 1793864591243,
                speed: 0.0,
            },
            PlayerEvent::VolumeChanged(100),
        ]
    );
}

#[test]
fn playback_state_inactive_ignores_its_stale_track() {
    // Recorded from a freshly started Soloist that is not the active device: it still reports
    // the last track it played. Showing it would be wrong, the music is playing elsewhere.
    assert_eq!(
        parse("playback_state_inactive.json"),
        [
            PlayerEvent::DeviceActive(false),
            PlayerEvent::VolumeChanged(40),
        ]
    );
}

#[test]
fn playback_state_without_item_is_inactive_idle() {
    let mut msg = fixture_json("playback_state_paused.json");
    msg.as_object_mut().unwrap().remove("item");
    assert_eq!(
        parse_value(&msg),
        [
            PlayerEvent::DeviceActive(true),
            PlayerEvent::TrackChanged(None),
            PlayerEvent::VolumeChanged(100),
        ]
    );
}

#[test]
fn playback_state_idle_status() {
    let mut msg = fixture_json("playback_state_paused.json");
    msg["status"] = json!("idle");
    assert!(parse_value(&msg).contains(&PlayerEvent::PlaybackChanged(Playback::Idle)));
}

#[test]
fn playback_state_unknown_status_is_skipped() {
    let mut msg = fixture_json("playback_state_paused.json");
    msg["status"] = json!("rewinding");
    let events = parse_value(&msg);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PlayerEvent::PlaybackChanged(_))),
        "{events:?}"
    );
    assert!(events.contains(&PlayerEvent::TrackChanged(Some(wonderful_nothing()))));
}

#[test]
fn playback_changed_statuses() {
    for (file, playback) in [
        ("playback_changed_playing.json", Playback::Playing),
        ("playback_changed_paused.json", Playback::Paused),
        ("playback_changed_buffering.json", Playback::Buffering),
    ] {
        assert_eq!(
            parse(file),
            [PlayerEvent::PlaybackChanged(playback)],
            "{file}"
        );
    }
}

#[test]
fn track_changed_music() {
    assert_eq!(changed_track(&parse("track_changed.json")), tangerine());
}

#[test]
fn track_changed_episode_has_empty_artist_and_show_as_album() {
    let track = changed_track(&parse("track_changed_episode.json"));
    assert_eq!(
        track,
        Track {
            title: "Midterm USA e la sfida di Abdul El-Sayed,  ancora violenza digitale su \
                    Telegram e gli allarmi bomba in Europa"
                .to_owned(),
            artist: String::new(),
            album: Some("The Essential".to_owned()),
            cover_url: Some(
                "https://i.scdn.co/image/ab6765630000ba8acbd71c655da56b08711ed39c".to_owned()
            ),
            duration_ms: 659000,
        }
    );
}

#[test]
fn several_artists_are_joined() {
    let mut msg = fixture_json("track_changed.json");
    let creators = msg["item"]["decorations"]["creators"]
        .as_array_mut()
        .unwrap();
    let mut second = creators[0].clone();
    second["entity"]["decorations"]["identity"]["name"] = json!("Denzel Curry");
    creators.push(second);
    assert_eq!(
        changed_track(&parse_value(&msg)).artist,
        "Glass Animals, Denzel Curry"
    );
}

#[test]
fn device_changed_inactive() {
    assert_eq!(
        parse("device_changed.json"),
        [
            PlayerEvent::DeviceName("HTPC".to_owned()),
            PlayerEvent::DeviceActive(false),
        ]
    );
}

#[test]
fn volume_changed() {
    assert_eq!(
        parse("volume_changed.json"),
        [PlayerEvent::VolumeChanged(94)]
    );
}

#[test]
fn volume_out_of_range_is_clamped_or_skipped() {
    assert_eq!(
        parse_value(&json!({"type": "volume_changed", "volume": 250})),
        [PlayerEvent::VolumeChanged(100)]
    );
    assert_eq!(
        parse_value(&json!({"type": "volume_changed", "volume": -3})),
        []
    );
}

#[test]
fn position_sync() {
    assert_eq!(
        parse("position_sync.json"),
        [PlayerEvent::Position {
            position_ms: 108180,
            timestamp_ms: 1793864586483,
            speed: 0.0,
        }]
    );
}

#[test]
fn queue_changed_is_ignored() {
    assert_eq!(parse("queue_changed.json"), []);
}

#[test]
fn context_changed_is_ignored() {
    assert_eq!(parse("context_changed.json"), []);
}

#[test]
fn unknown_type_is_ignored() {
    assert_eq!(parse_value(&json!({"type": "lyrics_changed", "x": 1})), []);
}

#[test]
fn server_error_and_command_result_are_not_events() {
    assert_eq!(
        parse_value(&json!({"type": "error", "message": "unknown command"})),
        []
    );
    assert_eq!(
        parse_value(&json!({"type": "command_result", "command": "pause"})),
        []
    );
}

#[test]
fn malformed_json_is_error_not_panic() {
    for raw in [
        "",
        "{",
        "not json",
        "[1, 2]",
        "42",
        "{\"no_type\": true}",
        "{\"type\": 7}",
    ] {
        assert!(parse_message(raw).is_err(), "{raw:?} should be an error");
    }
}

#[test]
fn missing_cover_gives_none() {
    let mut msg = fixture_json("track_changed.json");
    msg["item"]["decorations"]
        .as_object_mut()
        .unwrap()
        .remove("visual_identity");
    assert_eq!(changed_track(&parse_value(&msg)).cover_url, None);

    msg["item"]["decorations"]["visual_identity"] = json!({"cover": []});
    assert_eq!(changed_track(&parse_value(&msg)).cover_url, None);
}

#[test]
fn cover_falls_back_to_nearest_size() {
    let mut msg = fixture_json("track_changed.json");
    msg["item"]["decorations"]["visual_identity"]["cover"] = json!([
        {"url": "https://i.scdn.co/image/small", "size": "small"},
        {"url": "https://i.scdn.co/image/default", "size": "default"},
    ]);
    assert_eq!(
        changed_track(&parse_value(&msg)).cover_url.as_deref(),
        Some("https://i.scdn.co/image/default")
    );

    msg["item"]["decorations"]["visual_identity"]["cover"] =
        json!([{"url": "https://i.scdn.co/image/odd", "size": "gigantic"}]);
    assert_eq!(
        changed_track(&parse_value(&msg)).cover_url.as_deref(),
        Some("https://i.scdn.co/image/odd"),
        "an unknown size is still better than no cover"
    );
}

#[test]
fn corrupt_cover_url_is_skipped() {
    // Soloist once sent a context cover URL with raw binary bytes in it.
    let mut msg = fixture_json("track_changed.json");
    msg["item"]["decorations"]["visual_identity"]["cover"] = json!([
        {"url": "https://i.scdn.co/image/\u{0}\u{1f}garbage", "size": "large"},
        {"url": "http://example.com/plain-http.jpg", "size": "xlarge"},
        {"url": "https://i.scdn.co/image/good", "size": "default"},
    ]);
    assert_eq!(
        changed_track(&parse_value(&msg)).cover_url.as_deref(),
        Some("https://i.scdn.co/image/good")
    );
}

#[test]
fn unicode_and_emoji_titles_preserved() {
    let mut msg = fixture_json("track_changed.json");
    let title = "Jóga — Björk 🎵✨ 日本語 مرحبا";
    msg["item"]["decorations"]["identity"]["name"] = json!(title);
    assert_eq!(changed_track(&parse_value(&msg)).title, title);
}

#[test]
fn very_long_title_is_preserved_unmodified() {
    let mut msg = fixture_json("track_changed.json");
    let title = "Very long title ".repeat(1000);
    msg["item"]["decorations"]["identity"]["name"] = json!(title);
    assert_eq!(changed_track(&parse_value(&msg)).title, title);
}

#[test]
fn track_with_missing_fields_still_parses() {
    let msg = json!({"type": "track_changed", "item": {"decorations": {}}});
    assert_eq!(
        changed_track(&parse_value(&msg)),
        Track {
            title: String::new(),
            artist: String::new(),
            album: None,
            cover_url: None,
            duration_ms: 0,
        }
    );
}

#[test]
fn encode_set_volume_matches_wire_format() {
    let encoded: Value =
        serde_json::from_str(&encode_command(&WireCommand::SetVolume(40))).unwrap();
    assert_eq!(
        encoded,
        json!({"type": "command", "command": "set_volume", "volume": 40})
    );
}

#[test]
fn encode_simple_commands() {
    for (cmd, name) in [
        (WireCommand::Play, "play"),
        (WireCommand::Pause, "pause"),
        (WireCommand::SkipNext, "skip_next"),
        (WireCommand::SkipPrev, "skip_prev"),
    ] {
        let encoded: Value = serde_json::from_str(&encode_command(&cmd)).unwrap();
        assert_eq!(encoded, json!({"type": "command", "command": name}));
    }
}
