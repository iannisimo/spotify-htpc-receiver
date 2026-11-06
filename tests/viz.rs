//! The projectM wrapper. The rendering test needs libprojectM-4 and a GPU (or Mesa's llvmpipe);
//! run it with `cargo test --test viz -- --ignored`.

use std::ffi::{CStr, c_void};
use std::path::{Path, PathBuf};

use htpc_nowplaying::config::{Config, VisualizerConfig};
use htpc_nowplaying::viz::adaptive::Quality;
use htpc_nowplaying::viz::disabled::DisabledPresets;
use htpc_nowplaying::viz::{GlContext, Visualizer, VizError};

/// Draws an orange border and a white waveform, so a rendered frame is never all black.
const TEST_PRESET: &str = "[preset00]
fDecay=0.9
zoom=1.0
ob_size=0.05
ob_r=1.0
ob_g=0.5
ob_b=0.0
ob_a=1.0
wave_r=1.0
wave_g=1.0
wave_b=1.0
fWaveAlpha=1.0
nWaveMode=0
fWaveScale=1.0
";

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("htpc-viz-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(preset_dir: &Path) -> VisualizerConfig {
    let mut cfg = Config::default().visualizer;
    cfg.preset_dir = Some(preset_dir.to_owned());
    cfg
}

/// A context whose GL functions are all missing. Never reaches GL.
fn no_gl() -> GlContext {
    // SAFETY: nothing is ever called through it.
    unsafe { GlContext::current(&|_| std::ptr::null()) }
}

#[test]
fn empty_preset_dir_returns_no_presets_error() {
    let dir = temp_dir("empty");
    std::fs::write(dir.join("readme.txt"), "not a preset").unwrap();
    let err = Visualizer::new(no_gl(), &config(&dir), &DisabledPresets::default())
        .err()
        .unwrap();
    assert_eq!(err, VizError::NoPresets(vec![dir.clone()]));
    assert!(err.to_string().contains(&dir.display().to_string()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn missing_gl_functions_return_gl_unavailable() {
    let dir = temp_dir("nogl");
    std::fs::write(dir.join("test.milk"), TEST_PRESET).unwrap();
    let err = Visualizer::new(no_gl(), &config(&dir), &DisabledPresets::default())
        .err()
        .unwrap();
    assert!(
        matches!(&err, VizError::GlUnavailable(m) if m.contains("glGetIntegerv")),
        "{err}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn all_presets_disabled_is_reported_before_touching_gl() {
    let dir = temp_dir("alldisabled");
    std::fs::write(dir.join("test.milk"), TEST_PRESET).unwrap();
    let disabled = DisabledPresets::from_lines("test.milk\n");
    let err = Visualizer::new(no_gl(), &config(&dir), &disabled)
        .err()
        .unwrap();
    assert_eq!(err, VizError::AllDisabled);
    std::fs::remove_dir_all(dir).unwrap();
}

// ------------------------------------------------------------- offscreen GL for the render test

const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_ES3_BIT: i32 = 0x40;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_NONE: i32 = 0x3038;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

type Ptr = *mut c_void;

/// An OpenGL ES 3 context drawing into a `size`² pbuffer, current on this thread. The pbuffer
/// is framebuffer 0, standing in for the UI's window.
struct Egl {
    lib: libloading::Library,
    display: Ptr,
    context: Ptr,
    surface: Ptr,
}

impl Egl {
    fn sym<T: Copy>(&self, name: &[u8]) -> T {
        unsafe { *self.lib.get::<T>(name).expect("EGL symbol") }
    }

    fn new(size: i32) -> Egl {
        let lib = unsafe { libloading::Library::new("libEGL.so.1") }.expect("libEGL.so.1");
        let none = std::ptr::null_mut();
        let mut egl = Egl {
            lib,
            display: none,
            context: none,
            surface: none,
        };
        let get_display: extern "C" fn(u32, Ptr, *const isize) -> Ptr =
            egl.sym(b"eglGetPlatformDisplay\0");
        let initialize: extern "C" fn(Ptr, *mut i32, *mut i32) -> u32 = egl.sym(b"eglInitialize\0");
        let bind_api: extern "C" fn(u32) -> u32 = egl.sym(b"eglBindAPI\0");
        let choose_config: extern "C" fn(Ptr, *const i32, *mut Ptr, i32, *mut i32) -> u32 =
            egl.sym(b"eglChooseConfig\0");
        let create_context: extern "C" fn(Ptr, Ptr, Ptr, *const i32) -> Ptr =
            egl.sym(b"eglCreateContext\0");
        let create_pbuffer: extern "C" fn(Ptr, Ptr, *const i32) -> Ptr =
            egl.sym(b"eglCreatePbufferSurface\0");
        let make_current: extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32 = egl.sym(b"eglMakeCurrent\0");

        egl.display = get_display(
            EGL_PLATFORM_SURFACELESS_MESA,
            none,
            [EGL_NONE as isize].as_ptr(),
        );
        assert!(!egl.display.is_null(), "no surfaceless EGL display");
        assert_eq!(initialize(egl.display, none.cast(), none.cast()), 1);
        assert_eq!(bind_api(EGL_OPENGL_ES_API), 1);
        let attribs = [
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES3_BIT,
            EGL_SURFACE_TYPE,
            EGL_PBUFFER_BIT,
            EGL_NONE,
        ];
        let (mut config, mut n) = (none, 0);
        let chosen = choose_config(egl.display, attribs.as_ptr(), &mut config, 1, &mut n);
        assert!(chosen == 1 && n == 1, "no GLES 3 pbuffer config");
        let ctx_attribs = [EGL_CONTEXT_MAJOR_VERSION, 3, EGL_NONE];
        egl.context = create_context(egl.display, config, none, ctx_attribs.as_ptr());
        assert!(!egl.context.is_null(), "no GLES 3 context");
        let pb_attribs = [EGL_WIDTH, size, EGL_HEIGHT, size, EGL_NONE];
        egl.surface = create_pbuffer(egl.display, config, pb_attribs.as_ptr());
        assert!(!egl.surface.is_null(), "no pbuffer");
        assert_eq!(
            make_current(egl.display, egl.surface, egl.surface, egl.context),
            1
        );
        egl
    }

    fn proc_address(&self, name: &CStr) -> *const c_void {
        let get: extern "C" fn(*const std::ffi::c_char) -> *const c_void =
            self.sym(b"eglGetProcAddress\0");
        get(name.as_ptr())
    }
}

impl Drop for Egl {
    fn drop(&mut self) {
        let make_current: extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32 = self.sym(b"eglMakeCurrent\0");
        let destroy_surface: extern "C" fn(Ptr, Ptr) -> u32 = self.sym(b"eglDestroySurface\0");
        let destroy_context: extern "C" fn(Ptr, Ptr) -> u32 = self.sym(b"eglDestroyContext\0");
        let terminate: extern "C" fn(Ptr) -> u32 = self.sym(b"eglTerminate\0");
        let none = std::ptr::null_mut();
        make_current(self.display, none, none, none);
        destroy_surface(self.display, self.surface);
        destroy_context(self.display, self.context);
        terminate(self.display);
    }
}

/// The GL calls the test itself makes.
struct TestGl<'a> {
    egl: &'a Egl,
}

impl TestGl<'_> {
    fn f<T>(&self, name: &CStr) -> T {
        let p = self.egl.proc_address(name);
        assert!(!p.is_null(), "{name:?}");
        unsafe { std::mem::transmute_copy(&p) }
    }

    fn error(&self) -> u32 {
        let get_error: extern "system" fn() -> u32 = self.f(c"glGetError");
        get_error()
    }

    fn integers<const N: usize>(&self, name: u32) -> [i32; N] {
        let get: extern "system" fn(u32, *mut i32) = self.f(c"glGetIntegerv");
        let mut v = [0; N];
        get(name, v.as_mut_ptr());
        v
    }

    fn clear_to_black(&self) {
        let clear_color: extern "system" fn(f32, f32, f32, f32) = self.f(c"glClearColor");
        let clear: extern "system" fn(u32) = self.f(c"glClear");
        clear_color(0.0, 0.0, 0.0, 1.0);
        clear(GL_COLOR_BUFFER_BIT);
    }

    /// Pixels in the `w`×`h` area at (`x`, `y`) of framebuffer 0 that are not black.
    fn lit_pixels(&self, x: i32, y: i32, w: i32, h: i32) -> usize {
        let read: extern "system" fn(i32, i32, i32, i32, u32, u32, *mut c_void) =
            self.f(c"glReadPixels");
        let mut px = vec![0u8; (w * h * 4) as usize];
        read(
            x,
            y,
            w,
            h,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            px.as_mut_ptr().cast(),
        );
        let (pixels, _) = px.as_chunks::<4>();
        pixels.iter().filter(|p| p[..3] != [0, 0, 0]).count()
    }
}

fn sine(frame: usize) -> Vec<f32> {
    // 1/60 s of a 220 Hz tone at 44.1 kHz, stereo.
    (0..735)
        .flat_map(|i| {
            let t = (frame * 735 + i) as f32 / 44_100.0;
            let v = 0.8 * (t * 220.0 * std::f32::consts::TAU).sin();
            [v, v]
        })
        .collect()
}

#[test]
#[ignore = "needs libprojectM-4 and an EGL/GLES 3 driver"]
fn renders_frames_with_sine_input() {
    const SIZE: i32 = 256;
    const HALF: i32 = SIZE / 2;
    let dir = temp_dir("render");
    std::fs::write(dir.join("test.milk"), TEST_PRESET).unwrap();
    let egl = Egl::new(SIZE);
    let gl = TestGl { egl: &egl };

    // SAFETY: the EGL context is current on this thread for the whole test.
    let ctx = unsafe { GlContext::current(&|name| egl.proc_address(name)) };
    let mut viz =
        Visualizer::new(ctx, &config(&dir), &DisabledPresets::default()).expect("visualizer");
    assert_eq!(viz.current_preset_name(), "test");
    assert_eq!(gl.error(), 0);

    let full = Quality {
        scale: 1.0,
        mesh_w: 32,
        mesh_h: 24,
        fps_cap: 60,
    };
    let half = Quality {
        scale: 0.5,
        mesh_w: 16,
        mesh_h: 12,
        fps_cap: 30,
    };
    let render = |viz: &mut Visualizer, frame: usize, q: Quality| {
        viz.feed_pcm(&sine(frame));
        viz.render(SIZE as u32, SIZE as u32, q);
        assert_eq!(gl.error(), 0, "GL error after frame {frame}");
        assert_eq!(gl.integers::<1>(GL_FRAMEBUFFER_BINDING), [0]);
        assert_eq!(gl.integers::<4>(GL_VIEWPORT), [0, 0, SIZE, SIZE]);
    };

    for frame in 0..30 {
        render(&mut viz, frame, full);
    }
    let lit = gl.lit_pixels(0, 0, SIZE, SIZE);
    assert!(lit > 1000, "only {lit} lit pixels at full scale");

    // At half scale projectM draws into the bottom-left quarter; the upscale must fill the
    // rest, so the top-right quarter lights up again after a clear.
    for frame in 30..60 {
        gl.clear_to_black();
        render(&mut viz, frame, half);
        let top_right = gl.lit_pixels(HALF, HALF, HALF, HALF);
        assert!(
            top_right > 500,
            "frame {frame}: top-right not drawn ({top_right} lit)"
        );
    }

    viz.next_preset(); // the only preset: reloads it
    assert_eq!(viz.current_preset_name(), "test");
    assert!(viz.toggle_lock());
    assert!(!viz.toggle_lock());
    assert_eq!(viz.adjust_beat_sensitivity(10.0), 5.0);
    assert_eq!(viz.adjust_beat_sensitivity(-7.5), 0.0);
    render(&mut viz, 60, full);
    drop(viz);
    assert_eq!(gl.error(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Two presets, `off.milk` and `on.milk`, with the first switched off; sorted, not shuffled.
fn two_presets(name: &str) -> (PathBuf, VisualizerConfig, DisabledPresets) {
    let dir = temp_dir(name);
    std::fs::write(dir.join("off.milk"), TEST_PRESET).unwrap();
    std::fs::write(dir.join("on.milk"), TEST_PRESET).unwrap();
    let mut cfg = config(&dir);
    cfg.shuffle = false;
    (dir, cfg, DisabledPresets::from_lines("off.milk\n"))
}

#[test]
#[ignore = "needs libprojectM-4 and an EGL/GLES 3 driver"]
fn renders_with_disabled_presets() {
    let (dir, cfg, disabled) = two_presets("disabled");
    let egl = Egl::new(64);
    // SAFETY: the EGL context is current on this thread for the whole test.
    let ctx = unsafe { GlContext::current(&|name| egl.proc_address(name)) };
    let mut viz = Visualizer::new(ctx, &cfg, &disabled).expect("visualizer");
    assert_eq!(
        viz.current_preset_name(),
        "on",
        "starts on an enabled preset"
    );
    for _ in 0..3 {
        viz.next_preset();
        assert_eq!(viz.current_preset_name(), "on");
    }
    viz.set_disabled(&DisabledPresets::default());
    viz.next_preset();
    assert_eq!(viz.current_preset_name(), "off", "switched back on");
    drop(viz);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "needs libprojectM-4 and an EGL/GLES 3 driver"]
fn demo_disabled_preset_then_resume() {
    let (dir, cfg, disabled) = two_presets("demo");
    let egl = Egl::new(64);
    // SAFETY: the EGL context is current on this thread for the whole test.
    let ctx = unsafe { GlContext::current(&|name| egl.proc_address(name)) };
    let mut viz = Visualizer::new(ctx, &cfg, &disabled).expect("visualizer");

    viz.demo(&dir.join("off.milk"))
        .expect("demo of a disabled preset");
    assert_eq!(viz.current_preset_name(), "off");
    let quality = Quality {
        scale: 1.0,
        mesh_w: 16,
        mesh_h: 12,
        fps_cap: 60,
    };
    viz.render(64, 64, quality);
    assert_eq!(
        viz.current_preset_name(),
        "off",
        "no automatic switch during a demo"
    );

    viz.end_demo();
    assert_eq!(
        viz.current_preset_name(),
        "on",
        "resumes on the next enabled preset"
    );
    viz.end_demo(); // a second end does nothing
    assert_eq!(viz.current_preset_name(), "on");

    let missing = dir.join("missing.milk");
    assert!(matches!(viz.demo(&missing), Err(VizError::PresetLoad(_))));
    assert_eq!(
        viz.current_preset_name(),
        "on",
        "a failed demo keeps what was showing"
    );
    drop(viz);
    std::fs::remove_dir_all(dir).unwrap();
}
