//! What is playing, as last reported by the daemon, and a reducer that applies its events.

use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub title: String,
    /// Empty for podcast episodes, which have no creators.
    pub artist: String,
    /// Album name, or the show name for podcast episodes.
    pub album: Option<String>,
    pub cover_url: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Playback {
    #[default]
    Idle,
    Playing,
    Paused,
    Buffering,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    Connected,
    Disconnected,
    /// The name the daemon shows in Spotify's device list.
    DeviceName(String),
    /// Whether a Spotify account is paired with the daemon.
    LoggedIn(bool),
    DeviceActive(bool),
    TrackChanged(Option<Track>),
    PlaybackChanged(Playback),
    /// The daemon's position anchor: `position_ms` was valid at unix time `timestamp_ms`,
    /// advancing at `speed` (0 = not advancing).
    Position {
        position_ms: u64,
        timestamp_ms: u64,
        speed: f32,
    },
    VolumeChanged(u8),
}

/// What the user asked for; the daemon client turns it into wire commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Previous,
    Next,
    PlayPause,
    VolumeDelta(i8),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NowPlaying {
    pub track: Option<Track>,
    pub playback: Playback,
    pub volume: u8,
    pub connected: bool,
    /// Shown on the idle screen ("pick <device name> in Spotify"); `None` until the daemon says.
    pub device_name: Option<String>,
    /// Whether an account is paired with the daemon; `None` until it says. A dropped
    /// connection leaves it as it was.
    pub logged_in: Option<bool>,
    /// Whether this device is the one playing. While it is not, Soloist still relays what the
    /// active device (the phone) plays; none of that belongs on this screen.
    active: bool,
    anchor_ms: u64,
    anchor_at: Option<Instant>,
}

impl NowPlaying {
    /// Applies one daemon event. `now` and `now_unix_ms` are the same moment on the monotonic
    /// and wall clocks; the wall clock is only used to age the daemon's position anchor.
    ///
    /// Invariant: `track` is `None` exactly when `playback` is `Idle`.
    pub fn apply(&mut self, ev: PlayerEvent, now: Instant, now_unix_ms: u64) {
        match ev {
            PlayerEvent::Connected => {
                // The clock was stopped while disconnected; restart it from here, not from
                // the moment the connection dropped.
                self.freeze(now);
                self.connected = true;
            }
            PlayerEvent::Disconnected => {
                self.freeze(now);
                self.connected = false;
            }
            PlayerEvent::DeviceName(name) => self.device_name = Some(name),
            PlayerEvent::LoggedIn(logged_in) => self.logged_in = Some(logged_in),
            PlayerEvent::DeviceActive(active) => {
                self.active = active;
                if !active {
                    self.clear_track();
                }
            }
            PlayerEvent::VolumeChanged(volume) => self.volume = volume.min(100),
            // Playback of another device, relayed while this one is inactive (seen live:
            // track_changed and playback_changed carry no is_active flag).
            PlayerEvent::TrackChanged(_)
            | PlayerEvent::PlaybackChanged(_)
            | PlayerEvent::Position { .. }
                if !self.active => {}
            PlayerEvent::TrackChanged(None) => self.clear_track(),
            PlayerEvent::TrackChanged(Some(track)) => {
                // Soloist repeats the current item in every playback_state; only a new track
                // restarts the clock.
                if self.track.as_ref() != Some(&track) {
                    self.set_anchor(0, now);
                }
                if self.playback == Playback::Idle {
                    // Not known to be playing until the daemon says so.
                    self.playback = Playback::Paused;
                }
                self.track = Some(track);
            }
            PlayerEvent::PlaybackChanged(Playback::Idle) => self.clear_track(),
            PlayerEvent::PlaybackChanged(playback) => {
                if self.track.is_some() {
                    self.freeze(now);
                    self.playback = playback;
                }
            }
            PlayerEvent::Position {
                position_ms,
                timestamp_ms,
                speed,
            } => {
                let age_ms = now_unix_ms.saturating_sub(timestamp_ms);
                let speed = if speed.is_finite() && speed > 0.0 {
                    f64::from(speed)
                } else {
                    0.0
                };
                let aged = position_ms.saturating_add((age_ms as f64 * speed).round() as u64);
                self.set_anchor(aged, now);
            }
        }
    }

    /// Whether this device is the one playing.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Current playback position. Advances in real time only while playing and connected
    /// (the daemon's `speed` is only used to age its anchor: a stale paused anchor has speed 0).
    pub fn position_ms(&self, now: Instant) -> u64 {
        let Some(track) = &self.track else { return 0 };
        let mut position = self.anchor_ms;
        if self.playback == Playback::Playing
            && self.connected
            && let Some(at) = self.anchor_at
        {
            let elapsed = now.saturating_duration_since(at).as_millis();
            position = position.saturating_add(u64::try_from(elapsed).unwrap_or(u64::MAX));
        }
        if track.duration_ms > 0 {
            position = position.min(track.duration_ms);
        }
        position
    }

    fn set_anchor(&mut self, position_ms: u64, now: Instant) {
        self.anchor_ms = position_ms;
        self.anchor_at = Some(now);
    }

    /// Re-anchors at the current position, so a state change does not move the clock.
    fn freeze(&mut self, now: Instant) {
        let position = self.position_ms(now);
        self.set_anchor(position, now);
    }

    fn clear_track(&mut self) {
        self.track = None;
        self.playback = Playback::Idle;
        self.anchor_ms = 0;
        self.anchor_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const UNIX_NOW: u64 = 1_793_900_000_000;

    fn track(title: &str, duration_ms: u64) -> Track {
        Track {
            title: title.to_owned(),
            artist: "Artist".to_owned(),
            album: Some("Album".to_owned()),
            cover_url: None,
            duration_ms,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A connected model playing `track("A", 300_000)` from `start_ms`, anchored at `t0`.
    fn playing_at(t0: Instant, start_ms: u64) -> NowPlaying {
        let mut np = NowPlaying::default();
        for ev in [
            PlayerEvent::Connected,
            PlayerEvent::DeviceActive(true),
            PlayerEvent::TrackChanged(Some(track("A", 300_000))),
            PlayerEvent::PlaybackChanged(Playback::Playing),
            PlayerEvent::Position {
                position_ms: start_ms,
                timestamp_ms: UNIX_NOW,
                speed: 1.0,
            },
        ] {
            np.apply(ev, t0, UNIX_NOW);
        }
        np
    }

    #[test]
    fn starts_idle_and_disconnected() {
        let np = NowPlaying::default();
        assert_eq!(np.playback, Playback::Idle);
        assert_eq!(np.track, None);
        assert!(!np.connected);
        assert_eq!(np.position_ms(Instant::now()), 0);
    }

    #[test]
    fn position_extrapolates_while_playing() {
        let t0 = Instant::now();
        let np = playing_at(t0, 10_000);
        assert_eq!(np.position_ms(t0), 10_000);
        assert_eq!(np.position_ms(t0 + ms(2_500)), 12_500);
    }

    #[test]
    fn track_changed_resets_position() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        let t1 = t0 + ms(5_000);
        np.apply(
            PlayerEvent::TrackChanged(Some(track("B", 200_000))),
            t1,
            UNIX_NOW + 5_000,
        );
        assert_eq!(np.track.as_ref().unwrap().title, "B");
        assert_eq!(np.position_ms(t1), 0);
        assert_eq!(np.playback, Playback::Playing, "playback state is kept");
        assert_eq!(np.position_ms(t1 + ms(1_000)), 1_000);
    }

    #[test]
    fn same_track_does_not_reset_position() {
        // Soloist repeats the current item in every playback_state.
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        let t1 = t0 + ms(3_000);
        np.apply(
            PlayerEvent::TrackChanged(Some(track("A", 300_000))),
            t1,
            UNIX_NOW + 3_000,
        );
        assert_eq!(np.position_ms(t1), 13_000);
    }

    #[test]
    fn position_frozen_while_paused() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        let t1 = t0 + ms(4_000);
        np.apply(
            PlayerEvent::PlaybackChanged(Playback::Paused),
            t1,
            UNIX_NOW + 4_000,
        );
        assert_eq!(
            np.position_ms(t1),
            14_000,
            "frozen where it was when paused"
        );
        assert_eq!(np.position_ms(t1 + ms(60_000)), 14_000);
    }

    #[test]
    fn resume_continues_from_paused_position() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        np.apply(
            PlayerEvent::PlaybackChanged(Playback::Paused),
            t0 + ms(1_000),
            UNIX_NOW,
        );
        // A paused anchor has speed 0; resuming must still advance at normal speed.
        np.apply(
            PlayerEvent::Position {
                position_ms: 11_000,
                timestamp_ms: UNIX_NOW + 1_000,
                speed: 0.0,
            },
            t0 + ms(1_000),
            UNIX_NOW + 1_000,
        );
        let t2 = t0 + ms(30_000);
        np.apply(
            PlayerEvent::PlaybackChanged(Playback::Playing),
            t2,
            UNIX_NOW + 30_000,
        );
        assert_eq!(np.position_ms(t2), 11_000);
        assert_eq!(np.position_ms(t2 + ms(2_000)), 13_000);
    }

    #[test]
    fn buffering_does_not_advance_position() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        let t1 = t0 + ms(1_000);
        np.apply(
            PlayerEvent::PlaybackChanged(Playback::Buffering),
            t1,
            UNIX_NOW + 1_000,
        );
        assert_eq!(np.position_ms(t1 + ms(5_000)), 11_000);
    }

    #[test]
    fn position_clamped_to_duration() {
        let t0 = Instant::now();
        let np = playing_at(t0, 299_000);
        assert_eq!(np.position_ms(t0 + ms(10_000)), 300_000);
    }

    #[test]
    fn anchor_age_is_compensated() {
        // The anchor was taken 800 ms before we received it.
        let t0 = Instant::now();
        let mut np = playing_at(t0, 0);
        np.apply(
            PlayerEvent::Position {
                position_ms: 20_000,
                timestamp_ms: UNIX_NOW - 800,
                speed: 1.0,
            },
            t0,
            UNIX_NOW,
        );
        assert_eq!(np.position_ms(t0), 20_800);
    }

    #[test]
    fn anchor_from_the_future_is_not_aged() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 0);
        np.apply(
            PlayerEvent::Position {
                position_ms: 20_000,
                timestamp_ms: UNIX_NOW + 5_000,
                speed: 1.0,
            },
            t0,
            UNIX_NOW,
        );
        assert_eq!(np.position_ms(t0), 20_000);
    }

    #[test]
    fn invalid_speed_is_treated_as_stopped_for_aging() {
        let t0 = Instant::now();
        for speed in [f32::NAN, -1.0, f32::INFINITY] {
            let mut np = playing_at(t0, 0);
            np.apply(
                PlayerEvent::Position {
                    position_ms: 20_000,
                    timestamp_ms: UNIX_NOW - 800,
                    speed,
                },
                t0,
                UNIX_NOW,
            );
            assert_eq!(np.position_ms(t0), 20_000, "speed {speed}");
        }
    }

    #[test]
    fn query_before_anchor_does_not_underflow() {
        let t0 = Instant::now();
        let np = playing_at(t0 + ms(1_000), 5_000);
        assert_eq!(np.position_ms(t0), 5_000);
    }

    #[test]
    fn logged_in_is_tracked() {
        let t0 = Instant::now();
        let mut np = NowPlaying::default();
        assert_eq!(np.logged_in, None);
        np.apply(PlayerEvent::LoggedIn(false), t0, UNIX_NOW);
        assert_eq!(np.logged_in, Some(false));
        np.apply(PlayerEvent::LoggedIn(true), t0, UNIX_NOW);
        assert_eq!(np.logged_in, Some(true));
        np.apply(PlayerEvent::Disconnected, t0, UNIX_NOW);
        assert_eq!(np.logged_in, Some(true));
    }

    #[test]
    fn device_inactive_goes_idle_and_clears_track() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        np.apply(
            PlayerEvent::DeviceActive(false),
            t0 + ms(1_000),
            UNIX_NOW + 1_000,
        );
        assert_eq!(np.playback, Playback::Idle);
        assert_eq!(np.track, None);
        assert_eq!(np.position_ms(t0 + ms(5_000)), 0);
        assert!(np.connected, "still connected to the daemon");
    }

    #[test]
    fn no_track_means_idle() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        np.apply(PlayerEvent::TrackChanged(None), t0, UNIX_NOW);
        assert_eq!(np.playback, Playback::Idle);
        // A status without a track does not leave idle.
        np.apply(PlayerEvent::PlaybackChanged(Playback::Paused), t0, UNIX_NOW);
        assert_eq!(np.playback, Playback::Idle);
    }

    #[test]
    fn idle_status_clears_track() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        np.apply(PlayerEvent::PlaybackChanged(Playback::Idle), t0, UNIX_NOW);
        assert_eq!(np.playback, Playback::Idle);
        assert_eq!(np.track, None);
    }

    #[test]
    fn unknown_duration_does_not_clamp_position() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 0);
        np.apply(
            PlayerEvent::TrackChanged(Some(track("Live", 0))),
            t0,
            UNIX_NOW,
        );
        assert_eq!(np.position_ms(t0 + ms(3_000)), 3_000);
    }

    #[test]
    fn track_arriving_while_idle_starts_paused() {
        let t0 = Instant::now();
        let mut np = NowPlaying::default();
        np.apply(PlayerEvent::DeviceActive(true), t0, UNIX_NOW);
        np.apply(
            PlayerEvent::TrackChanged(Some(track("A", 1_000))),
            t0,
            UNIX_NOW,
        );
        assert_eq!(np.playback, Playback::Paused);
        assert_eq!(np.position_ms(t0 + ms(500)), 0);
    }

    #[test]
    fn playback_of_another_device_is_ignored_while_inactive() {
        // Seen live: with playback on the phone, Soloist relays track_changed and
        // playback_changed (no is_active flag), then playback_state with is_active false.
        let t0 = Instant::now();
        let mut np = NowPlaying::default();
        for ev in [
            PlayerEvent::Connected,
            PlayerEvent::DeviceActive(false),
            PlayerEvent::TrackChanged(Some(track("Phone song", 200_000))),
            PlayerEvent::PlaybackChanged(Playback::Playing),
            PlayerEvent::Position {
                position_ms: 5_000,
                timestamp_ms: UNIX_NOW,
                speed: 1.0,
            },
        ] {
            np.apply(ev, t0, UNIX_NOW);
            assert_eq!(np.track, None, "after {:?}", np.playback);
            assert_eq!(np.playback, Playback::Idle);
        }
        assert!(!np.is_active());
        np.apply(PlayerEvent::VolumeChanged(70), t0, UNIX_NOW);
        assert_eq!(np.volume, 70, "this device's own volume still applies");
    }

    #[test]
    fn becoming_inactive_mid_track_stops_following_playback() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 0);
        assert!(np.is_active());
        np.apply(PlayerEvent::DeviceActive(false), t0, UNIX_NOW);
        np.apply(
            PlayerEvent::TrackChanged(Some(track("B", 1_000))),
            t0,
            UNIX_NOW,
        );
        assert_eq!(np.track, None);
        // Back to this device: its next playback_state brings the track.
        np.apply(PlayerEvent::DeviceActive(true), t0, UNIX_NOW);
        np.apply(
            PlayerEvent::TrackChanged(Some(track("B", 1_000))),
            t0,
            UNIX_NOW,
        );
        assert_eq!(np.track.unwrap().title, "B");
    }

    #[test]
    fn disconnected_keeps_last_track_but_flags_not_connected() {
        let t0 = Instant::now();
        let mut np = playing_at(t0, 10_000);
        let t1 = t0 + ms(2_000);
        np.apply(PlayerEvent::Disconnected, t1, UNIX_NOW + 2_000);
        assert!(!np.connected);
        assert_eq!(np.track.as_ref().unwrap().title, "A");
        assert_eq!(
            np.position_ms(t1 + ms(10_000)),
            12_000,
            "progress stops while the daemon is unreachable"
        );
        let t2 = t1 + ms(10_000);
        np.apply(PlayerEvent::Connected, t2, UNIX_NOW + 12_000);
        assert!(np.connected);
        assert_eq!(
            np.position_ms(t2),
            12_000,
            "no jump for the time spent disconnected"
        );
        assert_eq!(np.position_ms(t2 + ms(1_000)), 13_000);
    }

    #[test]
    fn device_name_is_kept() {
        let t0 = Instant::now();
        let mut np = NowPlaying::default();
        assert_eq!(np.device_name, None);
        np.apply(PlayerEvent::DeviceName("HTPC".to_owned()), t0, UNIX_NOW);
        assert_eq!(np.device_name.as_deref(), Some("HTPC"));
        np.apply(PlayerEvent::DeviceActive(false), t0, UNIX_NOW);
        assert_eq!(
            np.device_name.as_deref(),
            Some("HTPC"),
            "survives going idle"
        );
    }

    #[test]
    fn volume_clamped_0_100() {
        let t0 = Instant::now();
        let mut np = NowPlaying::default();
        np.apply(PlayerEvent::VolumeChanged(42), t0, UNIX_NOW);
        assert_eq!(np.volume, 42);
        np.apply(PlayerEvent::VolumeChanged(250), t0, UNIX_NOW);
        assert_eq!(np.volume, 100);
    }
}
