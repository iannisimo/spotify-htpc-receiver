//! Beat onsets for delay calibration: flashes the screen on each beat of the music, so the
//! visual delay can be tuned until the flash lines up with what is heard.
//!
//! The samples arrive already held back by the capture's delay, so a flash at the moment they
//! are read lines up with the sound when the delay is right.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::visualizer::AudioSource;

/// How long the screen flashes after a beat.
pub const FLASH: Duration = Duration::from_millis(120);

/// How far back the running average of the energy looks.
const WINDOW: Duration = Duration::from_secs(1);
/// A beat is a chunk this many times louder than the running average.
const RATIO: f32 = 1.5;
/// Mean energy (squared amplitude) below which a chunk is not a beat at all: RMS 0.01.
const BEAT_FLOOR: f32 = 1e-4;
/// Mean energy below which a chunk counts as silence: RMS 0.001.
const SILENCE_FLOOR: f32 = 1e-6;
/// At most one beat per this long (240 bpm).
const MIN_GAP: Duration = Duration::from_millis(250);
/// The average needs this much history before beats are called, or the first chunk of any
/// sound would be one.
const MIN_HISTORY: Duration = Duration::from_millis(200);
/// No audio for this long means the music stopped.
const SILENCE_AFTER: Duration = Duration::from_secs(3);

/// Spots the start of beats in a stream of audio chunks.
#[derive(Debug, Default)]
pub struct OnsetDetector {
    /// Mean energy of each recent chunk.
    history: VecDeque<(Instant, f32)>,
    last_beat: Option<Instant>,
}

impl OnsetDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk of interleaved samples that arrived at `now`. True if it starts a beat.
    pub fn push(&mut self, interleaved: &[f32], now: Instant) -> bool {
        if interleaved.is_empty() {
            return false;
        }
        let energy = interleaved.iter().map(|s| s * s).sum::<f32>() / interleaved.len() as f32;
        while self
            .history
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) > WINDOW)
        {
            self.history.pop_front();
        }
        let known = self
            .history
            .front()
            .is_some_and(|&(at, _)| now.saturating_duration_since(at) >= MIN_HISTORY);
        let average = if self.history.is_empty() {
            0.0
        } else {
            self.history.iter().map(|&(_, e)| e).sum::<f32>() / self.history.len() as f32
        };
        self.history.push_back((now, energy));

        let rested = self
            .last_beat
            .is_none_or(|at| now.saturating_duration_since(at) >= MIN_GAP);
        let beat = known && rested && energy > BEAT_FLOOR && energy > RATIO * average;
        if beat {
            self.last_beat = Some(now);
        }
        beat
    }
}

/// A running calibration: reads the delayed audio and tracks the latest beat.
pub struct Calibration<A: AudioSource> {
    audio: A,
    detector: OnsetDetector,
    buffer: Vec<f32>,
    now: Option<Instant>,
    last_beat: Option<Instant>,
    last_sound: Option<Instant>,
}

impl<A: AudioSource> Calibration<A> {
    pub fn new(audio: A) -> Self {
        Calibration {
            audio,
            detector: OnsetDetector::new(),
            buffer: Vec::new(),
            now: None,
            last_beat: None,
            last_sound: None,
        }
    }

    /// Reads the audio due at `now` and looks for a beat in it.
    pub fn tick(&mut self, now: Instant) {
        self.now = Some(now);
        self.audio.read(&mut self.buffer);
        if self.buffer.is_empty() {
            return;
        }
        let energy = self.buffer.iter().map(|s| s * s).sum::<f32>() / self.buffer.len() as f32;
        if energy > SILENCE_FLOOR {
            self.last_sound = Some(now);
        }
        if self.detector.push(&self.buffer, now) {
            self.last_beat = Some(now);
        }
    }

    /// True within [`FLASH`] after the latest beat.
    pub fn flash(&self, now: Instant) -> bool {
        self.last_beat
            .is_some_and(|at| now >= at && now.saturating_duration_since(at) < FLASH)
    }

    /// True until sound has arrived, and again after 3 s of silence.
    pub fn waiting(&self) -> bool {
        match (self.last_sound, self.now) {
            (Some(sound), Some(now)) => now.saturating_duration_since(sound) >= SILENCE_AFTER,
            _ => true,
        }
    }

    /// Changes the capture's delay; the next beats show it.
    pub fn set_delay(&mut self, delay_ms: u32) {
        self.audio.set_delay(delay_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioStatus;

    const RATE: usize = 48_000;
    /// 10 ms of stereo audio.
    const CHUNK_FRAMES: usize = RATE / 100;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// One 10 ms chunk starting at `at_ms`: a 1 kHz tone while `loud(at_ms)`, else silence.
    fn chunk(at_ms: u64, loud: impl Fn(u64) -> bool) -> Vec<f32> {
        (0..CHUNK_FRAMES)
            .flat_map(|i| {
                let t = (at_ms as f32 / 1000.0) + i as f32 / RATE as f32;
                let v = if loud(at_ms) {
                    0.8 * (t * 1000.0 * std::f32::consts::TAU).sin()
                } else {
                    0.0
                };
                [v, v]
            })
            .collect()
    }

    /// 50 ms bursts every 500 ms, starting at 0.
    fn clicks(at_ms: u64) -> bool {
        at_ms % 500 < 50
    }

    #[test]
    fn detects_clicks_at_120_bpm() {
        let t0 = Instant::now();
        let mut det = OnsetDetector::new();
        let mut beats = Vec::new();
        for at in (0..4000).step_by(10) {
            if det.push(&chunk(at, clicks), t0 + ms(at)) {
                beats.push(at);
            }
        }
        assert!((7..=8).contains(&beats.len()), "{beats:?}");
        for beat in beats {
            let start = beat / 500 * 500;
            assert!(
                beat - start <= 10,
                "beat at {beat} ms is not at a burst start"
            );
        }
    }

    #[test]
    fn ignores_steady_tone() {
        let t0 = Instant::now();
        let mut det = OnsetDetector::new();
        for at in (0..5000).step_by(10) {
            let beat = det.push(&chunk(at, |_| true), t0 + ms(at));
            assert!(!(at >= 1000 && beat), "beat at {at} ms");
        }
    }

    #[test]
    fn quiet_noise_below_the_floor_is_no_beat() {
        let t0 = Instant::now();
        let mut det = OnsetDetector::new();
        let quiet = vec![0.001; CHUNK_FRAMES * 2];
        let faint = vec![0.005; CHUNK_FRAMES * 2];
        for at in (0..2000).step_by(10) {
            let samples = if at % 500 < 50 { &faint } else { &quiet };
            assert!(!det.push(samples, t0 + ms(at)));
        }
    }

    struct FakeAudio {
        queue: VecDeque<Vec<f32>>,
        delays: Vec<u32>,
    }

    impl AudioSource for FakeAudio {
        fn read(&mut self, out: &mut Vec<f32>) {
            out.clear();
            if let Some(chunk) = self.queue.pop_front() {
                *out = chunk;
            }
        }
        fn status(&mut self) -> Option<AudioStatus> {
            None
        }
        fn set_delay(&mut self, delay_ms: u32) {
            self.delays.push(delay_ms);
        }
    }

    fn calibration(chunks: impl IntoIterator<Item = Vec<f32>>) -> Calibration<FakeAudio> {
        Calibration::new(FakeAudio {
            queue: chunks.into_iter().collect(),
            delays: Vec::new(),
        })
    }

    #[test]
    fn silence_waits() {
        let t0 = Instant::now();
        let mut cal = calibration((0..600).map(|at| chunk(at * 10, |_| false)));
        assert!(cal.waiting(), "before any tick");
        for at in (0..2000).step_by(10) {
            cal.tick(t0 + ms(at));
            assert!(cal.waiting(), "silence at {at} ms");
        }

        // Clicks arrive.
        let mut cal = calibration((0..700).map(|i| chunk(i * 10, clicks)));
        for at in (0..1000).step_by(10) {
            cal.tick(t0 + ms(at));
        }
        assert!(!cal.waiting());

        // Then 3 s of silence.
        let mut cal = calibration(
            (0..100)
                .map(|i| chunk(i * 10, clicks))
                .chain((0..400).map(|i| chunk(i * 10, |_| false))),
        );
        for at in (0..1000).step_by(10) {
            cal.tick(t0 + ms(at));
        }
        assert!(!cal.waiting());
        for at in (1000..4500).step_by(10) {
            cal.tick(t0 + ms(at));
        }
        assert!(cal.waiting(), "3 s without sound");
    }

    #[test]
    fn flash_lasts_120ms() {
        let t0 = Instant::now();
        // 300 ms of silence for a baseline, then a loud chunk.
        let mut cal = calibration(
            (0..30)
                .map(|i| chunk(i * 10, |_| false))
                .chain([chunk(0, |_| true)]),
        );
        assert!(!cal.flash(t0));
        for i in 0..30 {
            cal.tick(t0 + ms(i * 10));
        }
        assert!(!cal.flash(t0 + ms(300)));
        let beat = t0 + ms(300);
        cal.tick(beat);
        assert!(cal.flash(beat));
        assert!(cal.flash(beat + ms(119)));
        assert!(!cal.flash(beat + ms(120)));
        assert!(!cal.flash(beat - ms(1)), "never before the beat");
    }

    #[test]
    fn delay_changes_reach_the_audio() {
        let mut cal = calibration([]);
        cal.set_delay(80);
        assert_eq!(cal.audio.delays, [80]);
    }
}
