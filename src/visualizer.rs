//! The visualizer screen: owns the audio capture and the projectM renderer while it is open, and
//! decides what the screen shows. Independent of Slint and GL so it can be tested with fakes;
//! `ui.rs` connects it to the window.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::audio::{self, AudioStatus};
use crate::config::VisualizerConfig;
use crate::viz::adaptive::{Adaptive, Quality, quality_range};
use crate::viz::disabled::DisabledPresets;
use crate::viz::{self, VizError};

/// How long the track overlay stays up after a track change.
pub const TRACK_OVERLAY: Duration = Duration::from_secs(4);
/// How long a status line (lock, sensitivity, preset name) stays up.
pub const STATUS_LINE: Duration = Duration::from_secs(2);
/// "Waiting for audio" is held back this long after opening: the capture normally finds the
/// stream within milliseconds, and the message should not flash on the way.
pub const WAITING_GRACE: Duration = Duration::from_millis(400);

/// Where the samples come from: the PipeWire capture, or a fake in tests.
pub trait AudioSource {
    /// Replaces `out` with the samples due now (interleaved stereo f32).
    fn read(&mut self, out: &mut Vec<f32>);
    /// The latest status change since the last call, if any.
    fn status(&mut self) -> Option<AudioStatus>;
    /// Changes the delay for audio captured from now on.
    fn set_delay(&mut self, delay_ms: u32);
}

/// What draws the frames: projectM, or a fake in tests.
pub trait Renderer {
    fn feed_pcm(&mut self, interleaved: &[f32]);
    fn render(&mut self, width: u32, height: u32, quality: Quality);
    fn next_preset(&mut self);
    fn prev_preset(&mut self);
    fn toggle_lock(&mut self) -> bool;
    fn adjust_beat_sensitivity(&mut self, delta: f32) -> f32;
    fn current_preset_name(&self) -> String;
    /// Sets beat sensitivity (0–5) and returns the value in effect.
    fn set_beat_sensitivity(&mut self, value: f32) -> f32;
    fn set_preset_duration(&mut self, seconds: f64);
    fn set_shuffle(&mut self, on: bool);
    fn set_disabled(&mut self, disabled: &DisabledPresets);
    /// Shows one preset now and stops automatic switching, until `end_demo`.
    fn demo(&mut self, path: &Path) -> Result<(), VizError>;
    fn end_demo(&mut self);
}

/// The live audio source.
pub struct LiveAudio {
    capture: audio::Capture,
    status: std::sync::mpsc::Receiver<AudioStatus>,
}

impl LiveAudio {
    pub fn start(delay_ms: u32) -> Result<LiveAudio, String> {
        let (capture, status) =
            audio::start(audio::StreamMatch::soloist(), delay_ms).map_err(|e| e.to_string())?;
        Ok(LiveAudio { capture, status })
    }
}

impl AudioSource for LiveAudio {
    fn read(&mut self, out: &mut Vec<f32>) {
        self.capture.read(out);
    }

    fn status(&mut self) -> Option<AudioStatus> {
        self.status.try_iter().last()
    }

    fn set_delay(&mut self, delay_ms: u32) {
        self.capture.set_delay(delay_ms);
    }
}

impl Renderer for viz::Visualizer {
    fn feed_pcm(&mut self, interleaved: &[f32]) {
        viz::Visualizer::feed_pcm(self, interleaved);
    }
    fn render(&mut self, width: u32, height: u32, quality: Quality) {
        viz::Visualizer::render(self, width, height, quality);
    }
    fn next_preset(&mut self) {
        viz::Visualizer::next_preset(self);
    }
    fn prev_preset(&mut self) {
        viz::Visualizer::prev_preset(self);
    }
    fn toggle_lock(&mut self) -> bool {
        viz::Visualizer::toggle_lock(self)
    }
    fn adjust_beat_sensitivity(&mut self, delta: f32) -> f32 {
        viz::Visualizer::adjust_beat_sensitivity(self, delta)
    }
    fn current_preset_name(&self) -> String {
        viz::Visualizer::current_preset_name(self)
    }
    fn set_beat_sensitivity(&mut self, value: f32) -> f32 {
        viz::Visualizer::set_beat_sensitivity(self, value)
    }
    fn set_preset_duration(&mut self, seconds: f64) {
        viz::Visualizer::set_preset_duration(self, seconds);
    }
    fn set_shuffle(&mut self, on: bool) {
        viz::Visualizer::set_shuffle(self, on);
    }
    fn set_disabled(&mut self, disabled: &DisabledPresets) {
        viz::Visualizer::set_disabled(self, disabled);
    }
    fn demo(&mut self, path: &Path) -> Result<(), VizError> {
        viz::Visualizer::demo(self, path)
    }
    fn end_demo(&mut self) {
        viz::Visualizer::end_demo(self);
    }
}

/// What the visualizer screen shows right now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct View {
    /// projectM is drawing: nothing opaque may cover the window.
    pub rendering: bool,
    /// Centred message over a black screen (empty when there is nothing to explain).
    pub message: String,
    /// Smaller line under the message.
    pub detail: String,
    /// Track title and artist, shown for a while after a track change.
    pub overlay: Option<(String, String)>,
    /// Short-lived status line: lock state, sensitivity, preset name.
    pub status: Option<String>,
}

pub struct Session<A, R> {
    base: Quality,
    floor: Quality,
    /// Why the window cannot host the visualizer at all.
    gl: Result<(), String>,
    open: bool,
    opened_at: Option<Instant>,
    audio: Option<A>,
    audio_status: AudioStatus,
    audio_error: Option<String>,
    renderer: Option<R>,
    renderer_error: Option<VizError>,
    /// Renderers of a closed screen, waiting for the GL thread to drop them.
    released: Vec<R>,
    adaptive: Adaptive,
    last_frame: Option<Instant>,
    overlay: Option<(String, String, Instant)>,
    status: Option<(String, Instant)>,
    pcm: Vec<f32>,
    stats: FrameStats,
    /// Delay for the next capture, and for the running one when it changes.
    delay_ms: u32,
    disabled: DisabledPresets,
    /// Settings changed from the settings panel; a renderer made later gets them too.
    changed: Changed,
    /// A preset demo is showing on the current renderer.
    demo_active: bool,
}

/// Which settings were changed since the session was made.
#[derive(Default)]
struct Changed {
    beat_sensitivity: Option<f32>,
    preset_duration_s: Option<f64>,
    shuffle: Option<bool>,
    disabled: bool,
}

/// Frame rate and render cost, logged at debug level every few seconds for tuning.
#[derive(Default)]
struct FrameStats {
    since: Option<Instant>,
    frames: u32,
    render: Duration,
}

impl FrameStats {
    const EVERY: Duration = Duration::from_secs(5);

    fn record(&mut self, now: Instant, render: Duration, quality: Quality) {
        let since = *self.since.get_or_insert(now);
        self.frames += 1;
        self.render += render;
        let elapsed = now.saturating_duration_since(since);
        if elapsed >= Self::EVERY {
            log::debug!(
                "visualizer: {:.1} fps, render call {:.2} ms/frame, {quality:?}",
                f64::from(self.frames) / elapsed.as_secs_f64(),
                self.render.as_secs_f64() * 1000.0 / f64::from(self.frames),
            );
            *self = FrameStats::default();
        }
    }
}

impl<A: AudioSource, R: Renderer> Session<A, R> {
    pub fn new(cfg: &VisualizerConfig, disabled: DisabledPresets) -> Self {
        let (base, floor) = quality_range(cfg);
        Session {
            base,
            floor,
            gl: Ok(()),
            open: false,
            opened_at: None,
            audio: None,
            audio_status: AudioStatus::Waiting,
            audio_error: None,
            renderer: None,
            renderer_error: None,
            released: Vec::new(),
            adaptive: Adaptive::new(base, floor),
            last_frame: None,
            overlay: None,
            status: None,
            pcm: Vec::new(),
            stats: FrameStats::default(),
            delay_ms: cfg.delay_ms,
            disabled,
            changed: Changed::default(),
            demo_active: false,
        }
    }

    /// The delay the next capture starts with (the configured one, or the last one set).
    pub fn delay_ms(&self) -> u32 {
        self.delay_ms
    }

    /// The presets switched off, for a renderer made on the GL thread.
    pub fn disabled(&self) -> &DisabledPresets {
        &self.disabled
    }

    pub fn set_beat_sensitivity(&mut self, value: f32) {
        self.changed.beat_sensitivity = Some(value);
        if let Some(r) = self.renderer.as_mut() {
            r.set_beat_sensitivity(value);
        }
    }

    pub fn set_preset_duration(&mut self, seconds: f64) {
        self.changed.preset_duration_s = Some(seconds);
        if let Some(r) = self.renderer.as_mut() {
            r.set_preset_duration(seconds);
        }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.changed.shuffle = Some(on);
        if let Some(r) = self.renderer.as_mut() {
            r.set_shuffle(on);
        }
    }

    /// Applies at once to a running capture, and to the next one.
    pub fn set_delay(&mut self, delay_ms: u32) {
        self.delay_ms = delay_ms;
        if let Some(a) = self.audio.as_mut() {
            a.set_delay(delay_ms);
        }
    }

    pub fn set_disabled(&mut self, disabled: DisabledPresets) {
        self.disabled = disabled;
        self.changed.disabled = true;
        if let Some(r) = self.renderer.as_mut() {
            r.set_disabled(&self.disabled);
        }
    }

    /// Shows one preset until [`Session::end_demo`]. The visualizer must be running with a
    /// renderer, else `Err(VizError::NotOpen)` (the UI opens it first).
    pub fn demo(&mut self, path: &Path) -> Result<(), VizError> {
        let renderer = self.renderer.as_mut().ok_or(VizError::NotOpen)?;
        renderer.demo(path)?;
        self.demo_active = true;
        Ok(())
    }

    pub fn end_demo(&mut self) {
        if std::mem::take(&mut self.demo_active)
            && let Some(r) = self.renderer.as_mut()
        {
            r.end_demo();
        }
    }

    /// Gives a new renderer the settings changed since the session began.
    fn apply_changes(&self, renderer: &mut R) {
        if let Some(v) = self.changed.beat_sensitivity {
            renderer.set_beat_sensitivity(v);
        }
        if let Some(s) = self.changed.preset_duration_s {
            renderer.set_preset_duration(s);
        }
        if let Some(on) = self.changed.shuffle {
            renderer.set_shuffle(on);
        }
        if self.changed.disabled {
            renderer.set_disabled(&self.disabled);
        }
    }

    /// The window cannot host OpenGL rendering (for example Slint's software renderer).
    pub fn gl_unavailable(&mut self, why: String) {
        self.gl = Err(why);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the screen. `start_audio` is called only if the visualizer can run at all.
    /// `track` is (title, artist) of what is playing, shown as the opening overlay.
    pub fn open(
        &mut self,
        now: Instant,
        start_audio: impl FnOnce() -> Result<A, String>,
        track: Option<(String, String)>,
    ) {
        if self.open {
            return;
        }
        self.open = true;
        self.opened_at = Some(now);
        // A failure last time (presets missing, PipeWire down) may be fixed now.
        self.audio_status = AudioStatus::Waiting;
        self.audio_error = None;
        self.renderer_error = None;
        self.adaptive = Adaptive::new(self.base, self.floor);
        self.last_frame = None;
        self.stats = FrameStats::default();
        self.status = None;
        self.overlay = track.map(|(title, artist)| (title, artist, now + TRACK_OVERLAY));
        if self.gl.is_ok() {
            match start_audio() {
                Ok(audio) => self.audio = Some(audio),
                Err(e) => {
                    log::warn!("visualizer: {e}");
                    self.audio_error = Some(e);
                }
            }
        }
    }

    /// Stops the audio now; the renderer waits in [`Session::take_released`] for the GL thread.
    pub fn close(&mut self) {
        self.end_demo();
        self.open = false;
        self.audio = None;
        self.released.extend(self.renderer.take());
        self.overlay = None;
        self.status = None;
        self.last_frame = None;
    }

    /// True when the GL thread should create a renderer and hand it to `attach_renderer`.
    pub fn needs_renderer(&self) -> bool {
        self.open
            && self.gl.is_ok()
            && self.renderer.is_none()
            && self.renderer_error.is_none()
            && self.audio_error.is_none()
    }

    pub fn attach_renderer(&mut self, renderer: Result<R, VizError>) {
        match renderer {
            Ok(mut renderer) if self.open => {
                self.apply_changes(&mut renderer);
                self.demo_active = false;
                self.renderer = Some(renderer);
            }
            Ok(renderer) => self.released.push(renderer),
            Err(e) => {
                log::warn!("visualizer: {e}");
                self.renderer_error = Some(e);
                // Nothing will use the audio until the screen is opened again.
                self.audio = None;
            }
        }
    }

    /// A closed renderer, to be dropped by the GL thread (its context must be current).
    pub fn take_released(&mut self) -> Option<R> {
        self.released.pop()
    }

    /// The GL context is going away: returns every renderer for the GL thread to drop. A new
    /// one is made when a context comes back.
    pub fn gl_lost(&mut self) -> Vec<R> {
        self.last_frame = None;
        self.demo_active = false;
        let mut all = std::mem::take(&mut self.released);
        all.extend(self.renderer.take());
        all
    }

    /// Picks up audio status changes. Returns true if the view may have changed.
    pub fn poll(&mut self) -> bool {
        let Some(status) = self.audio.as_mut().and_then(AudioSource::status) else {
            return false;
        };
        if status == self.audio_status {
            return false;
        }
        self.audio_status = status;
        // A gap with no frames must not count as one slow frame.
        self.last_frame = None;
        true
    }

    fn rendering(&self) -> bool {
        self.open
            && self.renderer.is_some()
            && self.audio.is_some()
            && self.audio_status == AudioStatus::Active
    }

    /// Draws one frame: feeds the audio due now and renders at the adaptive quality.
    pub fn frame(&mut self, now: Instant, width: u32, height: u32) {
        if !self.rendering() {
            return;
        }
        if let Some(last) = self.last_frame {
            let ms = now.saturating_duration_since(last).as_secs_f32() * 1000.0;
            self.adaptive.on_frame(ms);
        }
        self.last_frame = Some(now);
        let quality = self.adaptive.quality();
        let (Some(audio), Some(renderer)) = (self.audio.as_mut(), self.renderer.as_mut()) else {
            return;
        };
        audio.read(&mut self.pcm);
        renderer.feed_pcm(&self.pcm);
        let started = Instant::now();
        renderer.render(width, height, quality);
        self.stats.record(now, started.elapsed(), quality);
    }

    /// The FPS cap frames should be paced at.
    pub fn fps_cap(&self) -> u32 {
        self.adaptive.quality().fps_cap
    }

    pub fn track_changed(&mut self, now: Instant, title: String, artist: String) {
        if self.open {
            self.overlay = Some((title, artist, now + TRACK_OVERLAY));
        }
    }

    pub fn next_preset(&mut self, now: Instant) {
        if let Some(r) = self.renderer.as_mut() {
            r.next_preset();
            self.status = Some((r.current_preset_name(), now + STATUS_LINE));
        }
    }

    pub fn prev_preset(&mut self, now: Instant) {
        if let Some(r) = self.renderer.as_mut() {
            r.prev_preset();
            self.status = Some((r.current_preset_name(), now + STATUS_LINE));
        }
    }

    pub fn toggle_lock(&mut self, now: Instant) {
        if let Some(r) = self.renderer.as_mut() {
            let text = if r.toggle_lock() {
                "Locked"
            } else {
                "Unlocked"
            };
            self.status = Some((text.to_owned(), now + STATUS_LINE));
        }
    }

    pub fn adjust_beat_sensitivity(&mut self, now: Instant, delta: f32) {
        if let Some(r) = self.renderer.as_mut() {
            let value = r.adjust_beat_sensitivity(delta);
            self.status = Some((format!("Beat sensitivity {value:.1}"), now + STATUS_LINE));
        }
    }

    pub fn view(&self, now: Instant) -> View {
        if !self.open {
            return View::default();
        }
        let (message, detail) = self.message(now);
        View {
            rendering: self.rendering(),
            message: message.to_owned(),
            detail,
            overlay: self
                .overlay
                .as_ref()
                .filter(|(_, _, until)| now < *until)
                .map(|(title, artist, _)| (title.clone(), artist.clone())),
            status: self
                .status
                .as_ref()
                .filter(|(_, until)| now < *until)
                .map(|(text, _)| text.clone()),
        }
    }

    /// The explanation shown instead of the visuals, most fundamental problem first.
    fn message(&self, now: Instant) -> (&'static str, String) {
        if let Err(why) = &self.gl {
            return ("The visualizer needs OpenGL", why.clone());
        }
        match &self.renderer_error {
            Some(VizError::NoPresets(dirs)) => {
                let dir = dirs.first().map(|d| d.display().to_string());
                let detail = format!("Put .milk presets in {}", dir.unwrap_or_default());
                return ("No visualizer presets found", detail);
            }
            Some(VizError::GlUnavailable(why)) => {
                return ("The visualizer needs OpenGL", why.clone());
            }
            Some(VizError::Init(why)) => return ("The visualizer could not start", why.clone()),
            Some(VizError::AllDisabled) => {
                return (
                    "All presets are switched off",
                    "Switch some on in Settings → Presets".to_owned(),
                );
            }
            Some(other @ (VizError::NotOpen | VizError::PresetLoad(_))) => {
                return ("The visualizer could not start", other.to_string());
            }
            None => {}
        }
        if let Some(why) = &self.audio_error {
            return ("Cannot capture audio", why.clone());
        }
        let settled = self
            .opened_at
            .is_some_and(|at| now.saturating_duration_since(at) >= WAITING_GRACE);
        if self.audio_status != AudioStatus::Active && settled {
            return ("Waiting for audio…", "Play something on Spotify".to_owned());
        }
        ("", String::new())
    }
}
