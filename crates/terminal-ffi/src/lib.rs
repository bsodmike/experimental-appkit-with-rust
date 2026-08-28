//! # terminal-ffi
//!
//! The waist of the hourglass (PRD §2): the C ABI, and nothing else. Everything
//! here is one of the four shapes allowed to cross (PRD §4) — an opaque handle,
//! a `#[repr(C)]` struct, a byte buffer with an explicit length, or a function
//! pointer with a context.
//!
//! Four rules shape every function below.
//!
//! - **The handle is opaque.** C holds a token, never a view of Rust's
//!   internals, so the engine can be restructured without touching the header.
//! - **Null is always checked** and returns a status, never a crash (PRD §6.5).
//! - **No panic escapes.** Every entry point catches; unwinding out of an
//!   `extern "C"` function aborts the process, which kills the user's session
//!   (PRD §12).
//! - **Data crosses by copying into caller-owned buffers.** There is no second
//!   lifetime to get wrong and no free-function to forget (PRD §10-A, §11).

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use terminal_core::prelude::{Frame, Key, Keypad, Modifiers, TerminalSize};
use terminal_pty::{ChildOutcome, SpawnOptions, Terminal};

/// The terminal, as C sees it: a token it stores and hands back, and never
/// dereferences.
pub struct TerminalSession {
    terminal: Terminal,
    /// The frame the last copy-out was rendered into. Kept here so a redraw
    /// reuses its capacity rather than allocating (PRD §10-A).
    frame: std::sync::Mutex<Frame>,
}

/// The result of a fallible call. Out-parameters carry results; the return
/// value carries status (PRD §12).
#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalStatus {
    Ok = 0,
    /// The handle, or a required out-parameter, was null.
    NullHandle = -1,
    /// A panic was caught at the boundary. The session may be damaged, but the
    /// process is still alive.
    Panicked = -2,
    /// An argument was malformed — a null buffer with a non-zero length, text
    /// that is not UTF-8, a size of zero.
    InvalidArgument = -3,
    /// The shell could not be started.
    SpawnFailed = -4,
    /// Writing to the pty failed; the shell has probably gone.
    IoError = -5,
    /// The caller's buffer was too small. The out-parameter says how much is
    /// needed; nothing was copied.
    BufferTooSmall = -6,
}

/// A screen size in cells.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalSizeC {
    pub rows: u16,
    pub cols: u16,
}

/// A borrowed UTF-8 byte buffer: pointer plus explicit length, never
/// NUL-terminated, because terminal data legitimately contains NULs (PRD §4.3).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalBytes {
    pub bytes: *const u8,
    pub len: u32,
}

/// One run of the visible screen: consecutive columns sharing a style,
/// described as a slice of the frame's text buffer (PRD §10).
///
/// `fg` and `bg` are packed rather than resolved: `0x00_000000` is the terminal
/// default, `0x01_0000II` a palette index, `0x02_RRGGBB` truecolour. The engine
/// owns no theme, so the frontend resolves them.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalRun {
    pub utf8_offset: u32,
    pub utf8_len: u32,
    pub fg: u32,
    pub bg: u32,
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub attrs: u16,
}

/// The caller-owned buffers one frame is copied into. Allocate once, reuse
/// every frame: in the steady state neither side allocates.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalFrameBuffers {
    pub runs: *mut TerminalRun,
    pub runs_cap: u32,
    pub text: *mut u8,
    pub text_cap: u32,
}

/// What was copied, and everything else one redraw needs.
///
/// Filled in even when the buffers were too small, so the caller learns the
/// sizes it needs and can grow and retry — the two-call sizing pattern of
/// PRD §11, with no allocation to free.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalFrameInfo {
    pub runs_len: u32,
    pub text_len: u32,
    pub rows: u16,
    pub cols: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
}

/// Which key was pressed. `Char` carries its character in
/// [`TerminalKeyEvent::codepoint`]; `F` and `KeypadDigit` carry their number in
/// [`TerminalKeyEvent::number`].
#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalKeyCode {
    Char = 0,
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    F,
    KeypadDigit,
    KeypadEnter,
    KeypadPlus,
    KeypadMinus,
    KeypadMultiply,
    KeypadDivide,
    KeypadDecimal,
    KeypadEquals,
}

/// Modifier bits. `Cmd` is deliberately absent: on macOS it means an
/// application command, and those never reach the engine (PRD §8).
pub const TERMINAL_MOD_SHIFT: u16 = 1 << 0;
pub const TERMINAL_MOD_ALT: u16 = 1 << 1;
pub const TERMINAL_MOD_CTRL: u16 = 1 << 2;

/// The bits of [`TerminalRun::attrs`]. They mirror the engine's `CellAttrs`
/// exactly — a test in this crate fails if the two ever drift.
pub const TERMINAL_ATTR_BOLD: u16 = 1 << 0;
pub const TERMINAL_ATTR_DIM: u16 = 1 << 1;
pub const TERMINAL_ATTR_ITALIC: u16 = 1 << 2;
pub const TERMINAL_ATTR_UNDERLINE: u16 = 1 << 3;
pub const TERMINAL_ATTR_BLINK: u16 = 1 << 4;
pub const TERMINAL_ATTR_REVERSE: u16 = 1 << 5;
pub const TERMINAL_ATTR_HIDDEN: u16 = 1 << 6;
pub const TERMINAL_ATTR_STRIKETHROUGH: u16 = 1 << 7;

/// The tags in the top byte of [`TerminalRun::fg`] and `bg`: how a frontend
/// tells "the terminal default" from a colour that merely looks like one.
pub const TERMINAL_COLOR_TAG_SHIFT: u32 = 24;
pub const TERMINAL_COLOR_DEFAULT: u32 = 0x00;
pub const TERMINAL_COLOR_INDEXED: u32 = 0x01;
pub const TERMINAL_COLOR_RGB: u32 = 0x02;

/// A key press as the view reports it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalKeyEvent {
    pub code: TerminalKeyCode,
    /// The character for [`TerminalKeyCode::Char`], as a Unicode scalar value.
    pub codepoint: u32,
    /// The function-key number, or the keypad digit.
    pub number: u8,
    /// A bit-set of `TERMINAL_MOD_*`.
    pub modifiers: u16,
}

/// How the shell ended, and whether it has.
///
/// A frontend closes its window on a clean exit and keeps it open on anything
/// else, so the three cases have to be distinguishable: still running, ended
/// cleanly, ended badly. `hung_up` without `exited` is the third kind of
/// trouble — the reader thread stopped for its own reasons and the shell's fate
/// is unknown, which must never be read as success.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalChildStatus {
    /// The reader thread has stopped: there is nothing more to display.
    pub hung_up: bool,
    /// The shell was reaped and the fields below mean something.
    pub exited: bool,
    /// Its exit status, when it was not killed. Zero is an ordinary end.
    pub exit_code: i32,
    /// The signal that killed it, or zero.
    pub signal: i32,
}

/// Called when the screen changes, once per burst rather than once per read
/// (PRD §7).
///
/// It runs on the reader thread with no lock held, and must not call back into
/// this API or block: the frontend's version does nothing but ask the main
/// thread to redraw. `ctx` is handed back untouched (PRD §4.4).
pub type TerminalWakeUpFn = extern "C" fn(ctx: *mut c_void);

/// One environment variable for the child process.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalEnvPair {
    pub key: TerminalBytes,
    pub value: TerminalBytes,
}

/// Everything needed to start a terminal.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TerminalConfig {
    pub size: TerminalSizeC,
    /// The program to run, as UTF-8 bytes.
    pub program: TerminalBytes,
    /// Its arguments, as an array of byte buffers. May be null when `args_len`
    /// is zero.
    pub args: *const TerminalBytes,
    pub args_len: u32,
    /// The directory the shell starts in. An empty buffer means the user's home
    /// directory, which is what an app bundle wants — its own working directory
    /// is `/`.
    pub cwd: TerminalBytes,
    /// Extra environment variables, applied over the inherited environment and
    /// over the engine's own defaults. May be null when `env_len` is zero.
    ///
    /// `TERM` and `COLORTERM` are set by the engine whether or not they appear
    /// here: what `TERM` names is the engine's capability statement, not the
    /// frontend's decoration. Listing them here overrides that.
    pub env: *const TerminalEnvPair,
    pub env_len: u32,
    /// The wake-up callback, or null for none. Spelled out rather than written
    /// as `Option<TerminalWakeUpFn>`, which cbindgen renders as an opaque
    /// struct C cannot fill in — the header is generated precisely so that
    /// kind of mismatch is visible instead of silent (PRD §14).
    pub wake_up: Option<extern "C" fn(ctx: *mut c_void)>,
    pub wake_up_ctx: *mut c_void,
}

/// The frontend's context pointer, carried to the wake-up callback.
///
/// The callback can run on the reader thread, so the pointer crosses threads.
/// The contract, which the frontend must honour, is the usual one for this
/// shape: it must remain valid until `terminal_destroy` returns, and whatever
/// it points at must tolerate being used from another thread — in practice it
/// is a view pointer that the callback only ever `dispatch_async`es with.
struct WakeContext(*mut c_void);
// Safety: see the contract above. The pointer is never dereferenced by Rust.
unsafe impl Send for WakeContext {}
unsafe impl Sync for WakeContext {}

impl WakeContext {
    /// The pointer, handed back to the callback untouched. This is a method
    /// rather than a field access at the call site so the closure captures the
    /// wrapper — which is what carries the `Send`/`Sync` promise — rather than
    /// the bare pointer, which does not.
    fn ptr(&self) -> *mut c_void {
        self.0
    }
}

/// The most recent error, kept for diagnostics.
///
/// PRD §12 offers this as optional, and a Debug build of the frontend draws it
/// on screen: a caught panic that reports only "something panicked" is the
/// least debuggable state the app can be in. Best-effort and global rather than
/// per-session, because a panic does not necessarily know which session it was
/// in — and because this is a diagnostic, not an API to program against.
static LAST_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn set_last_error(message: String) {
    if let Ok(mut slot) = LAST_ERROR.lock() {
        *slot = Some(message);
    }
}

/// Where the last panic happened, captured by a hook because `catch_unwind`
/// only hands back the payload and not the location — and the location is the
/// half that shortens the search.
static PANIC_LOCATION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown location".to_string());
            if let Ok(mut slot) = PANIC_LOCATION.lock() {
                *slot = Some(location);
            }
            // Deliberately silent: the default hook writes to stderr, and the
            // frontend reads the message from here instead (PRD §12).
        }));
    });
}

/// Turn a panic payload into something worth reading.
fn describe_panic(payload: &Box<dyn std::any::Any + Send>) -> String {
    let message = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked with a payload of an unknown type".to_string()
    };
    match PANIC_LOCATION.lock().ok().and_then(|slot| slot.clone()) {
        Some(location) => format!("{message} ({location})"),
        None => message,
    }
}

/// Run `f`, turning a panic into a status instead of an abort (PRD §12).
fn guard(f: impl FnOnce() -> TerminalStatus) -> TerminalStatus {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(payload) => {
            set_last_error(describe_panic(&payload));
            TerminalStatus::Panicked
        }
    }
}

/// Borrow a caller's byte buffer. `None` for a null pointer with a non-zero
/// length; an empty slice for a zero length, null or not.
///
/// # Safety
/// `bytes.bytes` must point to `bytes.len` readable bytes for the duration of
/// the call.
unsafe fn as_slice<'a>(bytes: TerminalBytes) -> Option<&'a [u8]> {
    if bytes.len == 0 {
        return Some(&[]);
    }
    if bytes.bytes.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(bytes.bytes, bytes.len as usize) })
}

/// Start a shell on a new pty and return the handle.
///
/// Returns null if the configuration is unusable or the shell cannot start.
/// The caller must pass the handle to [`terminal_destroy`] exactly once.
///
/// # Safety
/// `config` must point to a valid `TerminalConfig`, whose byte buffers are
/// readable for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_create(config: *const TerminalConfig) -> *mut TerminalSession {
    install_panic_hook();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(config) = (unsafe { config.as_ref() }) else {
            set_last_error("terminal_create: null config".to_string());
            return std::ptr::null_mut();
        };
        if config.size.rows == 0 || config.size.cols == 0 {
            return std::ptr::null_mut();
        }
        let Some(program) = (unsafe { as_slice(config.program) }) else {
            return std::ptr::null_mut();
        };
        let Ok(program) = std::str::from_utf8(program) else {
            return std::ptr::null_mut();
        };

        let mut args: Vec<String> = Vec::new();
        if config.args_len > 0 {
            if config.args.is_null() {
                return std::ptr::null_mut();
            }
            let raw = unsafe { std::slice::from_raw_parts(config.args, config.args_len as usize) };
            for arg in raw {
                let Some(bytes) = (unsafe { as_slice(*arg) }) else {
                    return std::ptr::null_mut();
                };
                match std::str::from_utf8(bytes) {
                    Ok(s) => args.push(s.to_string()),
                    Err(_) => return std::ptr::null_mut(),
                }
            }
        }

        let Some(cwd) = (unsafe { as_slice(config.cwd) }) else {
            return std::ptr::null_mut();
        };
        let Ok(cwd) = std::str::from_utf8(cwd) else {
            return std::ptr::null_mut();
        };

        let mut env: Vec<(String, String)> = Vec::new();
        if config.env_len > 0 {
            if config.env.is_null() {
                return std::ptr::null_mut();
            }
            let raw = unsafe { std::slice::from_raw_parts(config.env, config.env_len as usize) };
            for pair in raw {
                let (Some(key), Some(value)) = (unsafe { as_slice(pair.key) }, unsafe {
                    as_slice(pair.value)
                }) else {
                    return std::ptr::null_mut();
                };
                match (std::str::from_utf8(key), std::str::from_utf8(value)) {
                    (Ok(k), Ok(v)) if !k.is_empty() => env.push((k.to_string(), v.to_string())),
                    _ => return std::ptr::null_mut(),
                }
            }
        }

        let mut options = SpawnOptions::new(program).args(args);
        // An empty cwd means home. The frontend should not have to work out
        // what "home" is, and an app bundle's own directory is `/`.
        match cwd {
            "" => {
                if let Some(home) = std::env::var_os("HOME") {
                    options = options.cwd(home);
                }
            }
            dir => options = options.cwd(dir),
        }
        for (key, value) in env {
            options = options.env(key, value);
        }

        let ctx = WakeContext(config.wake_up_ctx);
        let wake_up = config.wake_up;
        let size = TerminalSize::new(config.size.rows, config.size.cols);
        let terminal = Terminal::spawn_with(&options, size, move || {
            if let Some(wake_up) = wake_up {
                wake_up(ctx.ptr());
            }
        });
        match terminal {
            Ok(terminal) => Box::into_raw(Box::new(TerminalSession {
                terminal,
                frame: std::sync::Mutex::new(Frame::new()),
            })),
            Err(e) => {
                set_last_error(format!("terminal_create: {e}"));
                std::ptr::null_mut()
            }
        }
    }));
    if result.is_err() {
        set_last_error("terminal_create panicked".to_string());
    }
    result.unwrap_or(std::ptr::null_mut())
}

/// Stop the terminal and free it. Null is ignored; calling it twice on the same
/// handle is not (PRD §6).
///
/// This is the ordered shutdown of PRD §7 — the reader thread is signalled and
/// joined, and the shell hung up, before the memory goes away.
///
/// # Safety
/// `session` must be a handle from [`terminal_create`] that has not yet been
/// destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_destroy(session: *mut TerminalSession) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(unsafe { Box::from_raw(session) });
    }));
}

/// Borrow the session behind a handle.
///
/// # Safety
/// `session` must be a live handle from [`terminal_create`], or null.
unsafe fn session_ref<'a>(session: *mut TerminalSession) -> Option<&'a TerminalSession> {
    unsafe { session.as_ref() }
}

/// Send committed text — what the input system produced (PRD §8). The bytes go
/// to the shell unchanged.
///
/// # Safety
/// `bytes` must point to `len` readable bytes for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_send_text(
    session: *mut TerminalSession,
    bytes: *const u8,
    len: u32,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(text) = (unsafe { as_slice(TerminalBytes { bytes, len }) }) else {
            return TerminalStatus::InvalidArgument;
        };
        match session.terminal.send(text) {
            Ok(()) => TerminalStatus::Ok,
            Err(_) => TerminalStatus::IoError,
        }
    })
}

/// Send a key press. The engine encodes it against the current modes — DECCKM
/// for the arrows, DECKPAM for the keypad (PRD §8).
///
/// # Safety
/// `session` must be a live handle from [`terminal_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_send_key(
    session: *mut TerminalSession,
    event: TerminalKeyEvent,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(key) = key_of(event) else {
            return TerminalStatus::InvalidArgument;
        };
        match session
            .terminal
            .send_key(key, modifiers_of(event.modifiers))
        {
            Ok(()) => TerminalStatus::Ok,
            Err(_) => TerminalStatus::IoError,
        }
    })
}

/// Send pasted text, bracketed if the program asked for that (PRD §8).
///
/// # Safety
/// `bytes` must point to `len` readable bytes for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_paste(
    session: *mut TerminalSession,
    bytes: *const u8,
    len: u32,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(text) = (unsafe { as_slice(TerminalBytes { bytes, len }) }) else {
            return TerminalStatus::InvalidArgument;
        };
        let Ok(text) = std::str::from_utf8(text) else {
            return TerminalStatus::InvalidArgument;
        };
        match session.terminal.paste(text) {
            Ok(()) => TerminalStatus::Ok,
            Err(_) => TerminalStatus::IoError,
        }
    })
}

/// Resize: the engine reflows and the kernel signals the program (PRD §16.4).
///
/// # Safety
/// `session` must be a live handle from [`terminal_create`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_resize(
    session: *mut TerminalSession,
    rows: u16,
    cols: u16,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        if rows == 0 || cols == 0 {
            return TerminalStatus::InvalidArgument;
        }
        match session.terminal.resize(TerminalSize::new(rows, cols)) {
            Ok(()) => TerminalStatus::Ok,
            Err(_) => TerminalStatus::IoError,
        }
    })
}

/// Copy the visible screen into caller-owned buffers (PRD §10-A).
///
/// One call per frame takes the lock once, so the frame is internally
/// consistent — no tearing between row 3 and row 40. `info` is filled in even
/// when the buffers are too small, so a caller can size its buffers by calling
/// once with zero capacity and again with enough.
///
/// # Safety
/// `buffers`, when non-null, must describe writable memory of the stated
/// capacities, and `info` must point to a writable `TerminalFrameInfo`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_copy_frame(
    session: *mut TerminalSession,
    buffers: *const TerminalFrameBuffers,
    info: *mut TerminalFrameInfo,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(info) = (unsafe { info.as_mut() }) else {
            return TerminalStatus::NullHandle;
        };

        let mut frame = session
            .frame
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        session.terminal.render_into(&mut frame);

        *info = TerminalFrameInfo {
            runs_len: frame.runs().len() as u32,
            text_len: frame.text().len() as u32,
            rows: frame.size().rows,
            cols: frame.size().cols,
            cursor_row: frame.cursor().row,
            cursor_col: frame.cursor().col,
            cursor_visible: frame.cursor_visible(),
        };

        let Some(buffers) = (unsafe { buffers.as_ref() }) else {
            // Sizing call: the caller wanted the lengths, not the data.
            return TerminalStatus::BufferTooSmall;
        };
        if buffers.runs_cap < info.runs_len || buffers.text_cap < info.text_len {
            return TerminalStatus::BufferTooSmall;
        }
        if info.runs_len > 0 && buffers.runs.is_null() {
            return TerminalStatus::InvalidArgument;
        }
        if info.text_len > 0 && buffers.text.is_null() {
            return TerminalStatus::InvalidArgument;
        }

        for (i, run) in frame.runs().iter().enumerate() {
            // Safety: capacity was checked above.
            unsafe { buffers.runs.add(i).write(to_c_run(run)) };
        }
        if info.text_len > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    frame.text().as_ptr(),
                    buffers.text,
                    info.text_len as usize,
                )
            };
        }
        TerminalStatus::Ok
    })
}

/// Copy the window title (OSC 0/2) as UTF-8 into `buf`.
///
/// `out_len` always receives the length the title needs, so calling with a zero
/// capacity is how you learn the size (PRD §11). Nothing is copied when the
/// buffer is too small.
///
/// # Safety
/// `buf`, when non-null, must be writable for `cap` bytes, and `out_len` must
/// point to a writable `u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_copy_title(
    session: *mut TerminalSession,
    buf: *mut u8,
    cap: u32,
    out_len: *mut u32,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(out_len) = (unsafe { out_len.as_mut() }) else {
            return TerminalStatus::NullHandle;
        };
        let title = session.terminal.session().title();
        *out_len = title.len() as u32;
        if cap < title.len() as u32 {
            return TerminalStatus::BufferTooSmall;
        }
        if !title.is_empty() {
            if buf.is_null() {
                return TerminalStatus::InvalidArgument;
            }
            unsafe { std::ptr::copy_nonoverlapping(title.as_ptr(), buf, title.len()) };
        }
        TerminalStatus::Ok
    })
}

/// Whether the shell has exited and the reader thread has stopped.
///
/// # Safety
/// `out` must point to a writable `bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_has_hung_up(
    session: *mut TerminalSession,
    out: *mut bool,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(out) = (unsafe { out.as_mut() }) else {
            return TerminalStatus::NullHandle;
        };
        *out = session.terminal.has_hung_up();
        TerminalStatus::Ok
    })
}

/// How the shell ended, and whether it has (PRD-mac §13).
///
/// # Safety
/// `out` must point to a writable `TerminalChildStatus`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_child_status(
    session: *mut TerminalSession,
    out: *mut TerminalChildStatus,
) -> TerminalStatus {
    guard(|| {
        let Some(session) = (unsafe { session_ref(session) }) else {
            return TerminalStatus::NullHandle;
        };
        let Some(out) = (unsafe { out.as_mut() }) else {
            return TerminalStatus::NullHandle;
        };
        let mut status = TerminalChildStatus {
            hung_up: session.terminal.has_hung_up(),
            ..TerminalChildStatus::default()
        };
        match session.terminal.child_outcome() {
            Some(ChildOutcome::Code(code)) => {
                status.exited = true;
                status.exit_code = code;
            }
            Some(ChildOutcome::Signal(signal)) => {
                status.exited = true;
                status.signal = signal;
            }
            None => {}
        }
        *out = status;
        TerminalStatus::Ok
    })
}

/// Copy the most recent error message, for a Debug build to show on screen.
///
/// Uses the two-call sizing pattern of PRD §11: call with a zero capacity to
/// learn the length, then again with room. Empty when nothing has gone wrong.
/// Reading does not clear it — a redraw asks repeatedly while the message is
/// still on screen.
///
/// # Safety
/// `buf`, when non-null, must be writable for `cap` bytes, and `out_len` must
/// point to a writable `u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_copy_last_error(
    buf: *mut u8,
    cap: u32,
    out_len: *mut u32,
) -> TerminalStatus {
    guard(|| {
        let Some(out_len) = (unsafe { out_len.as_mut() }) else {
            return TerminalStatus::NullHandle;
        };
        let message = LAST_ERROR
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_default();
        *out_len = message.len() as u32;
        if cap < message.len() as u32 {
            return TerminalStatus::BufferTooSmall;
        }
        if !message.is_empty() {
            if buf.is_null() {
                return TerminalStatus::InvalidArgument;
            }
            unsafe { std::ptr::copy_nonoverlapping(message.as_ptr(), buf, message.len()) };
        }
        TerminalStatus::Ok
    })
}

/// Forget the most recent error, so a Debug overlay can be dismissed.
///
/// # Safety
/// Takes no pointers and is safe to call from any thread; it is `unsafe` only
/// to keep every entry point in this header declared the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn terminal_clear_last_error() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(mut slot) = LAST_ERROR.lock() {
            *slot = None;
        }
    }));
}

fn to_c_run(run: &terminal_core::prelude::Run) -> TerminalRun {
    TerminalRun {
        utf8_offset: run.utf8_offset,
        utf8_len: run.utf8_len,
        fg: run.fg,
        bg: run.bg,
        row: run.row,
        col: run.col,
        cols: run.cols,
        attrs: run.attrs,
    }
}

fn modifiers_of(bits: u16) -> Modifiers {
    let mut mods = Modifiers::NONE;
    if bits & TERMINAL_MOD_SHIFT != 0 {
        mods = mods | Modifiers::SHIFT;
    }
    if bits & TERMINAL_MOD_ALT != 0 {
        mods = mods | Modifiers::ALT;
    }
    if bits & TERMINAL_MOD_CTRL != 0 {
        mods = mods | Modifiers::CTRL;
    }
    mods
}

fn key_of(event: TerminalKeyEvent) -> Option<Key> {
    use TerminalKeyCode as C;
    Some(match event.code {
        C::Char => Key::Char(char::from_u32(event.codepoint)?),
        C::Enter => Key::Enter,
        C::Tab => Key::Tab,
        C::Backspace => Key::Backspace,
        C::Escape => Key::Escape,
        C::Delete => Key::Delete,
        C::Insert => Key::Insert,
        C::Up => Key::Up,
        C::Down => Key::Down,
        C::Right => Key::Right,
        C::Left => Key::Left,
        C::Home => Key::Home,
        C::End => Key::End,
        C::PageUp => Key::PageUp,
        C::PageDown => Key::PageDown,
        C::F => Key::F(event.number),
        C::KeypadDigit => Key::Keypad(Keypad::Digit(event.number)),
        C::KeypadEnter => Key::Keypad(Keypad::Enter),
        C::KeypadPlus => Key::Keypad(Keypad::Plus),
        C::KeypadMinus => Key::Keypad(Keypad::Minus),
        C::KeypadMultiply => Key::Keypad(Keypad::Multiply),
        C::KeypadDivide => Key::Keypad(Keypad::Divide),
        C::KeypadDecimal => Key::Keypad(Keypad::Decimal),
        C::KeypadEquals => Key::Keypad(Keypad::Equals),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const SIZE: TerminalSizeC = TerminalSizeC { rows: 10, cols: 40 };

    fn bytes(s: &str) -> TerminalBytes {
        TerminalBytes {
            bytes: s.as_ptr(),
            len: s.len() as u32,
        }
    }

    const NO_BYTES: TerminalBytes = TerminalBytes {
        bytes: std::ptr::null(),
        len: 0,
    };

    /// A configuration running `/bin/sh -c script`, with no wake-up.
    fn config(script: &str, args: &mut Vec<TerminalBytes>) -> TerminalConfig {
        args.push(bytes("-c"));
        args.push(bytes(script));
        TerminalConfig {
            size: SIZE,
            program: bytes("/bin/sh"),
            args: args.as_ptr(),
            args_len: args.len() as u32,
            cwd: NO_BYTES,
            env: std::ptr::null(),
            env_len: 0,
            wake_up: None,
            wake_up_ctx: std::ptr::null_mut(),
        }
    }

    /// Own the handle for the duration of a test, destroying it on the way out
    /// however the test ends.
    struct Handle(*mut TerminalSession);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { terminal_destroy(self.0) };
        }
    }

    fn spawn(script: &str) -> (Handle, Vec<TerminalBytes>) {
        // The argument buffers must outlive the create call, and the script
        // string must outlive them, so both are handed back to the caller.
        let mut args = Vec::new();
        let config = config(script, &mut args);
        let handle = unsafe { terminal_create(&config) };
        // The engine records why it could not start, so a failure here says
        // what went wrong rather than only that something did.
        assert!(
            !handle.is_null(),
            "the shell should have started: {}",
            last_error()
        );
        (Handle(handle), args)
    }

    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        // Generous, because these tests share a machine with every other
        // test in the workspace: a slow answer is not a wrong one.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn hung_up(handle: &Handle) -> bool {
        let mut out = false;
        assert_eq!(
            unsafe { terminal_has_hung_up(handle.0, &mut out) },
            TerminalStatus::Ok
        );
        out
    }

    /// Copy a frame the way a frontend would: size it, allocate, copy.
    fn copy_frame(handle: &Handle) -> (TerminalFrameInfo, Vec<TerminalRun>, String) {
        let mut info = TerminalFrameInfo::default();
        let status = unsafe { terminal_copy_frame(handle.0, std::ptr::null(), &mut info) };
        assert_eq!(status, TerminalStatus::BufferTooSmall, "the sizing call");

        let mut runs = vec![
            TerminalRun {
                utf8_offset: 0,
                utf8_len: 0,
                fg: 0,
                bg: 0,
                row: 0,
                col: 0,
                cols: 0,
                attrs: 0,
            };
            info.runs_len.max(1) as usize
        ];
        let mut text = vec![0u8; info.text_len.max(1) as usize];
        let buffers = TerminalFrameBuffers {
            runs: runs.as_mut_ptr(),
            runs_cap: runs.len() as u32,
            text: text.as_mut_ptr(),
            text_cap: text.len() as u32,
        };
        let status = unsafe { terminal_copy_frame(handle.0, &buffers, &mut info) };
        assert_eq!(status, TerminalStatus::Ok);
        runs.truncate(info.runs_len as usize);
        text.truncate(info.text_len as usize);
        (info, runs, String::from_utf8(text).expect("utf-8"))
    }

    #[test]
    fn a_handle_round_trips_and_the_shells_output_crosses_the_boundary() {
        let (handle, _args) = spawn("printf hello");
        wait_for("the shell to hang up", || hung_up(&handle));

        let (info, runs, text) = copy_frame(&handle);
        assert_eq!((info.rows, info.cols), (10, 40));
        assert!(info.cursor_visible);
        assert_eq!(text, "hello");
        assert_eq!(runs.len(), 1);
        let run = runs[0];
        assert_eq!((run.row, run.col, run.cols), (0, 0, 5));
        assert_eq!(
            &text[run.utf8_offset as usize..(run.utf8_offset + run.utf8_len) as usize],
            "hello"
        );
        assert_eq!((run.fg, run.bg), (0, 0), "default on default packs to zero");
    }

    #[test]
    fn a_frame_buffer_that_is_too_small_copies_nothing_and_says_how_much_it_needs() {
        let (handle, _args) = spawn("printf 'a long line of output'");
        wait_for("output", || {
            let mut info = TerminalFrameInfo::default();
            unsafe { terminal_copy_frame(handle.0, std::ptr::null(), &mut info) };
            info.text_len > 0
        });

        let mut info = TerminalFrameInfo::default();
        let mut one_run = [TerminalRun {
            utf8_offset: 0,
            utf8_len: 0,
            fg: 0,
            bg: 0,
            row: 0,
            col: 0,
            cols: 0,
            attrs: 0,
        }];
        let mut tiny = [0u8; 2];
        let buffers = TerminalFrameBuffers {
            runs: one_run.as_mut_ptr(),
            runs_cap: 1,
            text: tiny.as_mut_ptr(),
            text_cap: tiny.len() as u32,
        };
        let status = unsafe { terminal_copy_frame(handle.0, &buffers, &mut info) };
        assert_eq!(status, TerminalStatus::BufferTooSmall);
        assert!(info.text_len > 2, "it still reports the size needed");
        assert_eq!(tiny, [0, 0], "and copies nothing");
    }

    #[test]
    fn input_crosses_the_boundary_as_text_and_as_keys() {
        let (handle, _args) = spawn("stty -echo -icanon -isig; echo ready; head -c 6 | cat -v");
        wait_for("the shell to be listening", || {
            copy_frame(&handle).2.contains("ready")
        });

        let text = "hi";
        assert_eq!(
            unsafe { terminal_send_text(handle.0, text.as_ptr(), text.len() as u32) },
            TerminalStatus::Ok
        );
        let ctrl_c = TerminalKeyEvent {
            code: TerminalKeyCode::Char,
            codepoint: 'c' as u32,
            number: 0,
            modifiers: TERMINAL_MOD_CTRL,
        };
        assert_eq!(
            unsafe { terminal_send_key(handle.0, ctrl_c) },
            TerminalStatus::Ok
        );
        let up = TerminalKeyEvent {
            code: TerminalKeyCode::Up,
            codepoint: 0,
            number: 0,
            modifiers: 0,
        };
        assert_eq!(
            unsafe { terminal_send_key(handle.0, up) },
            TerminalStatus::Ok
        );

        wait_for("the shell to hang up", || hung_up(&handle));
        let screen = copy_frame(&handle).2;
        assert!(screen.contains("hi^C^[[A"), "screen was: {screen:?}");
    }

    #[test]
    fn a_paste_is_framed_when_the_program_asks_for_it() {
        // The shell turns bracketed paste on, exactly as a real one does, and
        // then reads back the framing the engine put around the text.
        let (handle, _args) =
            spawn("stty -echo -icanon; printf '\u{1b}[?2004h'; echo ready; head -c 14 | cat -v");
        wait_for("the shell to be listening", || {
            copy_frame(&handle).2.contains("ready")
        });
        let pasted = "ls";
        assert_eq!(
            unsafe { terminal_paste(handle.0, pasted.as_ptr(), pasted.len() as u32) },
            TerminalStatus::Ok
        );
        wait_for("the shell to hang up", || hung_up(&handle));
        let screen = copy_frame(&handle).2;
        assert!(
            screen.contains("^[[200~ls^[[201~"),
            "screen was: {screen:?}"
        );
    }

    #[test]
    fn the_title_uses_the_two_call_sizing_pattern() {
        let (handle, _args) = spawn("printf '\u{1b}]2;my title\u{7}'");
        wait_for("the title to arrive", || {
            let mut len = 0u32;
            unsafe { terminal_copy_title(handle.0, std::ptr::null_mut(), 0, &mut len) };
            len > 0
        });

        let mut len = 0u32;
        let status = unsafe { terminal_copy_title(handle.0, std::ptr::null_mut(), 0, &mut len) };
        assert_eq!(status, TerminalStatus::BufferTooSmall);
        assert_eq!(len as usize, "my title".len());

        let mut buf = vec![0u8; len as usize];
        let status =
            unsafe { terminal_copy_title(handle.0, buf.as_mut_ptr(), buf.len() as u32, &mut len) };
        assert_eq!(status, TerminalStatus::Ok);
        assert_eq!(String::from_utf8(buf).unwrap(), "my title");
    }

    #[test]
    fn a_resize_crosses_and_is_validated() {
        let (handle, _args) = spawn("sleep 30");
        assert_eq!(
            unsafe { terminal_resize(handle.0, 20, 60) },
            TerminalStatus::Ok
        );
        assert_eq!(
            unsafe { terminal_resize(handle.0, 0, 60) },
            TerminalStatus::InvalidArgument,
            "a screen with no rows is not a resize"
        );
        let (info, _, _) = copy_frame(&handle);
        assert_eq!((info.rows, info.cols), (20, 60));
    }

    static WAKE_UPS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn count_wake_up(ctx: *mut c_void) {
        // The context comes back untouched, which is the whole point of it
        // (PRD §4.4): here it carries the counter to bump.
        let counter = unsafe { &*(ctx as *const AtomicUsize) };
        counter.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn the_wake_up_callback_fires_with_its_context() {
        WAKE_UPS.store(0, Ordering::SeqCst);
        let mut args = vec![bytes("-c"), bytes("printf hello")];
        let config = TerminalConfig {
            size: SIZE,
            program: bytes("/bin/sh"),
            args: args.as_mut_ptr(),
            args_len: args.len() as u32,
            cwd: NO_BYTES,
            env: std::ptr::null(),
            env_len: 0,
            wake_up: Some(count_wake_up),
            wake_up_ctx: &WAKE_UPS as *const AtomicUsize as *mut c_void,
        };
        let handle = Handle(unsafe { terminal_create(&config) });
        assert!(!handle.0.is_null());
        wait_for("a wake-up", || WAKE_UPS.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn an_empty_cwd_starts_the_shell_at_home() {
        // An app bundle's own working directory is `/`, which is not where
        // anybody wants a shell to open.
        let (handle, _args) = spawn("printf '%s' \"$PWD\"");
        wait_for("the shell to hang up", || hung_up(&handle));
        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(copy_frame(&handle).2, home);
    }

    #[test]
    fn a_given_cwd_is_where_the_shell_starts() {
        let mut args = Vec::new();
        let mut config = config("printf '%s' \"$PWD\"", &mut args);
        config.cwd = bytes("/usr");
        let handle = Handle(unsafe { terminal_create(&config) });
        assert!(!handle.0.is_null());
        wait_for("the shell to hang up", || hung_up(&handle));
        assert_eq!(copy_frame(&handle).2, "/usr");
    }

    #[test]
    fn the_engine_names_the_terminal_and_the_frontend_can_still_override_it() {
        let (handle, _args) = spawn("printf '%s' \"$TERM\"");
        wait_for("the shell to hang up", || hung_up(&handle));
        assert_eq!(copy_frame(&handle).2, "xterm-256color");

        let mut args = Vec::new();
        let mut config = config("printf '%s|%s' \"$TERM\" \"$GRILL\"", &mut args);
        let env = [
            TerminalEnvPair {
                key: bytes("TERM"),
                value: bytes("dumb"),
            },
            TerminalEnvPair {
                key: bytes("GRILL"),
                value: bytes("set"),
            },
        ];
        config.env = env.as_ptr();
        config.env_len = env.len() as u32;
        let handle = Handle(unsafe { terminal_create(&config) });
        assert!(!handle.0.is_null());
        wait_for("the shell to hang up", || hung_up(&handle));
        assert_eq!(copy_frame(&handle).2, "dumb|set");
    }

    #[test]
    fn a_malformed_environment_is_refused_rather_than_half_applied() {
        let mut args = Vec::new();
        let mut empty_key = config("true", &mut args);
        let env = [TerminalEnvPair {
            key: NO_BYTES,
            value: bytes("orphan"),
        }];
        empty_key.env = env.as_ptr();
        empty_key.env_len = env.len() as u32;
        assert!(
            unsafe { terminal_create(&empty_key) }.is_null(),
            "an empty key names nothing"
        );

        let mut args = Vec::new();
        let mut null_array = config("true", &mut args);
        null_array.env_len = 1; // ...but the array is null
        assert!(unsafe { terminal_create(&null_array) }.is_null());
    }

    #[test]
    fn a_null_handle_is_a_status_and_never_a_crash() {
        let null = std::ptr::null_mut();
        let mut info = TerminalFrameInfo::default();
        let mut flag = false;
        let mut len = 0u32;
        assert_eq!(
            unsafe { terminal_send_text(null, b"x".as_ptr(), 1) },
            TerminalStatus::NullHandle
        );
        assert_eq!(
            unsafe { terminal_resize(null, 10, 10) },
            TerminalStatus::NullHandle
        );
        assert_eq!(
            unsafe { terminal_copy_frame(null, std::ptr::null(), &mut info) },
            TerminalStatus::NullHandle
        );
        assert_eq!(
            unsafe { terminal_copy_title(null, std::ptr::null_mut(), 0, &mut len) },
            TerminalStatus::NullHandle
        );
        assert_eq!(
            unsafe { terminal_has_hung_up(null, &mut flag) },
            TerminalStatus::NullHandle
        );
        // Destroying null is a no-op, not a crash.
        unsafe { terminal_destroy(null) };
    }

    #[test]
    fn a_null_out_parameter_is_caught_too() {
        let (handle, _args) = spawn("sleep 30");
        assert_eq!(
            unsafe { terminal_copy_frame(handle.0, std::ptr::null(), std::ptr::null_mut()) },
            TerminalStatus::NullHandle
        );
        assert_eq!(
            unsafe { terminal_has_hung_up(handle.0, std::ptr::null_mut()) },
            TerminalStatus::NullHandle
        );
    }

    #[test]
    fn a_bad_configuration_returns_null_rather_than_starting_something() {
        let mut args = Vec::new();
        let mut zero_rows = config("true", &mut args);
        zero_rows.size = TerminalSizeC { rows: 0, cols: 80 };
        assert!(unsafe { terminal_create(&zero_rows) }.is_null(), "no rows");

        let mut args = Vec::new();
        let mut null_program = config("true", &mut args);
        null_program.program = TerminalBytes {
            bytes: std::ptr::null(),
            len: 4,
        };
        assert!(
            unsafe { terminal_create(&null_program) }.is_null(),
            "null program"
        );

        assert!(
            unsafe { terminal_create(std::ptr::null()) }.is_null(),
            "null config"
        );

        let mut args = Vec::new();
        let mut missing = config("true", &mut args);
        missing.program = bytes("/nonexistent/shell");
        assert!(
            unsafe { terminal_create(&missing) }.is_null(),
            "no such program"
        );
    }

    fn child_status(handle: &Handle) -> TerminalChildStatus {
        let mut status = TerminalChildStatus::default();
        assert_eq!(
            unsafe { terminal_child_status(handle.0, &mut status) },
            TerminalStatus::Ok
        );
        status
    }

    #[test]
    fn a_clean_exit_is_reported_differently_from_a_crash() {
        // The frontend closes its window on the first and keeps it open on the
        // second, so this is the whole of that rule (PRD-mac §13).
        let (clean, _args) = spawn("exit 0");
        wait_for("a clean exit", || hung_up(&clean));
        let status = child_status(&clean);
        assert!(status.hung_up && status.exited);
        assert_eq!(status.exit_code, 0);
        assert_eq!(status.signal, 0);

        let (failed, _args) = spawn("exit 3");
        wait_for("a failed exit", || hung_up(&failed));
        let status = child_status(&failed);
        assert!(status.exited);
        assert_eq!(status.exit_code, 3);
    }

    #[test]
    fn a_signalled_shell_reports_its_signal() {
        let (handle, _args) = spawn("kill -TERM $$");
        wait_for("the signal", || hung_up(&handle));
        let status = child_status(&handle);
        assert!(status.exited);
        assert_eq!(status.signal, 15);
        assert_eq!(status.exit_code, 0, "an exit code would read as success");
    }

    #[test]
    fn a_running_shell_has_not_hung_up_and_has_not_exited() {
        let (handle, _args) = spawn("sleep 30");
        let status = child_status(&handle);
        assert!(!status.hung_up);
        assert!(!status.exited);
    }

    #[test]
    fn a_null_out_parameter_for_the_child_status_is_caught() {
        let (handle, _args) = spawn("sleep 30");
        assert_eq!(
            unsafe { terminal_child_status(handle.0, std::ptr::null_mut()) },
            TerminalStatus::NullHandle
        );
        let mut status = TerminalChildStatus::default();
        assert_eq!(
            unsafe { terminal_child_status(std::ptr::null_mut(), &mut status) },
            TerminalStatus::NullHandle
        );
    }

    /// The last-error slot and the panic hook are process-global, so the tests
    /// that touch them take a turn rather than racing each other.
    static GLOBAL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serialised() -> std::sync::MutexGuard<'static, ()> {
        GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read the last error with the two-call pattern, as a frontend would.
    fn last_error() -> String {
        let mut len = 0u32;
        let status = unsafe { terminal_copy_last_error(std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            assert_eq!(status, TerminalStatus::Ok);
            return String::new();
        }
        assert_eq!(status, TerminalStatus::BufferTooSmall);
        let mut buf = vec![0u8; len as usize];
        assert_eq!(
            unsafe { terminal_copy_last_error(buf.as_mut_ptr(), len, &mut len) },
            TerminalStatus::Ok
        );
        String::from_utf8(buf).expect("utf-8")
    }

    #[test]
    fn a_caught_panic_leaves_a_message_worth_reading() {
        let _turn = serialised();
        // "Panicked" alone is the least debuggable state the app can be in, so
        // the payload and the location are kept for a Debug build to draw.
        unsafe { terminal_clear_last_error() };
        install_panic_hook();
        let previous = std::panic::take_hook();
        install_panic_hook_for_test();

        let status = guard(|| panic!("a deliberate test panic"));
        assert_eq!(status, TerminalStatus::Panicked);

        std::panic::set_hook(previous);
        let message = last_error();
        assert!(message.contains("a deliberate test panic"), "{message}");
        assert!(
            message.contains("lib.rs"),
            "the location is the useful half: {message}"
        );
    }

    /// The real hook is installed once per process by `terminal_create`; this
    /// test does not create a session, so it installs the same one directly.
    fn install_panic_hook_for_test() {
        std::panic::set_hook(Box::new(|info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown location".to_string());
            if let Ok(mut slot) = PANIC_LOCATION.lock() {
                *slot = Some(location);
            }
        }));
    }

    #[test]
    fn a_failed_spawn_says_why() {
        let _turn = serialised();
        unsafe { terminal_clear_last_error() };
        let mut args = Vec::new();
        let mut missing = config("true", &mut args);
        missing.program = bytes("/nonexistent/shell");
        assert!(unsafe { terminal_create(&missing) }.is_null());
        let message = last_error();
        assert!(message.contains("terminal_create"), "{message}");
    }

    #[test]
    fn clearing_the_last_error_empties_it() {
        let _turn = serialised();
        unsafe { terminal_clear_last_error() };
        assert!(last_error().is_empty());
    }

    #[test]
    fn the_exported_attribute_bits_match_the_engine() {
        // The header is the frontend's only source for these. If the engine
        // renumbered a flag and this crate did not, every styled run would draw
        // with the wrong decoration and nothing would say so.
        use terminal_core::prelude::{CellAttrs, Color};
        assert_eq!(TERMINAL_ATTR_BOLD, CellAttrs::BOLD.bits());
        assert_eq!(TERMINAL_ATTR_DIM, CellAttrs::DIM.bits());
        assert_eq!(TERMINAL_ATTR_ITALIC, CellAttrs::ITALIC.bits());
        assert_eq!(TERMINAL_ATTR_UNDERLINE, CellAttrs::UNDERLINE.bits());
        assert_eq!(TERMINAL_ATTR_BLINK, CellAttrs::BLINK.bits());
        assert_eq!(TERMINAL_ATTR_REVERSE, CellAttrs::REVERSE.bits());
        assert_eq!(TERMINAL_ATTR_HIDDEN, CellAttrs::HIDDEN.bits());
        assert_eq!(TERMINAL_ATTR_STRIKETHROUGH, CellAttrs::STRIKETHROUGH.bits());

        assert_eq!(
            Color::Default.pack() >> TERMINAL_COLOR_TAG_SHIFT,
            TERMINAL_COLOR_DEFAULT
        );
        assert_eq!(
            Color::Indexed(7).pack() >> TERMINAL_COLOR_TAG_SHIFT,
            TERMINAL_COLOR_INDEXED
        );
        assert_eq!(
            Color::Rgb(1, 2, 3).pack() >> TERMINAL_COLOR_TAG_SHIFT,
            TERMINAL_COLOR_RGB
        );
    }

    #[test]
    fn a_panic_becomes_a_status_rather_than_an_abort() {
        // PRD §12: unwinding out of an extern "C" function aborts the process,
        // which kills the user's session. Every entry point goes through this.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the test output clean
        let status = guard(|| panic!("something went wrong"));
        std::panic::set_hook(previous);
        assert_eq!(status, TerminalStatus::Panicked);
    }

    #[test]
    fn text_that_is_not_utf8_is_rejected_where_it_must_be() {
        let (handle, _args) = spawn("sleep 30");
        let invalid = [0xFF, 0xFE];
        // A paste has to be text, so it is validated...
        assert_eq!(
            unsafe { terminal_paste(handle.0, invalid.as_ptr(), 2) },
            TerminalStatus::InvalidArgument
        );
        // ...but send_text is a byte channel and passes bytes through, since
        // the shell is entitled to receive whatever the user typed.
        assert_eq!(
            unsafe { terminal_send_text(handle.0, invalid.as_ptr(), 2) },
            TerminalStatus::Ok
        );
        assert_eq!(
            unsafe { terminal_send_text(handle.0, std::ptr::null(), 2) },
            TerminalStatus::InvalidArgument,
            "a null buffer with a length is malformed"
        );
    }
}
