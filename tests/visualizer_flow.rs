//! The visualizer screen's behaviour, with fake audio and renderer.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use htpc_nowplaying::audio::AudioStatus;
use htpc_nowplaying::config::{Config, VisualizerConfig};
use htpc_nowplaying::visualizer::{
    AudioSource, Renderer, STATUS_LINE, Session, TRACK_OVERLAY, View, WAITING_GRACE,
};
use htpc_nowplaying::viz::VizError;
use htpc_nowplaying::viz::adaptive::Quality;
use htpc_nowplaying::viz::disabled::DisabledPresets;

/// What the fakes saw, shared with the test.
#[derive(Default)]
struct Log {
    audio_started: u32,
    audio_dropped: u32,
    renderers_dropped: u32,
    fed: Vec<f32>,
    rendered: Vec<(u32, u32, Quality)>,
    presets: Vec<&'static str>,
    /// Status changes the fake audio reports on its next poll.
    statuses: Vec<AudioStatus>,
    /// Settings and demo calls the fakes received, in order.
    calls: Vec<String>,
}

type Shared = Rc<RefCell<Log>>;

struct FakeAudio {
    log: Shared,
    samples: Vec<f32>,
}

impl AudioSource for FakeAudio {
    fn read(&mut self, out: &mut Vec<f32>) {
        out.clear();
        out.append(&mut self.samples);
    }
    fn status(&mut self) -> Option<AudioStatus> {
        self.log.borrow_mut().statuses.drain(..).next_back()
    }
    fn set_delay(&mut self, delay_ms: u32) {
        self.log
            .borrow_mut()
            .calls
            .push(format!("delay {delay_ms}"));
    }
}

impl Drop for FakeAudio {
    fn drop(&mut self) {
        self.log.borrow_mut().audio_dropped += 1;
    }
}

struct FakeRenderer {
    log: Shared,
    locked: bool,
    sensitivity: f32,
    preset: usize,
}

const PRESETS: [&str; 3] = ["Geiss - Spiral", "Flexi - Mindblob", "Rovastar - Altars"];

impl Renderer for FakeRenderer {
    fn feed_pcm(&mut self, interleaved: &[f32]) {
        self.log.borrow_mut().fed.extend_from_slice(interleaved);
    }
    fn render(&mut self, width: u32, height: u32, quality: Quality) {
        self.log
            .borrow_mut()
            .rendered
            .push((width, height, quality));
    }
    fn next_preset(&mut self) {
        self.preset = (self.preset + 1) % PRESETS.len();
        self.log.borrow_mut().presets.push(PRESETS[self.preset]);
    }
    fn prev_preset(&mut self) {
        self.preset = (self.preset + PRESETS.len() - 1) % PRESETS.len();
        self.log.borrow_mut().presets.push(PRESETS[self.preset]);
    }
    fn toggle_lock(&mut self) -> bool {
        self.locked = !self.locked;
        self.locked
    }
    fn adjust_beat_sensitivity(&mut self, delta: f32) -> f32 {
        self.sensitivity = (self.sensitivity + delta).clamp(0.0, 5.0);
        self.sensitivity
    }
    fn current_preset_name(&self) -> String {
        PRESETS[self.preset].to_owned()
    }
    fn set_beat_sensitivity(&mut self, value: f32) -> f32 {
        self.sensitivity = value.clamp(0.0, 5.0);
        let calls = &mut self.log.borrow_mut().calls;
        calls.push(format!("sensitivity {value:.1}"));
        self.sensitivity
    }
    fn set_preset_duration(&mut self, seconds: f64) {
        let calls = &mut self.log.borrow_mut().calls;
        calls.push(format!("duration {seconds:.0}"));
    }
    fn set_shuffle(&mut self, on: bool) {
        self.log.borrow_mut().calls.push(format!("shuffle {on}"));
    }
    fn set_disabled(&mut self, disabled: &DisabledPresets) {
        let entries: Vec<&str> = disabled.entries().collect();
        let calls = &mut self.log.borrow_mut().calls;
        calls.push(format!("disabled {}", entries.join(",")));
    }
    fn demo(&mut self, path: &Path) -> Result<(), VizError> {
        let calls = &mut self.log.borrow_mut().calls;
        calls.push(format!("demo {}", path.display()));
        Ok(())
    }
    fn end_demo(&mut self) {
        self.log.borrow_mut().calls.push("end_demo".to_owned());
    }
}

impl Drop for FakeRenderer {
    fn drop(&mut self) {
        self.log.borrow_mut().renderers_dropped += 1;
    }
}

struct Harness {
    session: Session<FakeAudio, FakeRenderer>,
    log: Shared,
    t0: Instant,
}

impl Harness {
    fn new() -> Harness {
        Harness {
            session: Session::new(&viz_config(), DisabledPresets::default()),
            log: Shared::default(),
            t0: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    /// Opens with audio that reports `status` on the first poll.
    fn open(&mut self, status: Option<AudioStatus>) {
        let log = self.log.clone();
        let now = self.at(0);
        self.session.open(
            now,
            move || {
                let mut l = log.borrow_mut();
                l.audio_started += 1;
                l.statuses.extend(status);
                drop(l);
                Ok(FakeAudio {
                    log: log.clone(),
                    samples: vec![0.25; 8],
                })
            },
            Some(("Wonderful Nothing".to_owned(), "Glass Animals".to_owned())),
        );
    }

    /// What the GL thread does at the start of a frame.
    fn gl_pass(&mut self) {
        drop(self.session.take_released());
        if self.session.needs_renderer() {
            self.session.attach_renderer(Ok(FakeRenderer {
                log: self.log.clone(),
                locked: false,
                sensitivity: 1.0,
                preset: 0,
            }));
        }
    }

    /// Opened, renderer attached, audio active.
    fn running() -> Harness {
        let mut h = Harness::new();
        h.open(Some(AudioStatus::Active));
        h.gl_pass();
        h.session.poll();
        h
    }

    fn view(&self, ms: u64) -> View {
        self.session.view(self.at(ms))
    }
}

fn viz_config() -> VisualizerConfig {
    Config::default().visualizer
}

#[test]
fn open_creates_and_close_drops_capture_and_visualizer() {
    let mut h = Harness::running();
    assert!(h.session.is_open());
    assert_eq!(h.log.borrow().audio_started, 1);
    assert!(h.view(0).rendering);

    h.session.close();
    assert!(!h.session.is_open());
    assert_eq!(h.log.borrow().audio_dropped, 1, "audio stops at once");
    assert_eq!(
        h.log.borrow().renderers_dropped,
        0,
        "renderer waits for the GL thread"
    );
    h.gl_pass();
    assert_eq!(h.log.borrow().renderers_dropped, 1);
    assert!(!h.session.needs_renderer(), "closed: no new renderer");
}

#[test]
fn frames_feed_audio_and_render_at_window_size() {
    let mut h = Harness::running();
    h.session.frame(h.at(0), 1920, 1080);
    let log = h.log.borrow();
    assert_eq!(log.fed, vec![0.25; 8]);
    assert_eq!(log.rendered.len(), 1);
    let (w, hgt, q) = log.rendered[0];
    assert_eq!((w, hgt), (1920, 1080));
    assert_eq!(q.scale, 1.0);
    assert_eq!(q.fps_cap, viz_config().fps_cap);
}

#[test]
fn slow_frames_lower_the_quality() {
    let mut h = Harness::running();
    for i in 0..40 {
        h.session.frame(h.at(i * 100), 1920, 1080); // 100 ms per frame: far over budget
    }
    let last = h.log.borrow().rendered.last().unwrap().2;
    assert!(last.scale < 1.0, "{last:?}");
    assert!(h.session.fps_cap() < viz_config().fps_cap);
}

#[test]
fn waiting_for_audio_message_when_no_stream() {
    let grace = WAITING_GRACE.as_millis() as u64;
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Waiting));
    h.gl_pass();
    h.session.poll();
    let v = h.view(0);
    assert!(!v.rendering);
    assert_eq!(
        v.message, "",
        "held back: the stream usually turns up at once"
    );
    assert_eq!(h.view(grace - 1).message, "");
    assert_eq!(h.view(grace).message, "Waiting for audio…");

    // Before the first status arrives it is waiting too.
    let mut h = Harness::new();
    h.open(None);
    h.gl_pass();
    assert_eq!(h.view(grace).message, "Waiting for audio…");
}

#[test]
fn errors_are_not_held_back() {
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Waiting));
    h.session
        .attach_renderer(Err(VizError::NoPresets(vec![PathBuf::from("/p")])));
    assert_eq!(h.view(0).message, "No visualizer presets found");
}

#[test]
fn stream_appearing_starts_rendering_and_loss_stops_it() {
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Waiting));
    h.gl_pass();
    h.session.poll();
    assert!(!h.view(0).rendering);

    h.log.borrow_mut().statuses.push(AudioStatus::Active);
    assert!(h.session.poll(), "status change reported");
    assert!(h.view(0).rendering);
    assert_eq!(h.view(0).message, "");
    assert!(!h.session.poll(), "nothing new");

    // Playback moved to another device: back to waiting, nothing rendered.
    h.log.borrow_mut().statuses.push(AudioStatus::Lost);
    assert!(h.session.poll());
    assert!(!h.view(5000).rendering);
    assert_eq!(h.view(5000).message, "Waiting for audio…");
    h.session.frame(h.at(5000), 1920, 1080);
    assert!(h.log.borrow().rendered.is_empty());
}

#[test]
fn audio_that_cannot_start_shows_why() {
    let mut h = Harness::new();
    let now = h.at(0);
    h.session.open(
        now,
        || Err("PipeWire audio capture: cannot connect".to_owned()),
        None,
    );
    h.gl_pass();
    let v = h.view(0);
    assert!(!v.rendering);
    assert_eq!(v.message, "Cannot capture audio");
    assert!(v.detail.contains("cannot connect"), "{}", v.detail);
}

#[test]
fn no_presets_shows_message_and_back_works() {
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Active));
    let dir = PathBuf::from("/home/me/.local/share/projectM/presets");
    h.session
        .attach_renderer(Err(VizError::NoPresets(vec![dir.clone()])));
    h.session.poll();
    let v = h.view(0);
    assert!(!v.rendering);
    assert_eq!(v.message, "No visualizer presets found");
    assert!(
        v.detail.contains(&dir.display().to_string()),
        "{}",
        v.detail
    );
    assert!(!h.session.needs_renderer(), "does not retry every frame");

    // Preset keys do nothing; Back closes.
    h.session.next_preset(h.at(10));
    h.session.toggle_lock(h.at(10));
    h.session.close();
    assert!(!h.session.is_open());
    assert_eq!(h.log.borrow().audio_dropped, 1);
}

#[test]
fn gl_unavailable_shows_message_and_back_works() {
    let mut h = Harness::new();
    h.session
        .gl_unavailable("the software renderer has no OpenGL".to_owned());
    h.open(Some(AudioStatus::Active));
    assert_eq!(
        h.log.borrow().audio_started,
        0,
        "no capture without a renderer"
    );
    assert!(!h.session.needs_renderer());
    let v = h.view(0);
    assert!(!v.rendering);
    assert_eq!(v.message, "The visualizer needs OpenGL");
    assert!(v.detail.contains("software renderer"));
    h.session.close();
    assert!(!h.session.is_open());

    // projectM refusing the context is reported the same way.
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Active));
    h.session
        .attach_renderer(Err(VizError::GlUnavailable("too old".to_owned())));
    assert_eq!(h.view(0).message, "The visualizer needs OpenGL");
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Active));
    h.session.attach_renderer(Err(VizError::Init(
        "libprojectM-4 is not installed".to_owned(),
    )));
    assert_eq!(h.view(0).message, "The visualizer could not start");
    assert!(h.view(0).detail.contains("not installed"));
}

#[test]
fn track_change_shows_overlay_then_hides() {
    let mut h = Harness::running();
    let opening = h.view(0).overlay;
    assert_eq!(
        opening,
        Some(("Wonderful Nothing".to_owned(), "Glass Animals".to_owned())),
        "shown on open"
    );
    let gone = TRACK_OVERLAY.as_millis() as u64;
    assert_eq!(h.view(gone).overlay, None);

    h.session.track_changed(
        h.at(10_000),
        "Heat Waves".to_owned(),
        "Glass Animals".to_owned(),
    );
    assert_eq!(h.view(10_000).overlay.unwrap().0, "Heat Waves");
    assert!(h.view(10_000 + gone - 1).overlay.is_some());
    assert_eq!(h.view(10_000 + gone).overlay, None);
}

#[test]
fn lock_toggle_shows_status() {
    let mut h = Harness::running();
    h.session.toggle_lock(h.at(1000));
    assert_eq!(h.view(1000).status.as_deref(), Some("Locked"));
    h.session.toggle_lock(h.at(1500));
    assert_eq!(h.view(1500).status.as_deref(), Some("Unlocked"));
    let gone = 1500 + STATUS_LINE.as_millis() as u64;
    assert_eq!(h.view(gone).status, None);
}

#[test]
fn preset_change_shows_its_name() {
    let mut h = Harness::running();
    h.session.next_preset(h.at(0));
    assert_eq!(h.view(0).status.as_deref(), Some("Flexi - Mindblob"));
    h.session.prev_preset(h.at(0));
    h.session.prev_preset(h.at(0));
    assert_eq!(h.view(0).status.as_deref(), Some("Rovastar - Altars"));
    assert_eq!(
        h.log.borrow().presets,
        ["Flexi - Mindblob", "Geiss - Spiral", "Rovastar - Altars"]
    );
}

#[test]
fn beat_sensitivity_clamped_and_shown() {
    let mut h = Harness::running();
    h.session.adjust_beat_sensitivity(h.at(0), 0.1);
    assert_eq!(h.view(0).status.as_deref(), Some("Beat sensitivity 1.1"));
    for _ in 0..100 {
        h.session.adjust_beat_sensitivity(h.at(0), 0.1);
    }
    assert_eq!(h.view(0).status.as_deref(), Some("Beat sensitivity 5.0"));
    for _ in 0..100 {
        h.session.adjust_beat_sensitivity(h.at(0), -0.1);
    }
    assert_eq!(h.view(0).status.as_deref(), Some("Beat sensitivity 0.0"));
}

#[test]
fn reopening_after_close_works() {
    let mut h = Harness::running();
    h.session.close();
    h.gl_pass();
    h.open(Some(AudioStatus::Active));
    assert!(h.session.needs_renderer());
    h.gl_pass();
    h.session.poll();
    assert!(h.view(0).rendering);
    h.session.frame(h.at(0), 640, 360);
    let log = h.log.borrow();
    assert_eq!((log.audio_started, log.audio_dropped), (2, 1));
    assert_eq!(log.renderers_dropped, 1);
    assert_eq!(log.rendered.len(), 1);
}

#[test]
fn losing_the_gl_context_drops_the_renderer_and_rebuilds_it() {
    let mut h = Harness::running();
    drop(h.session.gl_lost());
    assert_eq!(h.log.borrow().renderers_dropped, 1);
    assert!(!h.view(0).rendering);
    assert!(h.session.needs_renderer());
    h.gl_pass();
    assert!(h.view(0).rendering);
}

#[test]
fn live_settings_reach_the_renderer() {
    let mut h = Harness::running();
    h.session.set_beat_sensitivity(2.5);
    h.session.set_preset_duration(45.0);
    h.session.set_shuffle(false);
    h.session.set_delay(120);
    h.session
        .set_disabled(DisabledPresets::from_lines("Fractal/\nx.milk\n"));
    assert_eq!(
        h.log.borrow().calls,
        [
            "sensitivity 2.5",
            "duration 45",
            "shuffle false",
            "delay 120",
            "disabled Fractal/,x.milk",
        ]
    );
    assert_eq!(h.session.delay_ms(), 120);
    assert!(h.session.disabled().contains("x.milk"));
}

#[test]
fn settings_set_while_closed_apply_on_open() {
    let mut h = Harness::new();
    h.session.set_beat_sensitivity(0.5);
    h.session.set_shuffle(false);
    h.session.set_delay(200);
    h.session.set_disabled(DisabledPresets::from_lines("a/\n"));
    assert!(h.log.borrow().calls.is_empty(), "no renderer or audio yet");
    assert_eq!(h.session.delay_ms(), 200, "the next capture starts with it");

    h.open(Some(AudioStatus::Active));
    h.gl_pass();
    assert_eq!(
        h.log.borrow().calls,
        ["sensitivity 0.5", "shuffle false", "disabled a/"],
        "settings never touched stay as configured"
    );
}

#[test]
fn all_disabled_shows_message_and_back_works() {
    let mut h = Harness::new();
    h.open(Some(AudioStatus::Active));
    h.session.attach_renderer(Err(VizError::AllDisabled));
    h.session.poll();
    let v = h.view(0);
    assert!(!v.rendering);
    assert_eq!(v.message, "All presets are switched off");
    assert_eq!(v.detail, "Switch some on in Settings → Presets");
    assert!(!h.session.needs_renderer(), "does not retry every frame");
    h.session.close();
    assert!(!h.session.is_open());
    assert_eq!(h.log.borrow().audio_dropped, 1);
}

#[test]
fn demo_pauses_and_end_demo_resumes() {
    let mut h = Harness::running();
    let preset = Path::new("/p/A/a1.milk");
    h.session.demo(preset).unwrap();
    h.session.frame(h.at(0), 1920, 1080);
    h.session.end_demo();
    h.session.end_demo(); // without a demo it does nothing
    assert_eq!(
        h.log.borrow().calls,
        ["demo /p/A/a1.milk", "end_demo"],
        "one demo, one end"
    );
}

#[test]
fn demo_when_closed_is_not_open() {
    let mut h = Harness::new();
    assert_eq!(
        h.session.demo(Path::new("/p/a.milk")),
        Err(VizError::NotOpen)
    );
    // Opened but no renderer yet: the same.
    h.open(Some(AudioStatus::Active));
    assert_eq!(
        h.session.demo(Path::new("/p/a.milk")),
        Err(VizError::NotOpen)
    );
    assert!(h.log.borrow().calls.is_empty());
}

#[test]
fn close_ends_demo() {
    let mut h = Harness::running();
    h.session.demo(Path::new("/p/a.milk")).unwrap();
    h.session.close();
    assert_eq!(h.log.borrow().calls, ["demo /p/a.milk", "end_demo"]);
    // Nothing is left to end after reopening.
    h.gl_pass();
    h.open(Some(AudioStatus::Active));
    h.gl_pass();
    h.session.end_demo();
    assert_eq!(h.log.borrow().calls.len(), 2);
}

#[test]
fn closed_session_draws_nothing() {
    let mut h = Harness::running();
    h.session.close();
    h.session.frame(h.at(0), 1920, 1080);
    assert!(h.log.borrow().rendered.is_empty());
    assert_eq!(h.view(0), View::default());
}
