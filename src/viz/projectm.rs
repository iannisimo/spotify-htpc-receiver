//! libprojectM 4's C API, loaded at runtime so the app still starts where it is not installed.
//! Signatures follow the 4.1 headers. Every call needs the GL context current.

use std::ffi::{c_char, c_double, c_float, c_int, c_uint, c_void};

#[repr(C)]
pub struct Instance {
    _private: [u8; 0],
}

pub type Handle = *mut Instance;
pub type SwitchRequested = unsafe extern "C" fn(is_hard_cut: bool, user_data: *mut c_void);
pub type SwitchFailed =
    unsafe extern "C" fn(file: *const c_char, message: *const c_char, user_data: *mut c_void);

/// `projectm_channels`
pub const STEREO: c_int = 2;

/// Library names tried in order: the versioned soname, then the development symlink.
const LIBRARY_NAMES: [&str; 2] = ["libprojectM-4.so.4", "libprojectM-4.so"];

macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(clippy::struct_field_names)]
        pub struct Api {
            $(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            // Declared last so it is dropped after the pointers are no longer usable.
            _lib: libloading::Library,
        }

        impl Api {
            pub fn load() -> Result<Api, String> {
                let lib = open()?;
                // SAFETY: each symbol is looked up by its C name and given its C signature.
                unsafe {
                    Ok(Api {
                        $($name: *lib
                            .get(concat!(stringify!($name), "\0").as_bytes())
                            .map_err(|e| format!("libprojectM-4 lacks {}: {e}", stringify!($name)))?,)*
                        _lib: lib,
                    })
                }
            }
        }
    };
}

api! {
    projectm_create: fn() -> Handle;
    projectm_destroy: fn(Handle);
    projectm_load_preset_file: fn(Handle, *const c_char, bool);
    projectm_set_texture_search_paths: fn(Handle, *const *const c_char, usize);
    projectm_set_window_size: fn(Handle, usize, usize);
    projectm_set_mesh_size: fn(Handle, usize, usize);
    projectm_set_fps: fn(Handle, i32);
    projectm_set_preset_duration: fn(Handle, c_double);
    projectm_set_soft_cut_duration: fn(Handle, c_double);
    projectm_set_hard_cut_enabled: fn(Handle, bool);
    projectm_set_beat_sensitivity: fn(Handle, c_float);
    projectm_set_preset_locked: fn(Handle, bool);
    projectm_set_preset_switch_requested_event_callback: fn(Handle, Option<SwitchRequested>, *mut c_void);
    projectm_set_preset_switch_failed_event_callback: fn(Handle, Option<SwitchFailed>, *mut c_void);
    projectm_pcm_add_float: fn(Handle, *const c_float, c_uint, c_int);
    projectm_opengl_render_frame: fn(Handle);
}

fn open() -> Result<libloading::Library, String> {
    let mut errors = Vec::new();
    for name in LIBRARY_NAMES {
        // SAFETY: loading libprojectM runs only its own initialisers.
        match unsafe { libloading::Library::new(name) } {
            Ok(lib) => return Ok(lib),
            Err(e) => errors.push(e.to_string()),
        }
    }
    Err(format!(
        "libprojectM-4 is not installed ({})",
        errors.join("; ")
    ))
}
