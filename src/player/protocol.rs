//! Soloist's WebSocket messages (`ws://127.0.0.1:<port>`, JSON text frames).

use std::fmt;

use serde_json::{Value, json};

use crate::model::{Playback, PlayerEvent, Track};

/// Commands understood by Soloist. It has no play/pause toggle and no relative volume:
/// the client picks `Play`/`Pause` and computes the absolute volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireCommand {
    Play,
    Pause,
    SkipNext,
    SkipPrev,
    SetVolume(u8),
}

#[derive(Debug)]
pub enum ParseError {
    Json(serde_json::Error),
    /// Valid JSON, but not an object with a string `type`.
    NotAMessage,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Json(e) => write!(f, "invalid JSON: {e}"),
            ParseError::NotAMessage => f.write_str("not an object with a string `type`"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Turns one Soloist message into model events (often several, sometimes none).
///
/// Only invalid JSON or a message without a `type` is an error. Message types we do not use
/// (`queue_changed`, `context_changed`, `command_result`, future ones) give no events; missing
/// or odd fields inside a known message are skipped rather than failing the whole message.
pub fn parse_message(raw: &str) -> Result<Vec<PlayerEvent>, ParseError> {
    let msg: Value = serde_json::from_str(raw).map_err(ParseError::Json)?;
    let kind = msg
        .get("type")
        .and_then(Value::as_str)
        .ok_or(ParseError::NotAMessage)?;

    let mut events = Vec::new();
    match kind {
        "auth_state" => {
            push_device_name(&msg, &mut events);
            let logged_in = msg["logged_in"].as_bool().unwrap_or(false);
            let active = msg["is_active"].as_bool().unwrap_or(false);
            events.push(PlayerEvent::LoggedIn(logged_in));
            events.push(PlayerEvent::DeviceActive(logged_in && active));
        }
        "device_changed" => {
            push_device_name(&msg, &mut events);
            if let Some(active) = msg["is_active"].as_bool() {
                events.push(PlayerEvent::DeviceActive(active));
            }
        }
        "playback_state" => parse_playback_state(&msg, &mut events),
        "track_changed" => events.push(PlayerEvent::TrackChanged(parse_track(&msg["item"]))),
        "playback_changed" => {
            events.extend(parse_status(&msg["status"]).map(PlayerEvent::PlaybackChanged))
        }
        "position_sync" => events.extend(parse_position(&msg["position"])),
        "volume_changed" => events.extend(parse_volume(&msg["volume"])),
        "error" => log::warn!(
            "soloist reported an error: {}",
            msg["message"].as_str().unwrap_or("(no message)")
        ),
        "command_result" | "queue_changed" | "context_changed" => {}
        other => log::debug!("ignoring soloist message type {other:?}"),
    }
    Ok(events)
}

/// Encodes a command frame: `{"type":"command","command":<name>, ...}`.
pub fn encode_command(cmd: &WireCommand) -> String {
    let frame = match cmd {
        WireCommand::Play => json!({"type": "command", "command": "play"}),
        WireCommand::Pause => json!({"type": "command", "command": "pause"}),
        WireCommand::SkipNext => json!({"type": "command", "command": "skip_next"}),
        WireCommand::SkipPrev => json!({"type": "command", "command": "skip_prev"}),
        WireCommand::SetVolume(volume) => {
            json!({"type": "command", "command": "set_volume", "volume": volume})
        }
    };
    frame.to_string()
}

fn parse_playback_state(msg: &Value, events: &mut Vec<PlayerEvent>) {
    let active = msg["is_active"].as_bool().unwrap_or(false);
    let volume = parse_volume(&msg["volume"]);
    if !active {
        // An inactive Soloist still reports the last track it played (seen after a restart);
        // the music is playing elsewhere, so none of the playback fields apply.
        events.push(PlayerEvent::DeviceActive(false));
        events.extend(volume);
        return;
    }
    events.push(PlayerEvent::DeviceActive(true));
    match parse_track(&msg["item"]) {
        None => events.push(PlayerEvent::TrackChanged(None)),
        Some(track) => {
            events.push(PlayerEvent::TrackChanged(Some(track)));
            events.extend(parse_status(&msg["status"]).map(PlayerEvent::PlaybackChanged));
            events.extend(parse_position(&msg["position"]));
        }
    }
    events.extend(volume);
}

fn push_device_name(msg: &Value, events: &mut Vec<PlayerEvent>) {
    if let Some(name) = msg["device_name"].as_str() {
        events.push(PlayerEvent::DeviceName(name.to_owned()));
    }
}

fn parse_status(status: &Value) -> Option<Playback> {
    match status.as_str()? {
        "playing" => Some(Playback::Playing),
        "paused" => Some(Playback::Paused),
        "buffering" => Some(Playback::Buffering),
        "idle" => Some(Playback::Idle),
        other => {
            log::debug!("ignoring unknown soloist status {other:?}");
            None
        }
    }
}

fn parse_position(position: &Value) -> Option<PlayerEvent> {
    Some(PlayerEvent::Position {
        position_ms: position["position_ms"].as_u64()?,
        timestamp_ms: position["timestamp_ms"].as_u64()?,
        speed: position["speed"].as_f64()? as f32,
    })
}

fn parse_volume(volume: &Value) -> Option<PlayerEvent> {
    let volume = volume.as_u64()?.min(100);
    Some(PlayerEvent::VolumeChanged(volume as u8))
}

/// A track or episode entity. `None` when there is no item; missing fields get empty values.
fn parse_track(item: &Value) -> Option<Track> {
    if !item.is_object() {
        return None;
    }
    let decorations = &item["decorations"];
    let name = |entity: &Value| {
        entity
            .pointer("/decorations/identity/name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    // Episodes have no creators; their parent is the show.
    let artist = decorations["creators"]
        .as_array()
        .map(|creators| {
            creators
                .iter()
                .filter_map(|c| name(&c["entity"]))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    Some(Track {
        title: decorations
            .pointer("/identity/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        artist,
        album: name(&decorations["parent"]["entity"]),
        cover_url: choose_cover(&decorations["visual_identity"]["cover"]),
        duration_ms: decorations
            .pointer("/playback/duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Cover sizes Soloist sends, smallest first (`small` 64 px, `default` 300 px, `large` and
/// `xlarge` 640 px).
const COVER_SIZES: [&str; 4] = ["small", "default", "large", "xlarge"];
const PREFERRED_COVER: usize = 2; // "large"

/// Picks the usable cover URL whose size is closest to `large`, preferring bigger on a tie
/// (downscaling looks better than upscaling). Unknown sizes come last; broken URLs are skipped.
fn choose_cover(covers: &Value) -> Option<String> {
    covers
        .as_array()?
        .iter()
        .filter_map(|cover| {
            let url = cover["url"].as_str().filter(|u| is_usable_url(u))?;
            let rank = cover["size"]
                .as_str()
                .and_then(|size| COVER_SIZES.iter().position(|&s| s == size));
            let key = match rank {
                Some(rank) => (rank.abs_diff(PREFERRED_COVER), usize::MAX - rank),
                None => (usize::MAX, 0),
            };
            Some((key, url))
        })
        .min_by_key(|&(key, _)| key)
        .map(|(_, url)| url.to_owned())
}

fn is_usable_url(url: &str) -> bool {
    url.starts_with("https://") && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}
