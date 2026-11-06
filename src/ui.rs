//! The Slint front-end: shows the model in the window and turns key presses into actions.

use std::cell::RefCell;
use std::error::Error;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use slint::{
    ComponentHandle, GraphicsAPI, RenderingState, Rgba8Pixel, SetRenderingNotifierError,
    SharedPixelBuffer, Timer, TimerMode,
};
use tokio::sync::mpsc;

use crate::config::{Config, disabled_presets_path};
use crate::cover::{CoverCache, DecodedImage};
use crate::input::{Action, Focus, Screen, UiState, handle_key};
use crate::model::{Command, NowPlaying, Playback, PlayerEvent};
use crate::player;
use crate::visualizer::{LiveAudio, Session, View};
use crate::viz::disabled::DisabledPresets;
use crate::viz::{GlContext, Visualizer, VizError};

slint::include_modules!();

/// Covers are fetched at Spotify's largest size and drawn scaled.
const COVER_PX: u32 = 640;
/// How often the visualizer screen checks the audio while it shows a message instead of visuals.
const VIZ_POLL: Duration = Duration::from_millis(100);

type VizSession = Session<LiveAudio, Visualizer>;

/// Runs the UI until its window is closed.
///
/// Threads: Slint owns the main thread and all UI state. Networking (Soloist, covers) runs on a
/// small tokio runtime; its results come back through `slint::spawn_local`, so the UI state
/// never needs locking.
pub fn run_ui(cfg: Config) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("htpc-io")
        .enable_all()
        .build()?;
    let app = App::new()?;

    let (events_tx, mut events) = mpsc::channel(64);
    let client = {
        let _context = runtime.enter();
        player::spawn(cfg.daemon.clone(), events_tx)
    };
    let covers = Arc::new(CoverCache::new()?);
    let io = runtime.handle().clone();
    // Set before the native window exists, so it is part of the initial window request. The
    // compositor may still refuse it, and Slint then follows the compositor's answer.
    app.window().set_fullscreen(cfg.fullscreen);

    let send: CommandSink = Box::new(move |cmd| client.send(cmd));
    let load_cover: CoverLoader = Box::new(move |url, controller| {
        let covers = covers.clone();
        let fetch = {
            let url = url.clone();
            io.spawn(async move { covers.load(&url, COVER_PX).await })
        };
        let deliver = slint::spawn_local(async move {
            let image = fetch.await.ok().flatten();
            if let Some(controller) = controller.upgrade() {
                controller.borrow_mut().cover_loaded(&url, image);
            }
        });
        if let Err(e) = deliver {
            log::warn!("cannot load cover: {e}");
        }
    });
    let controller = Controller::new(app.as_weak(), cfg, send, load_cover);
    install_renderer(&app, &controller);

    let weak = Rc::downgrade(&controller);
    app.on_key(move |text| {
        weak.upgrade()
            .is_some_and(|controller| controller.borrow_mut().key(&text))
    });
    let weak = Rc::downgrade(&controller);
    slint::spawn_local(async move {
        while let Some(ev) = events.recv().await {
            let Some(controller) = weak.upgrade() else {
                break;
            };
            controller.borrow_mut().apply(ev);
        }
    })?;

    controller.borrow_mut().refresh();
    app.run()?;

    // A renderer still alive here outlived its GL context (no teardown before exit); freeing
    // it without the context could crash, and the process is ending anyway.
    let viz = controller.borrow().viz.clone();
    for renderer in viz.borrow_mut().gl_lost() {
        std::mem::forget(renderer);
    }
    // Dropping the controller drops the Soloist client, which closes the connection.
    drop(controller);
    runtime.shutdown_timeout(Duration::from_secs(1));
    Ok(())
}

/// Hooks projectM into the window's rendering: it is created, fed and drawn right before Slint
/// draws, on the GL context Slint has current, and dropped there too. Without OpenGL (Slint's
/// software renderer) the visualizer screen explains that instead.
fn install_renderer(app: &App, controller: &Rc<RefCell<Controller>>) {
    let session = controller.borrow().viz.clone();
    let cfg = controller.borrow().cfg.visualizer.clone();
    let window = app.as_weak();
    let me = Rc::downgrade(controller);
    let hook = session.clone();
    let result = app.window().set_rendering_notifier(move |state, api| {
        let Ok(mut s) = hook.try_borrow_mut() else {
            return;
        };
        match state {
            RenderingState::BeforeRendering => {
                while let Some(closed) = s.take_released() {
                    drop(closed);
                }
                if s.needs_renderer() {
                    let made = match api {
                        GraphicsAPI::NativeOpenGL { get_proc_address } => {
                            // SAFETY: Slint keeps its GL context current around this callback,
                            // and the visualizer is only used and dropped from here.
                            let gl = unsafe { GlContext::current(get_proc_address) };
                            Visualizer::new(gl, &cfg, &s.disabled().clone())
                        }
                        _ => Err(VizError::GlUnavailable(
                            "the window does not render with OpenGL".to_owned(),
                        )),
                    };
                    s.attach_renderer(made);
                    // Visuals or an error now; update the window once this frame is done.
                    let me = me.clone();
                    Timer::single_shot(Duration::ZERO, move || {
                        if let Some(controller) = me.upgrade() {
                            controller.borrow_mut().refresh();
                        }
                    });
                }
                if let Some(app) = window.upgrade() {
                    let size = app.window().size();
                    s.frame(Instant::now(), size.width, size.height);
                }
            }
            RenderingState::RenderingTeardown => drop(s.gl_lost()),
            // For checking the idle cost: `RUST_LOG=htpc_nowplaying::ui=trace` logs each frame.
            RenderingState::AfterRendering => log::trace!("frame drawn"),
            _ => {}
        }
    });
    if let Err(e) = result {
        let why = match e {
            SetRenderingNotifierError::Unsupported => {
                "the window is drawn without OpenGL (software renderer)".to_owned()
            }
            other => other.to_string(),
        };
        log::info!("visualizer unavailable: {why}");
        session.borrow_mut().gl_unavailable(why);
    }
}

type CommandSink = Box<dyn Fn(Command)>;
type CoverLoader = Box<dyn Fn(String, Weak<RefCell<Controller>>)>;

/// Owns the UI state on the UI thread and keeps the window in sync with it.
struct Controller {
    app: slint::Weak<App>,
    me: Weak<RefCell<Controller>>,
    cfg: Config,
    player: NowPlaying,
    ui: UiState,
    /// Re-armed for the next whole second of playback while the progress is visible and moving.
    progress: Timer,
    /// The cover URL the window should show; results for other URLs are stale.
    cover_wanted: Option<String>,
    send: CommandSink,
    load_cover: CoverLoader,
    /// Shared with the rendering hook, which creates and draws the renderer.
    viz: Rc<RefCell<VizSession>>,
    /// Paces the visualizer: frames at its FPS cap while it draws, slow polling otherwise,
    /// stopped while it is closed.
    frames: Timer,
    frame_interval: Option<Duration>,
}

impl Controller {
    fn new(
        app: slint::Weak<App>,
        cfg: Config,
        send: CommandSink,
        load_cover: CoverLoader,
    ) -> Rc<RefCell<Controller>> {
        Rc::new_cyclic(|me| {
            RefCell::new(Controller {
                app,
                me: me.clone(),
                viz: Rc::new(RefCell::new(Session::new(
                    &cfg.visualizer,
                    DisabledPresets::load(&disabled_presets_path()),
                ))),
                cfg,
                player: NowPlaying::default(),
                ui: UiState::default(),
                progress: Timer::default(),
                cover_wanted: None,
                send,
                load_cover,
                frames: Timer::default(),
                frame_interval: None,
            })
        })
    }

    fn apply(&mut self, ev: PlayerEvent) {
        let now = Instant::now();
        let before = self.track_names();
        self.player.apply(ev, now, unix_now_ms());
        let after = self.track_names();
        if after != before
            && let Some((title, artist)) = after
        {
            self.viz.borrow_mut().track_changed(now, title, artist);
        }
        self.refresh();
    }

    /// Title and artist of the current track, for the visualizer's overlay.
    fn track_names(&self) -> Option<(String, String)> {
        let track = self.player.track.as_ref()?;
        Some((track.title.clone(), track.artist.clone()))
    }

    /// Handles a key press; returns false for keys that are not bound.
    fn key(&mut self, text: &str) -> bool {
        let Some(key) = key_name(text).and_then(|name| self.cfg.keys.get(&name)) else {
            return false;
        };
        let now = Instant::now();
        // The screen switch itself already happened in `handle_key`.
        match handle_key(&mut self.ui, key, &self.cfg) {
            // The idle screen has no controls. A command sent while another device plays
            // could reach that device's playback through Soloist.
            Action::Send(cmd) if self.player.track.is_none() => {
                log::debug!("ignoring {cmd:?}: nothing is playing here");
            }
            Action::Send(cmd) => (self.send)(cmd),
            Action::OpenVisualizer => {
                let track = self.track_names();
                let mut viz = self.viz.borrow_mut();
                let delay_ms = viz.delay_ms();
                viz.open(now, || LiveAudio::start(delay_ms), track);
            }
            Action::CloseVisualizer => self.viz.borrow_mut().close(),
            Action::PreviousPreset => self.viz.borrow_mut().prev_preset(now),
            Action::NextPreset => self.viz.borrow_mut().next_preset(now),
            Action::TogglePresetLock => self.viz.borrow_mut().toggle_lock(now),
            Action::BeatSensitivityDelta(delta) => {
                self.viz.borrow_mut().adjust_beat_sensitivity(now, delta);
            }
            // Wired up with the settings panel.
            Action::OpenSettings | Action::None => {}
        }
        self.refresh();
        true
    }

    /// One visualizer tick: picks up audio changes, updates the overlays and asks for a frame
    /// when there is something to draw (or a renderer to create).
    fn viz_tick(&mut self) {
        self.viz.borrow_mut().poll();
        let view = self.refresh_viz(Instant::now());
        let needs_renderer = self.viz.borrow().needs_renderer();
        if (view.rendering || needs_renderer)
            && let Some(app) = self.app.upgrade()
        {
            app.window().request_redraw();
        }
    }

    /// Pushes the visualizer's view into the window and paces its frames.
    fn refresh_viz(&mut self, now: Instant) -> View {
        let view = self.viz.borrow().view(now);
        if let Some(app) = self.app.upgrade() {
            app.set_viz_rendering(view.rendering);
            app.set_viz_message(view.message.as_str().into());
            app.set_viz_detail(view.detail.as_str().into());
            // Texts stay set while hidden, so they fade out instead of vanishing.
            if let Some((title, artist)) = &view.overlay {
                app.set_viz_overlay_title(title.as_str().into());
                app.set_viz_overlay_artist(artist.as_str().into());
            }
            app.set_viz_overlay_visible(view.overlay.is_some());
            if let Some(status) = &view.status {
                app.set_viz_status(status.as_str().into());
            }
            app.set_viz_status_visible(view.status.is_some());
        }
        self.schedule_frames(&view);
        view
    }

    fn schedule_frames(&mut self, view: &View) {
        let interval = if !self.viz.borrow().is_open() {
            None
        } else if view.rendering {
            let fps = self.viz.borrow().fps_cap().max(1);
            Some(Duration::from_secs(1) / fps)
        } else {
            Some(VIZ_POLL)
        };
        if interval == self.frame_interval {
            return;
        }
        self.frame_interval = interval;
        match interval {
            None => self.frames.stop(),
            Some(interval) => {
                let me = self.me.clone();
                self.frames.start(TimerMode::Repeated, interval, move || {
                    if let Some(controller) = me.upgrade() {
                        controller.borrow_mut().viz_tick();
                    }
                });
            }
        }
    }

    fn cover_loaded(&mut self, url: &str, image: Option<Arc<DecodedImage>>) {
        if self.cover_wanted.as_deref() != Some(url) {
            return; // the track changed while this cover was loading
        }
        let (Some(image), Some(app)) = (image, self.app.upgrade()) else {
            return;
        };
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &image.rgba,
            image.width,
            image.height,
        );
        app.set_cover_image(slint::Image::from_rgba8(pixels));
        app.set_has_cover(true);
    }

    /// Pushes the current state into the window and (re)arms or stops the progress timer.
    fn refresh(&mut self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let now = Instant::now();
        let player = &self.player;
        app.set_connected(player.connected);
        app.set_idle(player.track.is_none());
        app.set_visualizer(self.ui.screen == Screen::Visualizer);
        app.set_device_name(player.device_name.clone().unwrap_or_default().into());
        app.set_focused_button(match self.ui.focus {
            Focus::Previous => 0,
            Focus::PlayPause => 1,
            Focus::Next => 2,
            Focus::Visualizer => 3,
        });
        // Buffering counts as playing: OK would pause it.
        app.set_playing(matches!(
            player.playback,
            Playback::Playing | Playback::Buffering
        ));
        if let Some(track) = &player.track {
            let position = player.position_ms(now);
            app.set_track_title(track.title.as_str().into());
            app.set_artist(track.artist.as_str().into());
            app.set_album(track.album.clone().unwrap_or_default().into());
            app.set_elapsed(format_time(position).into());
            if track.duration_ms > 0 {
                app.set_total(format_time(track.duration_ms).into());
                app.set_progress(position as f32 / track.duration_ms as f32);
            } else {
                app.set_total("".into());
                app.set_progress(0.0);
            }
        }

        let cover_url = player.track.as_ref().and_then(|t| t.cover_url.clone());
        if cover_url != self.cover_wanted {
            app.set_has_cover(false);
            self.cover_wanted = cover_url.clone();
            if let Some(url) = cover_url {
                (self.load_cover)(url, self.me.clone());
            }
        }

        self.schedule_tick(now);
        self.refresh_viz(now);
    }

    /// Ticks only while the progress is on screen and moving, at the next whole second of
    /// playback so the elapsed time turns over in step with the music.
    fn schedule_tick(&mut self, now: Instant) {
        let moving = self.player.playback == Playback::Playing
            && self.player.connected
            && self.player.track.is_some()
            && self.ui.screen == Screen::NowPlaying;
        if !moving {
            self.progress.stop();
            return;
        }
        let into_second = self.player.position_ms(now) % 1000;
        let delay = Duration::from_millis(1000 - into_second + 5);
        let me = self.me.clone();
        self.progress.start(TimerMode::SingleShot, delay, move || {
            if let Some(controller) = me.upgrade() {
                controller.borrow_mut().refresh();
            }
        });
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The key name for a Slint key event text, matching the names used in `[keys]` in the config
/// (keysym names). `None` for keys that cannot be bound.
pub fn key_name(text: &str) -> Option<String> {
    use slint::platform::Key;
    const NAMED: [(Key, &str); 17] = [
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::Return, "Return"),
        (Key::Escape, "Escape"),
        (Key::Backspace, "BackSpace"),
        (Key::Tab, "Tab"),
        (Key::Space, "space"),
        (Key::Delete, "Delete"),
        (Key::Insert, "Insert"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::PageUp, "Page_Up"),
        (Key::PageDown, "Page_Down"),
        (Key::Menu, "Menu"),
        (Key::Pause, "Pause"),
    ];
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if let Some((_, name)) = NAMED.iter().find(|(key, _)| char::from(*key) == c) {
        return Some((*name).to_owned());
    }
    let (f1, f24) = (char::from(Key::F1), char::from(Key::F24));
    if (f1..=f24).contains(&c) {
        return Some(format!("F{}", u32::from(c) - u32::from(f1) + 1));
    }
    // Slint encodes other special keys as control characters or private-use code points.
    let private_use = ('\u{E000}'..='\u{F8FF}').contains(&c);
    (!c.is_control() && !private_use).then(|| c.to_string())
}

/// `m:ss`, or `h:mm:ss` from one hour on.
fn format_time(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;
    use i_slint_backend_testing::ElementHandle;
    use slint::platform::Key;

    struct Harness {
        app: App,
        controller: Rc<RefCell<Controller>>,
        sent: Rc<RefCell<Vec<Command>>>,
        covers_requested: Rc<RefCell<Vec<String>>>,
    }

    impl Harness {
        fn new(cfg: Config) -> Harness {
            thread_local!(static INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
            // The testing platform can be set once per test thread.
            if !INIT.replace(true) {
                i_slint_backend_testing::init_no_event_loop();
            }
            let app = App::new().unwrap();
            app.window()
                .set_size(slint::LogicalSize::new(1920.0, 1080.0));
            let sent = Rc::new(RefCell::new(Vec::new()));
            let covers_requested = Rc::new(RefCell::new(Vec::new()));
            let sink = sent.clone();
            let requests = covers_requested.clone();
            let controller = Controller::new(
                app.as_weak(),
                cfg,
                Box::new(move |cmd| sink.borrow_mut().push(cmd)),
                Box::new(move |url, _| requests.borrow_mut().push(url)),
            );
            // The testing backend has no OpenGL: the visualizer explains that.
            install_renderer(&app, &controller);
            controller.borrow_mut().refresh();
            Harness {
                app,
                controller,
                sent,
                covers_requested,
            }
        }

        fn apply(&self, events: impl IntoIterator<Item = PlayerEvent>) {
            for ev in events {
                self.controller.borrow_mut().apply(ev);
            }
        }

        fn press(&self, key: Key) -> bool {
            let text: slint::SharedString = key.into();
            self.controller.borrow_mut().key(&text)
        }

        fn playing(&self, track: Track) {
            let now_ms = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            self.apply([
                PlayerEvent::Connected,
                PlayerEvent::DeviceName("HTPC".to_owned()),
                PlayerEvent::DeviceActive(true),
                PlayerEvent::TrackChanged(Some(track)),
                PlayerEvent::PlaybackChanged(Playback::Playing),
                PlayerEvent::Position {
                    position_ms: 34_772,
                    timestamp_ms: now_ms,
                    speed: 1.0,
                },
            ]);
        }

        fn timer_running(&self) -> bool {
            self.controller.borrow().progress.running()
        }

        fn exists(&self, id: &str) -> bool {
            ElementHandle::find_by_element_id(&self.app, id)
                .next()
                .is_some()
        }

        fn element(&self, id: &str) -> ElementHandle {
            let mut found = ElementHandle::find_by_element_id(&self.app, id);
            let element = found.next().unwrap_or_else(|| panic!("{id} not shown"));
            assert!(found.next().is_none(), "{id} shown more than once");
            element
        }

        fn label(&self, id: &str) -> String {
            self.element(id)
                .accessible_label()
                .unwrap_or_default()
                .to_string()
        }

        /// The button whose area contains the focus ring.
        fn ring_inside(&self, button: &str) -> bool {
            let ring = self.element("RowButton::ring");
            let b = self.element(button);
            let (r, bp, bs) = (ring.absolute_position(), b.absolute_position(), b.size());
            r.x >= bp.x && r.x < bp.x + bs.width && r.y >= bp.y && r.y < bp.y + bs.height
        }
    }

    fn track(title: &str) -> Track {
        Track {
            title: title.to_owned(),
            artist: "Glass Animals".to_owned(),
            album: Some("I Love You So F***ing Much".to_owned()),
            cover_url: Some("https://i.scdn.co/image/a".to_owned()),
            duration_ms: 264_214,
        }
    }

    fn harness() -> Harness {
        Harness::new(Config::default())
    }

    #[test]
    fn idle_shown_when_no_track() {
        let h = harness();
        h.apply([
            PlayerEvent::Connected,
            PlayerEvent::DeviceName("HTPC".to_owned()),
            PlayerEvent::DeviceActive(false),
        ]);
        assert!(h.exists("App::idle-view"));
        assert!(!h.exists("App::now-playing"));
        assert_eq!(h.label("App::idle-hint"), "Open Spotify and pick");
        assert_eq!(h.label("App::idle-device"), "HTPC");
    }

    #[test]
    fn idle_without_device_name_still_reads_well() {
        let h = harness();
        assert!(h.exists("App::idle-view"));
        assert_eq!(h.label("App::idle-device"), "this device");
    }

    #[test]
    fn now_playing_shows_title_artist_album() {
        let h = harness();
        h.playing(track("Wonderful Nothing"));
        assert!(h.exists("App::now-playing"));
        assert!(!h.exists("App::idle-view"));
        assert_eq!(h.label("App::title-text"), "Wonderful Nothing");
        assert_eq!(h.label("App::artist-text"), "Glass Animals");
        assert_eq!(h.label("App::album-text"), "I Love You So F***ing Much");
        assert_eq!(h.label("App::elapsed-text"), "0:34");
        assert_eq!(h.label("App::total-text"), "4:24");
    }

    #[test]
    fn missing_album_hides_album_line() {
        let h = harness();
        h.playing(Track {
            album: None,
            ..track("A")
        });
        assert!(!h.exists("App::album-text"));
        assert!(h.exists("App::artist-text"));
    }

    #[test]
    fn episode_without_artist_shows_show_name() {
        let h = harness();
        h.playing(Track {
            artist: String::new(),
            album: Some("The Essential".to_owned()),
            ..track("Episode 12")
        });
        assert!(!h.exists("App::artist-text"));
        assert_eq!(h.label("App::album-text"), "The Essential");
    }

    #[test]
    fn long_title_does_not_resize_layout() {
        let h = harness();
        h.playing(track("Short"));
        let title = h.element("App::title-text").size();
        let next = h.element("App::next-button").absolute_position();

        h.apply([PlayerEvent::TrackChanged(Some(track(
            &"An extremely long title that goes on ".repeat(60),
        )))]);
        assert_eq!(h.element("App::title-text").size(), title);
        assert_eq!(h.element("App::next-button").absolute_position(), next);
        assert!(title.width < 1920.0 * 0.6, "title stays inside its column");
    }

    #[test]
    fn emoji_title_does_not_panic() {
        let h = harness();
        h.playing(track("Jóga — Björk 🎵✨ 日本語"));
        assert_eq!(h.label("App::title-text"), "Jóga — Björk 🎵✨ 日本語");
    }

    #[test]
    fn reconnecting_banner_when_disconnected() {
        let h = harness();
        h.playing(track("A"));
        let banner = h.element("App::banner");
        assert_eq!(banner.computed_opacity(), 0.0);

        h.apply([PlayerEvent::Disconnected]);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert_eq!(banner.computed_opacity(), 0.0, "not shown for a brief drop");
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1500));
        assert_eq!(banner.computed_opacity(), 1.0);
        assert!(h.label("App::banner-text").contains("Can't reach Soloist"));
        assert!(h.label("App::banner-text").contains("90 days"));
        assert!(h.exists("App::now-playing"), "last track stays visible");

        h.apply([PlayerEvent::Connected]);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert_eq!(banner.computed_opacity(), 0.0);
    }

    #[test]
    fn focus_ring_follows_state() {
        let h = harness();
        h.playing(track("A"));
        assert!(h.ring_inside("App::play-button"));
        assert!(h.press(Key::RightArrow));
        assert!(h.ring_inside("App::next-button"));
        h.press(Key::RightArrow);
        h.press(Key::RightArrow);
        assert!(h.ring_inside("App::viz-button"), "clamped at the end");
        for _ in 0..5 {
            h.press(Key::LeftArrow);
        }
        assert!(h.ring_inside("App::prev-button"));
    }

    #[test]
    fn ok_sends_the_focused_command() {
        let h = harness();
        h.playing(track("A"));
        h.press(Key::Return);
        h.press(Key::RightArrow);
        h.press(Key::Return);
        assert_eq!(*h.sent.borrow(), [Command::PlayPause, Command::Next]);
    }

    #[test]
    fn volume_keys_follow_audio_controls_setting() {
        let h = harness();
        h.playing(track("A"));
        assert!(
            h.press(Key::UpArrow),
            "bound key is consumed even when it does nothing"
        );
        assert!(h.sent.borrow().is_empty());

        let h = Harness::new(Config {
            audio_controls: true,
            ..Config::default()
        });
        h.playing(track("A"));
        h.press(Key::UpArrow);
        h.press(Key::DownArrow);
        assert_eq!(
            *h.sent.borrow(),
            [Command::VolumeDelta(5), Command::VolumeDelta(-5)]
        );
    }

    #[test]
    fn idle_screen_sends_no_commands() {
        let h = Harness::new(Config {
            audio_controls: true,
            ..Config::default()
        });
        h.apply([
            PlayerEvent::Connected,
            PlayerEvent::DeviceActive(false),
            // Relayed from the phone while this device is inactive.
            PlayerEvent::TrackChanged(Some(track("Phone song"))),
            PlayerEvent::PlaybackChanged(Playback::Playing),
        ]);
        assert!(h.exists("App::idle-view"), "the phone's track is not shown");
        assert!(h.covers_requested.borrow().is_empty());
        for key in [Key::Return, Key::UpArrow, Key::LeftArrow, Key::Return] {
            h.press(key);
        }
        assert!(h.sent.borrow().is_empty(), "sent {:?}", h.sent.borrow());
    }

    #[test]
    fn unbound_keys_are_not_consumed() {
        let h = harness();
        assert!(!h.press(Key::F5));
        assert!(!h.controller.borrow_mut().key("q"));
    }

    fn open_visualizer(h: &Harness) {
        for _ in 0..3 {
            h.press(Key::RightArrow);
        }
        h.press(Key::Return);
    }

    #[test]
    fn visualizer_without_gl_explains_and_back_works() {
        let h = harness();
        h.playing(track("A"));
        open_visualizer(&h);
        assert!(h.exists("App::viz-view"));
        assert!(!h.exists("App::now-playing"));
        assert!(!h.app.get_viz_rendering());
        assert_eq!(
            h.label("App::viz-message-text"),
            "The visualizer needs OpenGL"
        );
        assert!(
            h.label("App::viz-detail-text")
                .contains("software renderer")
        );
        assert!(
            h.exists("App::viz-message-box"),
            "shown at once: it is an error"
        );

        // Preset keys are harmless without a renderer; Back returns.
        for key in [Key::LeftArrow, Key::RightArrow, Key::UpArrow, Key::Return] {
            assert!(h.press(key));
        }
        h.press(Key::Escape);
        assert!(h.exists("App::now-playing"));
        assert!(h.ring_inside("App::viz-button"));
        assert!(
            h.sent.borrow().is_empty(),
            "no player commands from the visualizer"
        );
    }

    #[test]
    fn visualizer_shows_the_track_overlay() {
        let h = harness();
        h.playing(track("A"));
        open_visualizer(&h);
        assert!(h.app.get_viz_overlay_visible(), "shown on open");
        assert_eq!(h.label("App::viz-overlay-title"), "A");
        assert_eq!(h.label("App::viz-overlay-artist"), "Glass Animals");

        h.apply([PlayerEvent::TrackChanged(Some(track("B")))]);
        assert_eq!(h.label("App::viz-overlay-title"), "B");
        assert!(h.app.get_viz_overlay_visible());
    }

    #[test]
    fn visualizer_frames_paced_only_while_open() {
        let h = harness();
        h.playing(track("A"));
        assert!(!h.controller.borrow().frames.running());
        open_visualizer(&h);
        // No renderer can exist here, so it polls slowly instead of drawing.
        assert!(h.controller.borrow().frames.running());
        assert_eq!(h.controller.borrow().frame_interval, Some(VIZ_POLL));
        h.press(Key::Escape);
        assert!(!h.controller.borrow().frames.running());
        assert_eq!(h.controller.borrow().frame_interval, None);
    }

    #[test]
    fn progress_timer_stopped_when_paused() {
        let h = harness();
        assert!(!h.timer_running(), "idle");
        h.playing(track("A"));
        assert!(h.timer_running());
        h.apply([PlayerEvent::PlaybackChanged(Playback::Paused)]);
        assert!(!h.timer_running());
        h.apply([PlayerEvent::PlaybackChanged(Playback::Buffering)]);
        assert!(!h.timer_running());
        h.apply([PlayerEvent::PlaybackChanged(Playback::Playing)]);
        assert!(h.timer_running());
        h.apply([PlayerEvent::Disconnected]);
        assert!(!h.timer_running(), "disconnected");
    }

    #[test]
    fn progress_timer_stopped_in_visualizer() {
        let h = harness();
        h.playing(track("A"));
        for _ in 0..3 {
            h.press(Key::RightArrow);
        }
        h.press(Key::Return);
        assert!(!h.timer_running());
        h.press(Key::Escape);
        assert!(h.timer_running());
    }

    #[test]
    fn cover_is_requested_once_and_shown_when_loaded() {
        let h = harness();
        h.playing(track("A"));
        assert_eq!(*h.covers_requested.borrow(), ["https://i.scdn.co/image/a"]);
        assert!(!h.app.get_has_cover());

        // Same cover on the next track (same album): no new request, image kept.
        let image = Arc::new(DecodedImage {
            width: 2,
            height: 1,
            rgba: vec![255; 8],
        });
        h.controller
            .borrow_mut()
            .cover_loaded("https://i.scdn.co/image/a", Some(image));
        assert!(h.app.get_has_cover());
        h.apply([PlayerEvent::TrackChanged(Some(track("B")))]);
        assert_eq!(h.covers_requested.borrow().len(), 1);
        assert!(h.app.get_has_cover());
    }

    #[test]
    fn stale_or_failed_cover_is_ignored() {
        let h = harness();
        h.playing(track("A"));
        h.apply([PlayerEvent::TrackChanged(Some(Track {
            cover_url: Some("https://i.scdn.co/image/b".to_owned()),
            ..track("B")
        }))]);
        let image = Arc::new(DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        });
        h.controller
            .borrow_mut()
            .cover_loaded("https://i.scdn.co/image/a", Some(image));
        assert!(!h.app.get_has_cover(), "cover of the previous track");
        h.controller
            .borrow_mut()
            .cover_loaded("https://i.scdn.co/image/b", None);
        assert!(!h.app.get_has_cover(), "failed load keeps the placeholder");
    }

    #[test]
    fn going_idle_clears_the_cover() {
        let h = harness();
        h.playing(track("A"));
        let image = Arc::new(DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        });
        h.controller
            .borrow_mut()
            .cover_loaded("https://i.scdn.co/image/a", Some(image));
        h.apply([PlayerEvent::DeviceActive(false)]);
        assert!(!h.app.get_has_cover());
        h.playing(track("A"));
        assert_eq!(h.covers_requested.borrow().len(), 2, "requested again");
    }

    /// Draws real pixels with the software renderer (the no-GPU path), so text shaping and
    /// fonts are exercised, not only properties.
    #[test]
    fn software_renderer_draws_emoji_title() {
        use slint::platform::software_renderer::{
            MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
        };
        struct SoftwarePlatform(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for SoftwarePlatform {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }

        let (width, height) = (960usize, 540usize);
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(SoftwarePlatform(window.clone()))).unwrap();
        let app = App::new().unwrap();
        app.show().unwrap();
        window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
        let controller = Controller::new(
            app.as_weak(),
            Config::default(),
            Box::new(|_| {}),
            Box::new(|_, _| {}),
        );

        let lit_pixels = |title: &str| {
            let mut c = controller.borrow_mut();
            for ev in [
                PlayerEvent::Connected,
                PlayerEvent::DeviceActive(true),
                PlayerEvent::TrackChanged(Some(track(title))),
                PlayerEvent::PlaybackChanged(Playback::Paused),
            ] {
                c.apply(ev);
            }
            drop(c);
            let mut buffer = vec![PremultipliedRgbaColor::default(); width * height];
            window.request_redraw();
            assert!(window.draw_if_needed(|r| {
                r.render(&mut buffer, width);
            }));
            buffer
                .iter()
                .filter(|p| p.red > 200 && p.green > 200)
                .count()
        };
        let without_title = lit_pixels("");
        let with_title = lit_pixels("Jóga — Björk 🎵✨ 日本語");
        assert!(
            with_title > without_title + 200,
            "title drew {} bright pixels",
            with_title.saturating_sub(without_title)
        );
    }

    #[test]
    fn key_names_match_config_names() {
        let name = |key: Key| key_name(&slint::SharedString::from(key));
        assert_eq!(name(Key::LeftArrow).as_deref(), Some("Left"));
        assert_eq!(name(Key::RightArrow).as_deref(), Some("Right"));
        assert_eq!(name(Key::UpArrow).as_deref(), Some("Up"));
        assert_eq!(name(Key::DownArrow).as_deref(), Some("Down"));
        assert_eq!(name(Key::Return).as_deref(), Some("Return"));
        assert_eq!(name(Key::Escape).as_deref(), Some("Escape"));
        assert_eq!(name(Key::Backspace).as_deref(), Some("BackSpace"));
        assert_eq!(name(Key::Space).as_deref(), Some("space"));
        assert_eq!(name(Key::PageDown).as_deref(), Some("Page_Down"));
        assert_eq!(name(Key::F1).as_deref(), Some("F1"));
        assert_eq!(name(Key::F24).as_deref(), Some("F24"));
        assert_eq!(key_name("a").as_deref(), Some("a"));
        assert_eq!(key_name("A").as_deref(), Some("A"));
        assert_eq!(name(Key::Shift), None);
        assert_eq!(name(Key::Back), None, "not delivered by the winit backend");
        assert_eq!(key_name(""), None);
        assert_eq!(key_name("ab"), None);
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(999), "0:00");
        assert_eq!(format_time(61_000), "1:01");
        assert_eq!(format_time(264_214), "4:24");
        assert_eq!(format_time(3_599_999), "59:59");
        assert_eq!(format_time(3_600_000), "1:00:00");
        assert_eq!(format_time(3_725_000), "1:02:05");
    }
}
