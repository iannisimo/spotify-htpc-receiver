//! The projectM visualizer, rendered into the UI's own OpenGL context.
//!
//! Everything here must run on the thread whose GL context is current (Slint's rendering
//! notifier), from [`Visualizer::new`] to the drop.

pub mod adaptive;
mod gl;
pub mod presets;
mod projectm;

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::VisualizerConfig;
use adaptive::Quality;
use presets::Playlist;

/// Beat sensitivity range accepted by projectM.
pub const BEAT_SENSITIVITY_MAX: f32 = 5.0;

/// Proof that a GL context is current, with the functions looked up from it.
pub struct GlContext {
    gl: Result<gl::Gl, String>,
}

impl GlContext {
    /// Looks up the GL functions the visualizer needs through `get_proc_address` (as Slint's
    /// `GraphicsAPI::NativeOpenGL` provides).
    ///
    /// # Safety
    /// An OpenGL or OpenGL ES 3 context must be current on this thread, and must stay current
    /// whenever the [`Visualizer`] made from this is used or dropped.
    pub unsafe fn current(get_proc_address: &dyn Fn(&CStr) -> *const c_void) -> GlContext {
        GlContext {
            gl: gl::Gl::load(get_proc_address),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VizError {
    /// No `.milk` presets in any of these directories.
    NoPresets(Vec<PathBuf>),
    /// The GL context cannot run projectM.
    GlUnavailable(String),
    /// projectM could not be loaded or started.
    Init(String),
}

impl fmt::Display for VizError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VizError::NoPresets(dirs) => {
                let dirs: Vec<_> = dirs.iter().map(|d| d.display().to_string()).collect();
                write!(
                    f,
                    "no projectM presets (.milk files) in {}",
                    dirs.join(", ")
                )
            }
            VizError::GlUnavailable(msg) => write!(f, "OpenGL cannot run the visualizer: {msg}"),
            VizError::Init(msg) => write!(f, "projectM could not start: {msg}"),
        }
    }
}

impl std::error::Error for VizError {}

/// Set by projectM's callbacks during a call into it, read right after.
#[derive(Default)]
struct Events {
    /// projectM wants the next preset (its timer ran out, or a hard cut on a beat).
    switch_requested: Cell<Option<bool>>,
    /// The preset just loaded failed.
    load_failed: Cell<bool>,
}

unsafe extern "C" fn on_switch_requested(is_hard_cut: bool, user_data: *mut c_void) {
    // SAFETY: user_data is the Visualizer's boxed Events, alive while projectM is.
    let events = unsafe { &*(user_data as *const Events) };
    events.switch_requested.set(Some(is_hard_cut));
}

unsafe extern "C" fn on_switch_failed(
    file: *const c_char,
    message: *const c_char,
    user_data: *mut c_void,
) {
    // SAFETY: as above; projectM passes valid C strings or null.
    let events = unsafe { &*(user_data as *const Events) };
    let text = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    log::warn!("preset {} failed: {}", text(file), text(message));
    events.load_failed.set(true);
}

pub struct Visualizer {
    api: projectm::Api,
    handle: projectm::Handle,
    gl: gl::Gl,
    /// Boxed so the address handed to projectM's callbacks never moves.
    events: Box<Events>,
    playlist: Playlist,
    /// Holds the reduced-size frame while the scale is below 1.
    offscreen: Option<gl::Offscreen>,
    /// Set when the GL cannot render offscreen; then the scale is ignored.
    scaling_broken: bool,
    /// Mesh and FPS last handed to projectM.
    applied: Option<(u32, u32, u32)>,
    window: (u32, u32),
    locked: bool,
    beat_sensitivity: f32,
}

impl Visualizer {
    /// Finds presets, loads libprojectM and starts it on the current GL context.
    pub fn new(gl: GlContext, cfg: &VisualizerConfig) -> Result<Visualizer, VizError> {
        let dirs = match &cfg.preset_dir {
            Some(dir) => vec![dir.clone()],
            None => presets::default_dirs(
                std::env::var_os("XDG_DATA_HOME"),
                std::env::var_os("XDG_DATA_DIRS"),
                std::env::home_dir(),
            ),
        };
        let (dir, found) = dirs
            .iter()
            .find_map(|d| {
                let found = presets::find(d);
                (!found.is_empty()).then(|| (d.clone(), found))
            })
            .ok_or_else(|| VizError::NoPresets(dirs.clone()))?;
        let playlist = Playlist::new(found, cfg.shuffle, seed())
            .ok_or_else(|| VizError::NoPresets(vec![dir.clone()]))?;
        let gl = gl.gl.map_err(VizError::GlUnavailable)?;
        let api = projectm::Api::load().map_err(VizError::Init)?;

        // SAFETY (all projectM calls): the context is current (GlContext's contract) and
        // `handle` is checked non-null and lives until drop.
        let handle = unsafe { (api.projectm_create)() };
        if handle.is_null() {
            return Err(VizError::GlUnavailable(
                "projectM could not initialise on this OpenGL context".to_owned(),
            ));
        }
        let events = Box::<Events>::default();
        let user_data = &*events as *const Events as *mut c_void;
        let beat_sensitivity = cfg.beat_sensitivity.clamp(0.0, BEAT_SENSITIVITY_MAX);
        let texture_dir = CString::new(dir.as_os_str().as_bytes()).ok();
        unsafe {
            (api.projectm_set_preset_duration)(handle, cfg.preset_duration_s);
            (api.projectm_set_soft_cut_duration)(handle, cfg.soft_cut_s);
            (api.projectm_set_hard_cut_enabled)(handle, cfg.hard_cuts);
            (api.projectm_set_beat_sensitivity)(handle, beat_sensitivity);
            if let Some(texture_dir) = &texture_dir {
                // Presets name their textures relative to the preset collection.
                let paths = [texture_dir.as_ptr()];
                (api.projectm_set_texture_search_paths)(handle, paths.as_ptr(), paths.len());
            }
            (api.projectm_set_preset_switch_requested_event_callback)(
                handle,
                Some(on_switch_requested),
                user_data,
            );
            (api.projectm_set_preset_switch_failed_event_callback)(
                handle,
                Some(on_switch_failed),
                user_data,
            );
        }
        log::info!(
            "projectM: {} presets from {}",
            playlist.count(),
            dir.display()
        );
        let mut viz = Visualizer {
            api,
            handle,
            gl,
            events,
            playlist,
            offscreen: None,
            scaling_broken: false,
            applied: None,
            window: (0, 0),
            locked: false,
            beat_sensitivity,
        };
        viz.load_current(false);
        Ok(viz)
    }

    /// Queues interleaved stereo f32 samples for the next frames.
    pub fn feed_pcm(&mut self, interleaved: &[f32]) {
        let frames = interleaved.len() / 2;
        if frames > 0 {
            let frames = c_uint::try_from(frames).unwrap_or(c_uint::MAX);
            unsafe {
                (self.api.projectm_pcm_add_float)(
                    self.handle,
                    interleaved.as_ptr(),
                    frames,
                    projectm::STEREO,
                );
            }
        }
    }

    /// Draws one frame, `width`×`height` pixels, at `quality`. projectM 4.1 always draws into
    /// the window's framebuffer (0), whatever is bound; Slint renders its window there too.
    /// Leaves the framebuffer bindings and viewport as it found them.
    pub fn render(&mut self, width: u32, height: u32, quality: Quality) {
        if width == 0 || height == 0 {
            return;
        }
        let settings = (quality.mesh_w, quality.mesh_h, quality.fps_cap);
        if self.applied != Some(settings) {
            self.applied = Some(settings);
            unsafe {
                (self.api.projectm_set_mesh_size)(
                    self.handle,
                    quality.mesh_w.max(1) as usize,
                    quality.mesh_h.max(1) as usize,
                );
                (self.api.projectm_set_fps)(
                    self.handle,
                    i32::try_from(quality.fps_cap).unwrap_or(i32::MAX),
                );
            }
        }
        let scale = quality.scale.clamp(0.1, 1.0);
        let scaled = (
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
        );
        let saved = self.gl.save();
        if self.prepare_offscreen(scaled, (width, height)) {
            // projectM draws a smaller frame into the window's corner; stretch it over the rest.
            self.set_window(scaled);
            unsafe { (self.api.projectm_opengl_render_frame)(self.handle) };
            if let Some(via) = &self.offscreen {
                self.gl.upscale(via, width, height);
            }
        } else {
            self.set_window((width, height));
            unsafe { (self.api.projectm_opengl_render_frame)(self.handle) };
        }
        self.gl.restore(&saved);
        self.after_call();
        if let Some(hard_cut) = self.events.switch_requested.take() {
            self.playlist.forward();
            self.load_current(!hard_cut);
        }
    }

    /// Makes `self.offscreen` `scaled` in size. `false` means render directly at full size:
    /// no scaling wanted, or the GL cannot make the framebuffer.
    fn prepare_offscreen(&mut self, scaled: (u32, u32), full: (u32, u32)) -> bool {
        if scaled == full || self.scaling_broken {
            if let Some(old) = self.offscreen.take() {
                old.delete(&self.gl);
            }
            return false;
        }
        if self
            .offscreen
            .as_ref()
            .is_some_and(|o| (o.width, o.height) != scaled)
            && let Some(old) = self.offscreen.take()
        {
            old.delete(&self.gl);
        }
        if self.offscreen.is_none() {
            match gl::Offscreen::new(&self.gl, scaled.0, scaled.1) {
                Ok(target) => self.offscreen = Some(target),
                Err(e) => {
                    log::warn!("visualizer renders at full size: {e}");
                    self.scaling_broken = true;
                    return false;
                }
            }
        }
        true
    }

    fn set_window(&mut self, size: (u32, u32)) {
        if self.window != size {
            self.window = size;
            unsafe {
                (self.api.projectm_set_window_size)(self.handle, size.0 as usize, size.1 as usize)
            };
        }
    }

    pub fn next_preset(&mut self) {
        self.playlist.forward();
        self.load_current(false);
    }

    pub fn prev_preset(&mut self) {
        self.playlist.back();
        self.load_current(false);
    }

    /// Locks or unlocks the current preset (no automatic switching while locked). Returns
    /// whether it is now locked. Next/previous still work.
    pub fn toggle_lock(&mut self) -> bool {
        self.locked = !self.locked;
        unsafe { (self.api.projectm_set_preset_locked)(self.handle, self.locked) };
        self.locked
    }

    /// Changes beat sensitivity by `delta`, clamped to 0–5. Returns the new value.
    pub fn adjust_beat_sensitivity(&mut self, delta: f32) -> f32 {
        self.beat_sensitivity = (self.beat_sensitivity + delta).clamp(0.0, BEAT_SENSITIVITY_MAX);
        unsafe { (self.api.projectm_set_beat_sensitivity)(self.handle, self.beat_sensitivity) };
        self.beat_sensitivity
    }

    pub fn current_preset_name(&self) -> String {
        self.playlist.current_name()
    }

    /// Loads the playlist's current preset, skipping (and forgetting) any that fail.
    fn load_current(&mut self, smooth: bool) {
        loop {
            let path = self.playlist.current();
            match CString::new(path.as_os_str().as_bytes()) {
                Ok(file) => unsafe {
                    (self.api.projectm_load_preset_file)(self.handle, file.as_ptr(), smooth)
                },
                Err(_) => self.events.load_failed.set(true), // NUL in a path: cannot load
            }
            if !self.events.load_failed.take() {
                return;
            }
            if self.playlist.remove_current().is_none() {
                log::warn!("no projectM preset could be loaded");
                return;
            }
        }
    }

    /// Clears a failure reported during a render (a preset that fails only once drawn).
    fn after_call(&mut self) {
        if self.events.load_failed.take() && self.playlist.remove_current().is_some() {
            self.load_current(false);
        }
    }
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        if let Some(target) = self.offscreen.take() {
            target.delete(&self.gl);
        }
        unsafe { (self.api.projectm_destroy)(self.handle) };
    }
}

/// Seed for the shuffle order.
fn seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
}
