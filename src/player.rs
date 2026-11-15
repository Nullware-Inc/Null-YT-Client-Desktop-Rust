//! Built-in video player: libmpv, loaded at runtime, drawing into a pixel
//! buffer with mpv's software renderer. Each frame becomes a Slint image, so
//! playback lives inside the app window like any other view.
//!
//! libmpv is loaded with `libloading`, so the app still starts without it
//! (it then falls back to launching an external mpv).
//!  - Windows: `libmpv-2.dll` next to the exe (bundled in releases)
//!  - Linux:   the system's `libmpv.so.2` (package `libmpv2` / `mpv-libs` / `mpv`)
//!  - macOS:   Homebrew's `libmpv.2.dylib`

use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

type Handle = *mut c_void;
type RenderCtx = *mut c_void;

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct Event {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}

const PARAM_INVALID: c_int = 0;
const PARAM_API_TYPE: c_int = 1;
const PARAM_SW_SIZE: c_int = 17;
const PARAM_SW_FORMAT: c_int = 18;
const PARAM_SW_STRIDE: c_int = 19;
const PARAM_SW_POINTER: c_int = 20;
const UPDATE_FRAME: u64 = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_INT64: c_int = 4;
const FORMAT_DOUBLE: c_int = 5;
const EVENT_SHUTDOWN: c_int = 1;
const EVENT_END_FILE: c_int = 7;
const END_FILE_REASON_ERROR: c_int = 4;

/// The libmpv functions we use.
struct Api {
    _lib: libloading::Library,
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    get_property_string: unsafe extern "C" fn(Handle, *const c_char) -> *mut c_char,
    set_property_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    free: unsafe extern "C" fn(*mut c_void),
    set_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, f64) -> *mut Event,
    wakeup: unsafe extern "C" fn(Handle),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    terminate_destroy: unsafe extern "C" fn(Handle),
    render_create: unsafe extern "C" fn(*mut RenderCtx, Handle, *mut RenderParam) -> c_int,
    render_set_update_callback:
        unsafe extern "C" fn(RenderCtx, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void),
    render_update: unsafe extern "C" fn(RenderCtx) -> u64,
    render: unsafe extern "C" fn(RenderCtx, *mut RenderParam) -> c_int,
    render_free: unsafe extern "C" fn(RenderCtx),
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn candidates() -> Vec<std::path::PathBuf> {
    let here = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    let names: &[&str] = if cfg!(windows) {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else if cfg!(target_os = "macos") {
        &[
            "libmpv.2.dylib",
            "/opt/homebrew/lib/libmpv.2.dylib",
            "/usr/local/lib/libmpv.2.dylib",
            "libmpv.dylib",
        ]
    } else {
        &["libmpv.so.2", "libmpv.so.1", "libmpv.so"]
    };
    let mut out = vec![];
    for n in names {
        let local = here.join(n);
        if local.exists() {
            out.push(local);
        }
        out.push(n.into());
    }
    out
}

impl Api {
    fn load() -> Option<Api> {
        for path in candidates() {
            // SAFETY: loading libmpv runs its initialisers, which are benign.
            let Ok(lib) = (unsafe { libloading::Library::new(&path) }) else {
                continue;
            };
            macro_rules! sym {
                ($name:literal) => {
                    *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.ok()?
                };
            }
            return Some(Api {
                create: sym!("mpv_create"),
                initialize: sym!("mpv_initialize"),
                set_option_string: sym!("mpv_set_option_string"),
                command: sym!("mpv_command"),
                get_property: sym!("mpv_get_property"),
                get_property_string: sym!("mpv_get_property_string"),
                set_property_string: sym!("mpv_set_property_string"),
                free: sym!("mpv_free"),
                set_property: sym!("mpv_set_property"),
                wait_event: sym!("mpv_wait_event"),
                wakeup: sym!("mpv_wakeup"),
                error_string: sym!("mpv_error_string"),
                terminate_destroy: sym!("mpv_terminate_destroy"),
                render_create: sym!("mpv_render_context_create"),
                render_set_update_callback: sym!("mpv_render_context_set_update_callback"),
                render_update: sym!("mpv_render_context_update"),
                render: sym!("mpv_render_context_render"),
                render_free: sym!("mpv_render_context_free"),
                _lib: lib,
            });
        }
        None
    }

    fn get() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(Api::load).as_ref()
    }

    fn err(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string.
        unsafe { CStr::from_ptr((self.error_string)(code)) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Is libmpv available on this machine?
pub fn available() -> bool {
    Api::get().is_some()
}

/// Options for a new player.
pub struct Options {
    pub ytdl_path: String,
    pub ytdl_format: String,
    /// `key=value` pairs passed to yt-dlp (cookies...).
    pub ytdl_raw: Vec<String>,
    pub volume: f64,
    /// Largest frame we render; bigger videos are scaled down by mpv.
    pub max_size: (u32, u32),
}

/// Woken by mpv when a new frame is ready.
struct Wake {
    pending: Mutex<bool>,
    cond: Condvar,
}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    // SAFETY: ctx is the Wake we registered; it outlives the render context.
    let wake = unsafe { &*(ctx as *const Wake) };
    *wake.pending.lock().unwrap() = true;
    wake.cond.notify_one();
}

struct Shared {
    api: &'static Api,
    handle: Handle,
    stop: AtomicBool,
    last_error: Mutex<Option<String>>,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// A libmpv instance with its render thread. Dropping it shuts mpv down.
pub struct Player {
    shared: Arc<Shared>,
    wake: Arc<Wake>,
    render_thread: Option<JoinHandle<()>>,
    event_thread: Option<JoinHandle<()>>,
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

impl Player {
    /// Starts mpv. `on_frame` gets every rendered frame, on the render thread.
    pub fn new(
        opts: Options,
        on_frame: impl Fn(SharedPixelBuffer<Rgba8Pixel>) + Send + 'static,
    ) -> Result<Player, String> {
        let api = Api::get().ok_or("libmpv not found")?;
        // SAFETY: plain libmpv calls on a handle we own.
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("mpv_create failed".into());
        }
        let set = |k: &str, v: &str| unsafe {
            (api.set_option_string)(handle, cstr(k).as_ptr(), cstr(v).as_ptr())
        };
        set("vo", "libmpv");
        set("hwdec", "auto-copy-safe");
        set("keep-open", "yes");
        set("idle", "yes");
        set("terminal", "no");
        set("input-default-bindings", "no");
        set("osc", "no");
        set("ytdl", "yes");
        set(
            "script-opts",
            &format!("ytdl_hook-ytdl_path={}", opts.ytdl_path),
        );
        set("ytdl-format", &opts.ytdl_format);
        // Ask yt-dlp for (auto) captions too; mpv adds them as subtitle
        // tracks, hidden until the viewer turns captions on.
        let mut raw = opts.ytdl_raw.clone();
        raw.extend(["write-auto-subs=".into(), "sub-langs=en.*".into()]);
        set("ytdl-raw-options", &raw.join(","));
        set("sid", "no");
        set("sub-font-size", "40");
        set("sub-border-size", "2");
        set("volume", &opts.volume.to_string());
        set("cache", "yes");
        set("demuxer-max-bytes", "150MiB");
        let rc = unsafe { (api.initialize)(handle) };
        if rc < 0 {
            unsafe { (api.terminate_destroy)(handle) };
            return Err(format!("mpv_initialize: {}", api.err(rc)));
        }

        let mut ctx: RenderCtx = std::ptr::null_mut();
        let sw = cstr("sw");
        let mut params = [
            RenderParam {
                kind: PARAM_API_TYPE,
                data: sw.as_ptr() as *mut c_void,
            },
            RenderParam {
                kind: PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let rc = unsafe { (api.render_create)(&mut ctx, handle, params.as_mut_ptr()) };
        if rc < 0 {
            unsafe { (api.terminate_destroy)(handle) };
            return Err(format!("mpv render context: {}", api.err(rc)));
        }

        let wake = Arc::new(Wake {
            pending: Mutex::new(false),
            cond: Condvar::new(),
        });
        unsafe {
            (api.render_set_update_callback)(
                ctx,
                Some(on_update),
                Arc::as_ptr(&wake) as *mut c_void,
            )
        };
        let shared = Arc::new(Shared {
            api,
            handle,
            stop: AtomicBool::new(false),
            last_error: Mutex::new(None),
        });

        // Render thread: waits for mpv to say a frame is due, draws it into
        // a fresh buffer and hands it over.
        let ctx_addr = ctx as usize;
        let (s, w) = (shared.clone(), wake.clone());
        let render_thread = std::thread::spawn(move || {
            let ctx = ctx_addr as RenderCtx;
            let fmt = cstr("rgb0");
            loop {
                {
                    let mut p = w.pending.lock().unwrap();
                    while !*p && !s.stop.load(Ordering::Relaxed) {
                        p = w.cond.wait(p).unwrap();
                    }
                    *p = false;
                }
                if s.stop.load(Ordering::Relaxed) {
                    break;
                }
                // SAFETY: render calls happen only on this thread.
                let flags = unsafe { (s.api.render_update)(ctx) };
                if flags & UPDATE_FRAME == 0 {
                    continue;
                }
                let (vw, vh) = (s.int("dwidth"), s.int("dheight"));
                if vw <= 0 || vh <= 0 {
                    continue;
                }
                let (w_px, h_px) = fit((vw as u32, vh as u32), opts.max_size);
                let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w_px, h_px);
                let mut size = [w_px as c_int, h_px as c_int];
                let mut stride: usize = w_px as usize * 4;
                let bytes = buf.make_mut_bytes();
                let mut params = [
                    RenderParam {
                        kind: PARAM_SW_SIZE,
                        data: size.as_mut_ptr() as *mut c_void,
                    },
                    RenderParam {
                        kind: PARAM_SW_FORMAT,
                        data: fmt.as_ptr() as *mut c_void,
                    },
                    RenderParam {
                        kind: PARAM_SW_STRIDE,
                        data: &mut stride as *mut usize as *mut c_void,
                    },
                    RenderParam {
                        kind: PARAM_SW_POINTER,
                        data: bytes.as_mut_ptr() as *mut c_void,
                    },
                    RenderParam {
                        kind: PARAM_INVALID,
                        data: std::ptr::null_mut(),
                    },
                ];
                if unsafe { (s.api.render)(ctx, params.as_mut_ptr()) } < 0 {
                    continue;
                }
                // "rgb0" leaves the 4th byte undefined; make it opaque.
                for px in bytes.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
                on_frame(buf);
            }
            unsafe { (s.api.render_free)(ctx) };
        });

        // Event thread: drains mpv's event queue and remembers load errors.
        let s = shared.clone();
        let event_thread = std::thread::spawn(move || {
            loop {
                // SAFETY: the handle stays valid until terminate_destroy,
                // which happens after this thread has seen SHUTDOWN.
                let ev = unsafe { &*(s.api.wait_event)(s.handle, -1.0) };
                match ev.event_id {
                    EVENT_SHUTDOWN => break,
                    EVENT_END_FILE if !ev.data.is_null() => {
                        let end = unsafe { &*(ev.data as *const EndFile) };
                        if end.reason == END_FILE_REASON_ERROR {
                            *s.last_error.lock().unwrap() = Some(s.api.err(end.error));
                        }
                    }
                    _ => {}
                }
                if s.stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        Ok(Player {
            shared,
            wake,
            render_thread: Some(render_thread),
            event_thread: Some(event_thread),
        })
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let rc = unsafe { (self.shared.api.command)(self.shared.handle, ptrs.as_mut_ptr()) };
        if rc < 0 {
            Err(self.shared.api.err(rc))
        } else {
            Ok(())
        }
    }

    pub fn load(&self, url: &str) -> Result<(), String> {
        *self.shared.last_error.lock().unwrap() = None;
        self.set_flag("pause", false);
        self.command(&["loadfile", url, "replace"])
    }

    /// Reloads the current video at `pos` (used when changing quality).
    pub fn load_at(&self, url: &str, pos: f64) -> Result<(), String> {
        let start = format!("start={pos:.2}");
        // mpv 0.38+ takes an index before the options; older versions don't.
        self.command(&["loadfile", url, "replace", "-1", &start])
            .or_else(|_| self.command(&["loadfile", url, "replace", &start]))
    }

    pub fn set_string(&self, name: &str, v: &str) {
        unsafe {
            (self.shared.api.set_property_string)(
                self.shared.handle,
                cstr(name).as_ptr(),
                cstr(v).as_ptr(),
            )
        };
    }

    pub fn string(&self, name: &str) -> Option<String> {
        let api = self.shared.api;
        let ptr = unsafe { (api.get_property_string)(self.shared.handle, cstr(name).as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let out = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { (api.free)(ptr as *mut c_void) };
        Some(out)
    }

    /// Chapter start times in seconds.
    pub fn chapters(&self) -> Vec<f64> {
        let n = self.shared.int("chapter-list/count");
        (0..n)
            .filter_map(|i| self.double(&format!("chapter-list/{i}/time")))
            .collect()
    }

    /// Title of the chapter playing now.
    pub fn chapter_title(&self) -> Option<String> {
        self.string("chapter-metadata/title")
            .or_else(|| {
                let i = self.shared.int("chapter");
                (i >= 0)
                    .then(|| self.string(&format!("chapter-list/{i}/title")))
                    .flatten()
            })
            .filter(|t| !t.is_empty())
    }

    pub fn stop(&self) {
        let _ = self.command(&["stop"]);
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.command(&["seek", &format!("{secs:.3}"), "absolute"]);
    }

    pub fn seek_by(&self, delta: f64) {
        let _ = self.command(&["seek", &format!("{delta:.3}"), "relative"]);
    }

    pub fn set_flag(&self, name: &str, on: bool) {
        let mut v: c_int = on as c_int;
        unsafe {
            (self.shared.api.set_property)(
                self.shared.handle,
                cstr(name).as_ptr(),
                FORMAT_FLAG,
                &mut v as *mut c_int as *mut c_void,
            )
        };
    }

    pub fn set_double(&self, name: &str, mut v: f64) {
        unsafe {
            (self.shared.api.set_property)(
                self.shared.handle,
                cstr(name).as_ptr(),
                FORMAT_DOUBLE,
                &mut v as *mut f64 as *mut c_void,
            )
        };
    }

    pub fn double(&self, name: &str) -> Option<f64> {
        self.shared.double(name)
    }

    pub fn flag(&self, name: &str) -> bool {
        let mut v: c_int = 0;
        let rc = unsafe {
            (self.shared.api.get_property)(
                self.shared.handle,
                cstr(name).as_ptr(),
                FORMAT_FLAG,
                &mut v as *mut c_int as *mut c_void,
            )
        };
        rc >= 0 && v != 0
    }

    /// Why the last file failed to load, if it did.
    pub fn error(&self) -> Option<String> {
        self.shared.last_error.lock().unwrap().clone()
    }
}

impl Shared {
    fn int(&self, name: &str) -> i64 {
        let mut v: i64 = 0;
        unsafe {
            (self.api.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_INT64,
                &mut v as *mut i64 as *mut c_void,
            )
        };
        v
    }

    fn double(&self, name: &str) -> Option<f64> {
        let mut v: f64 = 0.0;
        let rc = unsafe {
            (self.api.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                FORMAT_DOUBLE,
                &mut v as *mut f64 as *mut c_void,
            )
        };
        (rc >= 0).then_some(v)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        {
            *self.wake.pending.lock().unwrap() = true;
            self.wake.cond.notify_one();
        }
        if let Some(t) = self.render_thread.take() {
            let _ = t.join();
        }
        unsafe { (self.shared.api.wakeup)(self.shared.handle) };
        if let Some(t) = self.event_thread.take() {
            let _ = t.join();
        }
        // SAFETY: render context is freed and no thread uses the handle now.
        unsafe { (self.shared.api.terminate_destroy)(self.shared.handle) };
    }
}

/// Scales (w, h) down to fit inside `max`, keeping the aspect ratio.
pub fn fit((w, h): (u32, u32), (mw, mh): (u32, u32)) -> (u32, u32) {
    if w <= mw && h <= mh {
        return (w, h);
    }
    let s = (mw as f64 / w as f64).min(mh as f64 / h as f64);
    (
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    )
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_inside_max() {
        assert_eq!(fit((1280, 720), (1920, 1080)), (1280, 720));
        assert_eq!(fit((3840, 2160), (1920, 1080)), (1920, 1080));
        assert_eq!(fit((1080, 1920), (1920, 1080)), (608, 1080));
        assert_eq!(fmt_time(3723.4), "1:02:03");
        assert_eq!(fmt_time(65.0), "1:05");
    }
}
