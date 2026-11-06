//! The few OpenGL (ES) 3 calls needed to render projectM at a lower resolution and scale it up.

use std::ffi::{CStr, c_void};

pub type Enum = u32;

const FRAMEBUFFER: Enum = 0x8D40;
const READ_FRAMEBUFFER: Enum = 0x8CA8;
const DRAW_FRAMEBUFFER: Enum = 0x8CA9;
const DRAW_FRAMEBUFFER_BINDING: Enum = 0x8CA6;
const READ_FRAMEBUFFER_BINDING: Enum = 0x8CAA;
const RENDERBUFFER: Enum = 0x8D41;
const RGBA8: Enum = 0x8058;
const COLOR_ATTACHMENT0: Enum = 0x8CE0;
const FRAMEBUFFER_COMPLETE: Enum = 0x8CD5;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const LINEAR: Enum = 0x2601;
const NEAREST: Enum = 0x2600;
/// The window's own framebuffer. projectM 4.1 always draws into it, whatever is bound.
const WINDOW_FRAMEBUFFER: u32 = 0;
const VIEWPORT: Enum = 0x0BA2;

macro_rules! gl_fns {
    ($($field:ident = $name:literal: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        pub struct Gl {
            $($field: extern "system" fn($($arg),*) $(-> $ret)?,)*
        }

        impl Gl {
            /// Looks up every function; fails naming the first one missing.
            pub fn load(get_proc_address: &dyn Fn(&CStr) -> *const c_void) -> Result<Gl, String> {
                Ok(Gl {
                    $($field: {
                        let p = get_proc_address($name);
                        if p.is_null() {
                            return Err(format!("{} is missing", $name.to_string_lossy()));
                        }
                        // SAFETY: a non-null pointer for this name has this GL signature.
                        unsafe { std::mem::transmute::<*const c_void, extern "system" fn($($arg),*) $(-> $ret)?>(p) }
                    },)*
                })
            }
        }
    };
}

gl_fns! {
    get_integerv = c"glGetIntegerv": fn(Enum, *mut i32);
    viewport = c"glViewport": fn(i32, i32, i32, i32);
    gen_framebuffers = c"glGenFramebuffers": fn(i32, *mut u32);
    delete_framebuffers = c"glDeleteFramebuffers": fn(i32, *const u32);
    bind_framebuffer = c"glBindFramebuffer": fn(Enum, u32);
    check_framebuffer_status = c"glCheckFramebufferStatus": fn(Enum) -> Enum;
    gen_renderbuffers = c"glGenRenderbuffers": fn(i32, *mut u32);
    delete_renderbuffers = c"glDeleteRenderbuffers": fn(i32, *const u32);
    bind_renderbuffer = c"glBindRenderbuffer": fn(Enum, u32);
    renderbuffer_storage = c"glRenderbufferStorage": fn(Enum, Enum, i32, i32);
    framebuffer_renderbuffer = c"glFramebufferRenderbuffer": fn(Enum, Enum, Enum, u32);
    blit_framebuffer = c"glBlitFramebuffer": fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, Enum);
}

/// Framebuffer bindings and viewport at the start of a frame, put back at its end so the UI
/// toolkit finds its own state.
pub struct Saved {
    draw: u32,
    read: u32,
    viewport: [i32; 4],
}

impl Gl {
    fn integer(&self, name: Enum) -> i32 {
        let mut v = 0;
        (self.get_integerv)(name, &mut v);
        v
    }

    pub fn save(&self) -> Saved {
        let mut viewport = [0; 4];
        (self.get_integerv)(VIEWPORT, viewport.as_mut_ptr());
        Saved {
            draw: self.integer(DRAW_FRAMEBUFFER_BINDING) as u32,
            read: self.integer(READ_FRAMEBUFFER_BINDING) as u32,
            viewport,
        }
    }

    pub fn restore(&self, saved: &Saved) {
        (self.bind_framebuffer)(DRAW_FRAMEBUFFER, saved.draw);
        (self.bind_framebuffer)(READ_FRAMEBUFFER, saved.read);
        let [x, y, w, h] = saved.viewport;
        (self.viewport)(x, y, w, h);
    }

    /// Stretches the `via.width`×`via.height` corner projectM just drew into framebuffer 0
    /// over the whole `width`×`height` window, through `via` (a framebuffer cannot be blitted
    /// onto itself).
    pub fn upscale(&self, via: &Offscreen, width: u32, height: u32) {
        let (w, h) = (via.width as i32, via.height as i32);
        (self.bind_framebuffer)(READ_FRAMEBUFFER, WINDOW_FRAMEBUFFER);
        (self.bind_framebuffer)(DRAW_FRAMEBUFFER, via.fbo);
        (self.blit_framebuffer)(0, 0, w, h, 0, 0, w, h, COLOR_BUFFER_BIT, NEAREST);
        (self.bind_framebuffer)(READ_FRAMEBUFFER, via.fbo);
        (self.bind_framebuffer)(DRAW_FRAMEBUFFER, WINDOW_FRAMEBUFFER);
        let (to_w, to_h) = (width as i32, height as i32);
        (self.blit_framebuffer)(0, 0, w, h, 0, 0, to_w, to_h, COLOR_BUFFER_BIT, LINEAR);
    }
}

/// A colour-only framebuffer holding the reduced-size frame between the two blits.
pub struct Offscreen {
    fbo: u32,
    rbo: u32,
    pub width: u32,
    pub height: u32,
}

impl Offscreen {
    pub fn new(gl: &Gl, width: u32, height: u32) -> Result<Offscreen, String> {
        let (mut fbo, mut rbo) = (0, 0);
        (gl.gen_framebuffers)(1, &mut fbo);
        (gl.gen_renderbuffers)(1, &mut rbo);
        let target = Offscreen {
            fbo,
            rbo,
            width,
            height,
        };
        (gl.bind_renderbuffer)(RENDERBUFFER, rbo);
        (gl.renderbuffer_storage)(RENDERBUFFER, RGBA8, width as i32, height as i32);
        (gl.bind_renderbuffer)(RENDERBUFFER, 0);
        (gl.bind_framebuffer)(FRAMEBUFFER, fbo);
        (gl.framebuffer_renderbuffer)(FRAMEBUFFER, COLOR_ATTACHMENT0, RENDERBUFFER, rbo);
        let status = (gl.check_framebuffer_status)(FRAMEBUFFER);
        if status != FRAMEBUFFER_COMPLETE {
            target.delete(gl);
            return Err(format!("offscreen framebuffer incomplete (0x{status:x})"));
        }
        Ok(target)
    }

    pub fn delete(self, gl: &Gl) {
        (gl.delete_framebuffers)(1, &self.fbo);
        (gl.delete_renderbuffers)(1, &self.rbo);
    }
}
