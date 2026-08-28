# PRD: The Rust ↔ AppKit Boundary

**Status:** design complete, ready for implementation. Every decision in §17 —
including the three formerly-open items in §17.3 — is now resolved, so code may
be written against the contract below.

**Scope:** this document describes *how the two halves of the program talk to
each other* — the Rust ↔ AppKit boundary. It was originally paired with a
`SPEC.md` describing *what the terminal does*; that document has since been
removed, so this PRD now stands alone as the single source of truth. A few
sections below still refer to design sketches that lived in that spec — those
references are retained as historical context for why a decision went the way it
did, and each is resolved inline.

---

## 1. What this document is for

You are building one program out of two languages that cannot see each other.

Rust does not know what an `NSView` is. Objective-C does not know what a
`Vec<Cell>` is. Neither can call the other directly, and neither can safely touch
the other's memory. Everything in this document follows from that single fact.

The goal here is a **mental model** you can reason with — so that when you later
write a function signature at the boundary, you already know whether it is
legal, who frees what, and which thread it runs on.

Read §2–§4 for the model. Read §5–§12 for the contract. Read §13–§14 for the
mechanics of actually wiring it up. §16 covers the buffer model, which reflow
makes the most consequential engine decision in the document.

---

## 2. The mental model: an hourglass

The system is not three equal layers. It is **rich, narrow, rich**:

```
        ┌─────────────────────────────────────────────┐
        │  RUST — the terminal engine                 │
        │                                             │
        │  Grid, Cursor, Cell, VT parser, scrollback, │   rich types,
        │  modes, selection, PTY, key encoding        │   ownership,
        │  Vec<T>, String, Result<T,E>, traits        │   generics
        └─────────────────────────────────────────────┘
                            │
                   ╔════════▼════════╗
                   ║   THE C ABI     ║   ← the waist.
                   ║                 ║     pointers, integers,
                   ║  opaque handles ║     fixed-layout structs,
                   ║  repr(C) structs║     byte buffers,
                   ║  byte buffers   ║     function pointers.
                   ║  function ptrs  ║     nothing else.
                   ╚════════▲════════╝
                            │
        ┌─────────────────────────────────────────────┐
        │  NATIVE — the macOS frontend                │
        │                                             │
        │  NSApplication, NSWindow, TerminalView,     │   rich objects,
        │  NSEvent, Core Text, NSPasteboard, IME      │   ARC, runtime
        └─────────────────────────────────────────────┘
```

The waist is deliberately impoverished. That is not a limitation you are working
around — it is the design. Every type that crosses is a type both sides had to
agree on, and the fewer of those there are, the fewer ways the contract can be
broken.

**The single sentence to remember:** *Rust owns terminal semantics, AppKit owns
macOS presentation, and the only vocabulary they share is C.*

---

## 3. Why C, and nothing richer

It is worth understanding *why* the waist is so narrow, because the reasons tell
you what is safe to do.

**Rust's normal types have no stable memory layout.** By default Rust uses
`repr(Rust)`, which lets the compiler reorder struct fields, insert padding
however it likes, and change all of that between compiler versions. A `String`
is a pointer, a length and a capacity — but *in what order*, and with what
niche optimisations, is explicitly unspecified. There is nothing for C to agree
with. `#[repr(C)]` opts a specific struct into C's layout rules, and only then
does it have a shape another language can rely on.

**Rust's generics and traits do not exist at runtime in a callable form.**
`Vec<T>` is monomorphised per `T`; a `dyn Trait` is a fat pointer whose vtable
layout is a compiler implementation detail. There is no way to name any of it
from C.

**Each side manages memory with rules the other does not follow.** Objective-C
objects are reference-counted by the Obj-C runtime, with ARC inserting
retain/release. Swift has its own. Rust uses ownership and `Drop`, with no
runtime at all. If Obj-C calls `free()` on a Rust allocation, it is calling the
wrong allocator's free on the wrong bookkeeping — undefined behaviour, and the
kind that corrupts silently rather than crashing.

**C is the only thing all three understand.** The C ABI on macOS is stable,
documented, and every language on the platform can speak it. Objective-C is
literally a superset of C. Swift has explicit C interop. Rust has `extern "C"`
and `#[repr(C)]`.

So the boundary speaks C — not because C is good, but because it is the only
common ground that exists.

---

## 4. The four shapes that may cross

Everything at the waist is one of exactly four things. If you find yourself
wanting a fifth, the design is wrong.

### 4.1 Opaque handles

A pointer to something Rust owns, which the native side stores and passes back,
but **never dereferences**.

```c
typedef struct TerminalSession TerminalSession;   // never defined in C

TerminalSession *terminal_create(uint16_t rows, uint16_t cols);
void             terminal_destroy(TerminalSession *session);
```

C is told the type exists but never told what is inside it. That is the point:
the native side physically cannot poke at Rust's internals, and Rust is free to
restructure them without breaking the header. The handle is just a token meaning
"the terminal you made earlier."

This is the primary mechanism. Almost everything else is a function that takes a
handle as its first argument.

### 4.2 Plain data (`#[repr(C)]` structs)

Fixed-layout aggregates of primitives, copied by value across the boundary.

```rust
#[repr(C)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}
```

`#[repr(C)]` is the promise: fields in declaration order, C's alignment and
padding rules. Only then may it cross. Use fixed-width types (`u16`, `u32`,
`i32`) rather than `usize` at the boundary, so the layout does not silently
depend on the target's pointer width.

### 4.3 Byte buffers (pointer + length)

Text and bulk data cross as a pointer and an explicit length. Never as a
NUL-terminated string, because terminal data legitimately contains NUL bytes,
and never as a Rust `String`.

```c
void terminal_write(TerminalSession *s, const uint8_t *bytes, size_t len);
```

Direction matters enormously here, and §10 is devoted to the harder direction
(Rust → native).

### 4.4 Function pointers (with a context pointer)

The only way Rust can tell the native side that something happened.

```c
typedef void (*terminal_event_fn)(void *ctx, TerminalEvent event);

void terminal_set_event_handler(TerminalSession *s,
                                terminal_event_fn handler,
                                void *ctx);
```

The `void *ctx` is essential and easy to omit by accident. A bare C function
pointer carries no state, so `ctx` is how the callback finds its way back to the
right `TerminalView` instance. Rust stores it and hands it back untouched.

### Summary

| Shape | Rust side | C side | Who dereferences |
|---|---|---|---|
| Opaque handle | `*mut TerminalSession` | `TerminalSession *` | Rust only |
| Plain data | `#[repr(C)] struct` | `struct` | both (it is a copy) |
| Byte buffer | `*const u8` + `usize` | `const uint8_t *` + `size_t` | whoever owns it (§6) |
| Callback | `extern "C" fn` pointer | function pointer | native only |

---

## 5. Which layer owns what

The dividing test is a question you can ask about any piece of behaviour:

> **Could this work on Linux with no UI at all?**
> If yes, it belongs in Rust. If it needs an `NSEvent`, it belongs in native.

| Concern | Owner | Why |
|---|---|---|
| VT/ANSI parsing | Rust | Pure byte-stream logic. |
| Grid, cursor, attributes, modes | Rust | Terminal semantics. |
| Scrollback | Rust | Terminal semantics. |
| Selection *state* | Rust | Needs to understand wrapped lines, wide chars, scrollback offsets — all engine knowledge. |
| PTY, shell process | Rust | POSIX, not AppKit. |
| Key → byte-sequence encoding | Rust | `Ctrl+C` → `0x03` is terminal semantics, not macOS. |
| Window, view, drawing | Native | AppKit. |
| Font metrics, glyph rasterisation | Native | Core Text. |
| Which physical key was pressed | Native | `NSEvent`. |
| IME / dead keys / composition | Native | `NSTextInputClient`. |
| Clipboard | Native | `NSPasteboard`. |
| Menus, ⌘N/⌘W/⌘Q | Native | Application behaviour, not terminal behaviour. |

Two entries deserve comment because they are the ones people get wrong.

**Selection state in Rust, selection gestures in native.** The view knows a drag
happened at pixel (412, 96). It converts that to a cell (row 6, col 51) using
font metrics it owns, and tells Rust. Rust decides what that *means* — which
cells are selected, how the selection extends across a wrapped line, what the
selected text is.

**Key encoding in Rust.** This surprises people, but `Ctrl+C` producing byte
`0x03`, and the arrow keys producing `ESC [ A` or `ESC O A` depending on DECCKM
mode, are properties of the *terminal*, not of macOS. The native side reports
"this key, with these modifiers"; Rust decides what bytes that becomes. Keeping
this in Rust is also what makes it testable.

---

## 6. Ownership and lifetime

**The rule, in one line: whoever allocated it, frees it — through a function
from that same side.**

### Creating and destroying the handle

Rust hands out a pointer to a heap allocation it still logically owns:

```rust
#[no_mangle]
pub extern "C" fn terminal_create(rows: u16, cols: u16) -> *mut TerminalSession {
    let session = Box::new(TerminalSession::new(rows, cols));
    Box::into_raw(session)          // Rust stops tracking it; C now holds the token
}

#[no_mangle]
pub unsafe extern "C" fn terminal_destroy(session: *mut TerminalSession) {
    if session.is_null() { return; }
    drop(Box::from_raw(session));   // ownership returns to Rust, Drop runs
}
```

`Box::into_raw` is precisely "stop managing this, but do not free it." The
allocation stays alive with no owner until `Box::from_raw` adopts it again. That
pairing *is* the lifetime contract, and the native side's only obligation is to
call `terminal_destroy` exactly once.

### The rules that follow

1. **Never `free()` a Rust pointer from Obj-C or Swift**, and never hand a
   `malloc`'d pointer to Rust to free. Different allocators.
2. **Every "give me a thing" has a matching "release the thing"**, unless the
   data was copied into a caller-owned buffer (§10 — the preferred pattern
   precisely because it has no second call to forget).
3. **A borrowed pointer is valid only for the duration of the call.** If Rust
   passes the native side a `const uint8_t *` in a callback, the native side must
   copy it before returning. It may not stash it.
4. **The handle must outlive every use of it.** Destroying a session while the
   PTY reader thread is still running is a use-after-free; §7 makes shutdown
   explicit for this reason.
5. **Null is always checked.** A null handle returns an error, never crashes.

---

## 7. Threading

**Decision: the engine is internally synchronised — `Arc<Mutex<Emulator>>` —
and the mutex lives behind the opaque handle.**

From the native side's point of view there is no lock. The API is simply
thread-safe: call it from anywhere, it does the right thing. That is a deliberate
choice; exposing lock/unlock across the FFI would make deadlock the frontend's
problem to solve.

```
   ┌────────────────────────┐        ┌───────────────────────────┐
   │  AppKit main thread    │        │  PTY reader thread (Rust) │
   │                        │        │                           │
   │  drawRect: ───────┐    │        │   read(pty_fd) ──┐        │
   │  keyDown:  ───────┤    │        │                  │        │
   │  resize    ───────┤    │        │   parse into ────┤        │
   └───────────────────┼────┘        │   the grid       │        │
                       │             └──────────────────┼────────┘
                       │                                │
                       └──────────► Mutex<Emulator> ◄───┘
                                          │
                                          │  wake-up (coalesced)
                                          ▼
                                  dispatch_async(main) → setNeedsDisplay:
```

**Why a dedicated reader thread and not an async runtime.** There is exactly one
file descriptor to watch. There is nothing to multiplex, so an async runtime adds
a scheduler, a dependency, and a set of lifetime puzzles in exchange for nothing.
One thread that blocks on `read()` is simpler and easier to reason about.

**The reader thread parses.** It does not merely shuttle bytes to the main
thread — it takes the lock, runs the VT parser, mutates the grid, and releases.
The main thread must never parse, because parsing is unbounded work driven by a
hostile-by-accident data source (any program can print a megabyte).

**Lock discipline.** The lock is held for microseconds — one chunk of parsing, or
one frame's worth of copying — and *never* across a callback into Obj-C. A
callback that re-entered the terminal API would deadlock on a non-reentrant
mutex. Notifications are therefore fired *after* the lock is released.

### Coalescing wake-ups: the load-bearing detail

This is the single most common way a naive terminal falls over, and it costs
almost nothing to get right.

`cat` of a large file produces thousands of small reads. If each one calls
`dispatch_async(dispatch_get_main_queue(), ...)` to request a redraw, you enqueue
thousands of blocks onto the main queue. The main thread then spends its time
draining a backlog of redundant redraw requests, the UI goes unresponsive, and
the app appears to hang under exactly the workload it should handle best.

The fix is an atomic flag:

```
reader thread:                        main thread:
  parse chunk                           (block runs)
  if dirty.swap(true) == false:         dirty.store(false)
      dispatch_async(main, redraw)      draw current state
```

Only the `false → true` transition posts a block. Every subsequent chunk sets a
flag that is already set and posts nothing. The main thread always draws the
*latest* state, and the queue never holds more than one pending redraw. Output
bursts are absorbed by the reader thread at full speed while the UI repaints at
whatever rate it can sustain.

**Implemented.** `terminal-core`'s `Session` is the mutex and the coalesced
wake-up; `terminal-pty`'s `Terminal` is the pty, the shell and the `pty-reader`
thread that parses on the far side of it. The details — why the callback fires
outside the lock, why the dirty flag is cleared before the copy rather than
after, why the pty is split into two handles, and how the reader thread is made
joinable — are in `docs/adrs/2026-08-28.adr-session-pty-and-reader-thread.md`.

### Shutdown

Destroying a session must be ordered, or a background thread outlives the memory
it is writing into. There are **two** such threads: the PTY reader (this section)
and the scrollback-reflow maintenance thread (§16.5).

1. Native calls `terminal_shutdown(handle)`.
2. Rust signals **both** threads to stop and **joins both**.
3. Only then may native call `terminal_destroy(handle)`.

A single `terminal_destroy` that does all of this internally is acceptable and
safer; what is not acceptable is a destroy that returns while either thread is
still running.

---

## 8. Path A — a keystroke travels down

This is the flow that shows why the input boundary has *three* channels rather
than one. Here is the concrete reason.

```
  physical key press
        │
        ▼
  NSEvent arrives at TerminalView
        │
        ├── is it an application command?  (⌘N, ⌘W, ⌘Q, ⌘C, ⌘V)
        │      └─► handled natively. Never reaches Rust.
        │
        ├── is it text the input system produced?  (letters, dead keys, IME)
        │      └─► NSTextInputClient insertText: → UTF-8 bytes
        │             └─► terminal_send_text(handle, bytes, len)
        │
        └── is it a key with terminal meaning?  (arrows, F-keys, Ctrl+letter,
               Home/End/PgUp, Escape, Tab, Return, Backspace)
               └─► terminal_send_key(handle, keycode, modifiers)
                      └─► Rust encodes per current modes → bytes → PTY
```

**Why not just send `NSEvent.characters` for everything?**

Because it is lossy in both directions. `Ctrl+C` may give you a control
character or an empty string depending on layout. Arrow keys have no character
at all. And an IME composing Japanese produces text through a completely separate
path, with marked (provisional) text that must not be sent to the shell until it
is committed.

So: **text input and key events are different channels, and application commands
are a third that never crosses at all.** The engine receives "a key with
modifiers" and applies terminal rules (DECCKM changes what the arrow keys emit;
DECKPAM changes the keypad), or it receives finished UTF-8 text and passes it
through.

The IME rule specifically: while composition is active, the view displays marked
text itself and sends **nothing**. On commit, `insertText:` fires once and the
committed text crosses as UTF-8.

**Pointer input is a fourth channel, and it already exists in v1 for selection.**
Because selection state lives in Rust (§5), the view reports pointer gestures —
down / drag / up, at a cell, with button and modifiers — through
`terminal_pointer_event`. In v1 the engine turns these into selection. When mouse
reporting (§17, #15) is added post-MVP, the engine routes the *same* events to
PTY-byte encoding instead, exactly as it already does for keys: the view reports
what happened, Rust decides what it means. The one frontend rule to preserve now
is that holding Shift (or Option) must force local selection even once an
application has enabled mouse tracking.

---

## 9. Path B — shell output travels up

```
  shell writes to the pty
        │
        ▼
  reader thread: read() returns N bytes           ← arbitrary chunk boundary
        │
        ▼
  lock the mutex
        │
        ▼
  VT parser consumes the bytes                    ← stateful across reads
        │  ├─ mutates grid / cursor / modes
        │  ├─ records which rows changed (damage)
        │  └─ may queue a reply (see below)
        ▼
  unlock
        │
        ├─► if any reply bytes queued → write them back to the pty
        │
        └─► if dirty flag was false → dispatch_async(main) → setNeedsDisplay:
                                                    │
                                                    ▼
                                          drawRect: locks, copies out
                                          the visible rows, unlocks, draws
```

Two things in that diagram are easy to miss.

**The parser must be stateful across reads.** A read can end in the middle of
`ESC [ 3 1 m`, or in the middle of a multi-byte UTF-8 character. The parser
therefore lives across calls and resumes; feeding it arbitrary chunks must be
safe. This is not an edge case — it happens constantly with a fast-writing
program.

**The engine sometimes owes the shell an answer.** `CSI 6 n` (Device Status
Report) asks the terminal where its cursor is, and the application *blocks*
waiting for `ESC [ row ; col R` to come back. `CSI c` (Device Attributes) is the
same. So parsing is not a pure sink: it can produce bytes that must be written
back to the PTY. If this is forgotten, programs mysteriously hang rather than
failing loudly, which makes it an expensive omission to debug later.

The API must therefore make replies impossible to ignore — feeding input returns
the bytes owed, rather than hiding them behind a getter nobody calls.

**Implemented.** `Screen::advance` returns the owed bytes and is `#[must_use]`,
so dropping them is deliberate. The queue behind it is capped and overflow is
dropped: a program can ask faster than anyone drains, and an unbounded queue
would be a memory leak driven by untrusted output. Answers, and the rest of the
VT coverage this slice added — scrolling margins, DECSC/DECRC, DEC private
modes, the alternate screen, OSC titles — are recorded in
`docs/adrs/2026-08-28.adr-vt-coverage-and-alternate-screen.md`.

---

## 10. The read path, and why the obvious API is unsound

This is the most important technical section in the document.

The natural-looking API — the one a first sketch reaches for — is:

```c
const TerminalCell *terminal_get_cells(TerminalSession *s);   // ⚠ unsound
```

It returns a pointer into Rust's grid. With the threading model in §7, that
pointer is a bug: the moment the function returns, the lock is released, and the
reader thread is free to mutate — or reallocate — the very memory the native
side is now reading. There is no lifetime you can write in C that expresses
"valid until the next parse."

There are three sound alternatives.

**A. Copy into a caller-owned buffer — recommended.**

```c
size_t terminal_copy_visible(TerminalSession *s,
                             TerminalRun *out, size_t out_cap,
                             size_t *out_len);
```

The frontend owns a buffer it allocates once and reuses every frame. One call
per frame takes the lock, copies, releases. No second lifetime for anyone to get
wrong, no free-function to forget, and because the lock is taken exactly once,
the frame is internally consistent — no tearing between row 3 and row 40. Steady
state allocates nothing.

**B. A snapshot object.** `terminal_snapshot()` copies under the lock and returns
a second opaque handle that the UI reads at leisure and then frees. Cleanest
decoupling, but adds an allocation per frame and a second lifetime.

**C. A callback visitor.** Rust holds the lock and calls back per row. Zero
copies, but the callback must never re-enter the terminal API or it deadlocks —
a trap laid for whoever maintains this next — and it inverts control in a way
that fights `drawRect:`.

### What the copied data should look like

An earlier sketch proposed one struct per cell with a single `uint32_t
codepoint`. That cannot represent combining characters: one `u32` cannot hold
`e` + U+0301, nor an emoji ZWJ sequence.

**Recommendation: copy out *runs*, not cells.** A run is a span of consecutive
cells sharing the same colours and attributes, described as a slice of a UTF-8
buffer:

```rust
#[repr(C)]
pub struct TerminalRun {
    pub utf8_offset: u32,   // into an accompanying byte buffer
    pub utf8_len: u32,
    pub fg: u32,
    pub bg: u32,
    pub row: u16,           // display row
    pub col: u16,           // starting column
    pub cols: u16,          // columns occupied (wide chars count 2)
    pub attrs: u16,
}
```

`row` was not in the first sketch. It is here so one flat array of runs is
self-describing, and the copy-out call stays a single buffer with a single
length rather than runs plus a parallel row index. The field order above packs
to 24 bytes with no padding.

`fg` and `bg` are *packed*, not resolved: `0x00_000000` is the terminal default,
`0x01_0000II` a palette index, `0x02_RRGGBB` truecolour. The engine owns no
theme (§5), so "asked for the default" has to survive the boundary as something
the frontend can still recognise. A zeroed run therefore reads as "default on
default".

**Implemented.** `terminal-core`'s `render` module builds exactly this, as
`prelude::Frame` (one `String` plus one `Vec<Run>`, the two buffers the copy-out
call will fill) and `prelude::Run`. `Screen::render_into` refills a frame while
keeping its capacity, so the redraw path allocates nothing in the steady state,
and the cursor position is captured in the same snapshot so the caret cannot
disagree with the text under it. Coalescing, the wide-character spacer and the
`row` decision are recorded in `docs/adrs/2026-08-28.adr-render-frame.md`.
Damage tracking, selection, the alternate screen and a scrolled-back viewport
are later slices.

Three reasons this is better than a cell array:

1. **Core Text draws runs, not cells.** Per-cell glyph drawing is slow, and for
   combining marks it is simply wrong — the mark needs to be shaped together
   with its base character.
2. **It keeps Unicode assembly in Rust**, where the terminal semantics already
   live, which is what lets the renderer stay "dumb" — it draws what it is given
   and owns no terminal semantics.
3. **Far fewer items cross per frame** — a typical line is a handful of runs
   rather than 200 cells.

The cost is that the renderer positions by cell but draws by run: it advances a
fixed cell width per column rather than trusting the font's advances. That is
what real terminal emulators do, and it is the behaviour that makes a monospace
grid stay aligned.

---

## 11. Text and strings

- **Everything is UTF-8 bytes plus an explicit length.** Not NUL-terminated
  (terminal data contains NULs), never a Rust `String`.
- Native → Rust: `[myString UTF8String]` with `strlen`, or better, the bytes and
  length directly from `NSData`.
- Rust → native: `[[NSString alloc] initWithBytes:len:encoding:NSUTF8StringEncoding]`.
- For getters like the window title or the selected text, prefer the §10-A
  pattern: caller provides a buffer, Rust returns the number of bytes it wants.
  Call once with a null buffer to learn the size, once more to fill it. This
  avoids inventing a `terminal_free_string()` that someone will eventually forget
  to call.

---

## 12. Errors and panics

**A Rust panic must never unwind into Objective-C.** Since Rust 1.81, unwinding
out of an `extern "C"` function aborts the process rather than being undefined
behaviour — which is a genuine safety net, but not a solution: aborting still
kills the user's app and loses their session.

So every FFI entry point wraps its body:

```rust
#[no_mangle]
pub unsafe extern "C" fn terminal_resize(s: *mut TerminalSession,
                                         rows: u16, cols: u16) -> TerminalStatus {
    let Some(session) = s.as_mut() else { return TerminalStatus::NullHandle };
    match std::panic::catch_unwind(AssertUnwindSafe(|| session.resize(rows, cols))) {
        Ok(()) => TerminalStatus::Ok,
        Err(_) => TerminalStatus::Panicked,
    }
}
```

Conventions:

- Every fallible function returns a `#[repr(C)]` status enum with an explicit
  `i32` representation. Never `Result`, never `Option` — neither has a defined C
  layout.
- Out-parameters carry results; the return value carries status.
- A separate `terminal_last_error_message()` may provide a human-readable string
  for logging, using the two-call sizing pattern from §11.
- Rust's logging must never write to stdout/stderr — those are the PTY's, and
  diagnostic output injected into the terminal stream corrupts the display.

---

## 13. Objective-C++ or Swift?

Both work. They differ in how much ceremony sits between AppKit and the C header.

### Objective-C / Objective-C++

Objective-C is a strict superset of C. The header generated from Rust is just
`#import`ed and the functions are called directly:

```objc
@implementation TerminalView {
    TerminalSession *_session;
}

- (void)keyDown:(NSEvent *)event {
    terminal_send_key(_session, event.keyCode, (uint32_t)event.modifierFlags);
}
@end
```

There is no bridging layer at all — this is as thin as the seam can be.

**Objective-C++** (`.mm` files) additionally lets you use C++ in the glue. The
practical benefit is RAII: a small C++ wrapper class whose destructor calls
`terminal_destroy` removes an entire category of leak, and `std::vector` is a
natural home for the reusable render buffer from §10-A. AppKit code in the same
file continues to be ordinary Objective-C.

The constraint: **C++ types must never appear in the Rust-facing ABI.** C++ has
its own name mangling and its own layout rules. `std::vector` may live in the
glue; it may not cross the waist.

### Swift

Swift can call C directly, but the seam is noisier:

- The header must be exposed through a **bridging header** (app target) or a
  **module map** (framework / SwiftPM).
- Pointers become `UnsafeMutablePointer<T>`, `UnsafeRawPointer`,
  `UnsafeMutableBufferPointer`, and buffer access goes through `withUnsafeBytes`
  closures.
- **Only a non-capturing closure can become a C function pointer.** Swift will
  convert a closure to `@convention(c)` only if it captures nothing at all — so
  the callback from §4.4 cannot close over `self`. State must travel through the
  `void *ctx` parameter and be recovered with `Unmanaged<TerminalView>
  .fromOpaque(ctx).takeUnretainedValue()`. This is not a corner case; it is the
  normal path for every event callback.
- Objective-C **blocks are not C function pointers** either, so the same
  restriction applies if you reach for a block.

Swift is the more pleasant language for writing the *rest* of the app. It is the
less pleasant language for writing the seam.

### Recommendation

**Objective-C++ for the bridge and `TerminalView`.** It gives the thinnest
possible call path to the C ABI, plus RAII for handle and buffer lifetimes, which
are exactly the two things §6 says are easy to get wrong.

If the wider app is to be written in Swift, the clean arrangement is: a small
Objective-C++ bridge target exposing a *Swift-friendly Objective-C* class
(`TerminalSessionBridge`) that hides every `Unsafe*` type, with Swift consuming
that. Swift then never sees a raw pointer and never needs a non-capturing
callback.

What matters more than the choice: **the Rust side is identical either way.** The
C ABI does not change. This decision is reversible; the ones in §6, §7 and §10
are not.

---

## 14. The header, and the build

### Generate the header — do not write it

The C header must be produced from the Rust source by **cbindgen**, run as a
build step.

A hand-maintained header is a silent-corruption hazard. If Rust says a parameter
is `u32` and the header says `uint16_t`, nothing warns you — you get garbage
arguments or a smashed stack, at runtime, intermittently. And the header *will*
drift, because it is edited in a different file from the signature it describes.

### Link a static library

The Rust crate builds as `crate-type = ["staticlib"]`, producing a `.a` that
Xcode links into the app binary.

Prefer this to `cdylib`: a static library needs no `@rpath` configuration, no
separately code-signed dylib inside the bundle, and no runtime loader path to get
wrong at distribution time.

### The pipeline

```
  cargo build --release  ──►  libterminal_ffi.a  ──┐
         │                                          ├──►  Xcode links  ──►  Terminal.app
         └─ cbindgen  ──►  terminal.h  ─────────────┘
```

An Xcode **Run Script** build phase invokes cargo before the compile phase.

### Practical gotchas, in the order you will hit them

1. **Xcode's `PATH` does not include `~/.cargo/bin`.** Xcode build phases do not
   run your login shell, so `cargo: command not found` is the first thing that
   happens to everyone. Use an absolute path or set `PATH` explicitly in the
   script.
2. **Map Xcode's architecture to Rust's target triple.** `$ARCHS` gives `arm64`
   and/or `x86_64`; Rust wants `aarch64-apple-darwin` and `x86_64-apple-darwin`.
   For a universal build, build both and combine with `lipo -create`.
3. **Set `MACOSX_DEPLOYMENT_TARGET` to match Xcode's deployment target**, or you
   get linker warnings about objects built for different macOS versions.
4. **Rust's std pulls in system libraries.** Run
   `cargo rustc -- --print native-static-libs` to get the exact list to add to
   *Other Linker Flags* — typically `-lresolv` and a couple of frameworks.
   Guessing here wastes an afternoon; the compiler will just tell you.
5. **Keep `cargo`'s output directory out of Xcode's derived data**, so a clean in
   one does not silently invalidate the other.

---

## 15. What must never cross the boundary

A checklist to review any proposed signature against:

- ❌ `String`, `&str`, `Vec<T>`, `Option<T>`, `Result<T, E>` — no defined C layout.
- ❌ Enums without `#[repr(C)]` or an explicit integer representation.
- ❌ Trait objects, closures, generics, references with lifetimes.
- ❌ C++ types — `std::string`, `std::vector`, anything with a mangled name.
- ❌ Objective-C objects. **Rust must never learn what an `NSView` is.** If a
  Rust signature mentions an AppKit type, the architecture has failed.
- ❌ Pointers into Rust-owned memory that outlive the call (§10).
- ❌ Panics (§12).
- ❌ Ownership transfers with no matching release function (§6).

---

## 16. The buffer model, reflow, and coordinates

**Decision: wrapped lines reflow on resize.**

This is the decision with the widest blast radius, so it gets its own section.
Reflow is not a feature layered onto a grid — it determines how the buffer is
stored and forces a second coordinate system into existence.

### 16.1 Scrollback cannot be fixed-width rows

A terminal without reflow can store scrollback as what it displayed: rows of
exactly `cols` cells. With reflow it cannot, because the same text must be able
to render at any width.

Scrollback therefore stores **logical lines** — the text between two explicit
newlines, of unbounded length — and display rows are *derived* by wrapping a
logical line to the current width:

```
  logical line 41:  "the quick brown fox jumps over the lazy dog"
                     │
        width 20     ├──► row: "the quick brown fox "
                     ├──► row: "jumps over the lazy "
                     └──► row: "dog"

        width 40     ├──► row: "the quick brown fox jumps over the lazy "
                     └──► row: "dog"
```

The `wrapped` flag per display row still exists, but it is now an *output* of
wrapping rather than stored state.

**A display row is an index, not a copy.** This is the detail that makes the
whole approach affordable. A display row should be a `(line_id, start_offset,
len)` triple pointing into a logical line — not an owned `Vec<Cell>`. Reflowing
then never moves or copies cell data; it recomputes wrap points by scanning
accumulated display width. The difference between "rebuild the buffer" and
"recompute a small index" is the difference between reflow being expensive and
reflow being cheap, and it is decided here, by the storage layout.

**Refinement (see ADR 2026-08-27): this is the *scrollback* representation, not
the live screen.** The primary screen where mutation happens is an owned,
cell-addressable grid — so that cursor positioning, scroll regions and edits stay
trivial — and `(line_id, offset, len)` is how *scrollback and reflow* address
text, not how the active screen is stored. Scrollback logical lines are stored
packed (UTF-8 text plus attribute runs), and rows convert into that packed form
as they scroll off. §17.5 records the full set of storage decisions this implies.

The alternate screen is the exception: **it never reflows.** A full-screen
application owns its own layout and repaints completely on `SIGWINCH`, so
rewrapping its contents would corrupt a display the application is about to
redraw anyway. Reflow applies to the primary screen and its scrollback only.

### 16.2 Two coordinate systems

Reflow means a piece of text moves when the window resizes. Anything that
remembers a position must remember it in terms that survive that move.

| | Display coordinate | Logical coordinate |
|---|---|---|
| Shape | `(row, col)` in the visible viewport | `(line_id, char_offset)` |
| Stable across resize? | **No** | **Yes** |
| Used by | rendering, mouse hit-testing, cursor reporting (DSR) | selection anchors, viewport scroll anchor |

`line_id` must be a **monotonic counter, not an array index.** Scrollback evicts
its oldest lines, which would shift every index and silently invalidate every
stored anchor. A counter that only ever increases makes eviction harmless — an
anchor pointing at an evicted line is detectably stale rather than quietly wrong.

**The frontend never sees logical coordinates.** Native says "the drag started
at cell (6, 51)" and "draw me the visible rows"; Rust converts to and from
logical coordinates internally. This preserves the rule from §5: the frontend
knows about pixels and cells, the engine knows about text.

### 16.3 What must survive a rewrap

Three pieces of state are anchored logically and recomputed after every reflow:

1. **Selection.** Anchored at `(line_id, offset)` for both ends so it stays glued
   to its text across *scrolling* and *appended output*. Across a *width-change
   reflow*, v1 **clears** the selection rather than recomputing its geometry
   (§17, #14): the anchors exist regardless, but placing a rewrapped selection —
   a select-all across deep history in particular — would force exactly the
   synchronous reflow §16.5 exists to avoid. Surviving small selections is a
   contained later upgrade, precisely because the anchors are already there.
2. **Viewport position.** Anchored at "the top of the view is line `L`, wrap
   segment `S`" rather than "scrolled up N rows". Without this, resizing while
   scrolled back jumps you somewhere arbitrary — a bug users notice immediately.
3. **The cursor.** Re-derived from its logical position, not carried across as
   `(row, col)`.

Two mechanical constraints fall out:

- **A double-width character may not be split across a wrap point.** If only one
  column remains, the wrap happens before it, leaving one cell of padding.
- **Reflow must be idempotent.** Wrapping to width 80, then 40, then back to 80
  must reproduce the original layout exactly. If it does not, resizing a window
  back and forth degrades the buffer.

### 16.4 Live resize

**Decision: reflow the viewport eagerly and the scrollback lazily, and debounce
the whole thing across the gesture.**

Dragging a window corner does not produce one resize — macOS emits a stream of
them at roughly display refresh rate for the duration of the gesture. Two
separate costs scale with that event rate, and they are worth separating because
each has its own fix.

**Cost one: recomputing wrap points.** Proportional to cells scanned. Because
§16.1 makes display rows derived indices rather than copies, this is a scan
rather than a rebuild — cheaper than it first appears, but still linear in
buffer depth:

| Scrollback depth | Cells scanned per reflow | Order of magnitude |
|---|---|---|
| 1k lines | ~80k | sub-millisecond |
| 10k lines (a common default) | ~800k | a few milliseconds |
| 100k lines | ~8M | tens of milliseconds |

*(Reasoning about scale, not measurements — benchmark before trusting these.)*
The shape of the problem is that it degrades with accumulated history: it works
on the first day and gets worse over weeks of use.

**Cost two: the `SIGWINCH` storm.** This one is independent of reflow and is
easy to overlook. Forwarding every resize event to the PTY makes `vim` perform a
**full repaint per event** — generating a flood of output that must then be read,
parsed and rendered. Throttling this is necessary whatever the reflow strategy
is, which is what makes debouncing close to free: you need the mechanism anyway.

The three mitigations compose:

- **Debounce across the gesture.** AppKit exposes `inLiveResize`; defer
  `SIGWINCH` and full reflow to the end, keeping only the viewport current
  during the drag. This alone removes roughly two orders of magnitude of work,
  and solves the repaint storm.
- **Reflow the viewport eagerly, the scrollback lazily.** Only visible rows need
  correct layout for the next repaint; deeper history is rewrapped when scrolled
  into, or on an idle pass. This bounds the remaining cost by screen size rather
  than buffer depth, so it stays flat as history accumulates.
- **Cache wrap points per logical line**, invalidated on width change, so
  re-rendering at an unchanged width costs nothing.

### 16.5 The reflow worker — the v1 mechanism

The 100k scrollback ceiling (§17, #13) makes lazy scrollback reflow **v1-critical,
not optional**: a full-buffer reflow at that depth is tens of milliseconds, far
too much to run synchronously at gesture-end. So the three mitigations above are
realised by a concrete worker.

- **A dedicated Rust maintenance thread** owns background reflow — not the reader
  thread (a busy PTY would starve it exactly when history grows fastest) and not
  the main run-loop (that would leak engine work across the boundary, against §5).
  It parks when caught up and is woken on a width change.
- **Reflow is chunked and lock-bounded.** The worker takes the mutex, rewraps
  about a screenful of logical lines, releases, and repeats. No single lock-hold
  exceeds a frame budget, so it never stalls the reader thread or `drawRect:`.
  *This chunking — not the eager/lazy split — is what actually makes resize
  smooth;* an unchunked background pass janks just as hard as a synchronous one,
  because it holds the one mutex too long.
- **It always loses to interactive work.** Between chunks the worker sleeps
  briefly, so the reader and main threads reliably win the (non-fair) mutex.
  Background reflow of deep history therefore takes a second or two of wall-clock
  to *complete* — which is invisible, because what is on screen was already
  correct at gesture-end and never depended on the worker finishing.
- **Width changes mid-gesture restart cheaply.** The worker re-reads an atomic
  `target_width` at the top of each chunk; if it changed, it abandons and
  restarts. The per-line wrap-point cache (mitigation three above, keyed by
  width) turns the common back-and-forth — 80→120→80 — into cache hits rather
  than recomputation.

This worker is the second thread §7's shutdown sequence must signal and join
before `terminal_destroy`.

---

## 17. Decisions

### 17.1 Settled

| # | Decision | Resolution |
|---|---|---|
| 1 | Ownership / threading model | `Arc<Mutex<Emulator>>`, with the lock hidden behind the opaque handle. The native side sees a thread-safe API and no lock at all (§7). |
| 2 | Read path across the FFI | Copy into a caller-owned, reused buffer, once per frame. Follows from #1: no pointer into Rust memory may outlive the lock (§10-A). |
| 3 | Rendering data model | Runs over a UTF-8 buffer, not a cell array. A per-cell `u32` codepoint cannot hold combining marks or ZWJ sequences; runs can, and they match how Core Text shapes text (§10). |
| 4 | Native language | Objective-C++ for the bridge and `TerminalView` (§13). Reversible — the Rust side is identical either way. |
| 5 | Viewport ownership | Rust. The engine owns scroll position alongside scrollback and selection, so all three stay consistent and the frontend speaks only display coordinates (§16.2). |
| 6 | Reflow on resize | **Yes — wrapped lines reflow.** The consequential one: it dictates the buffer model and forces logical coordinates into existence (§16). |
| 7 | PTY location | Rust, alongside the engine, so the whole stack is testable headlessly. |
| 8 | Event notification | One `TerminalEvent` enum through a single callback (§4.4), so adding an event is a variant rather than a new FFI function. |

### 17.2 Opened by the reflow decision, and settled

Choosing reflow (#6) created a sub-branch, since each answer changes how
scrollback is stored.

| # | Decision | Resolution |
|---|---|---|
| 9 | Scrollback storage | **Logical lines**, with display rows as derived `(line_id, offset, len)` indices rather than owned copies (§16.1). Makes reflow a scan rather than a rebuild, and makes idempotent round-tripping across widths tractable. |
| 10 | Reflow eagerness | **Viewport eagerly, scrollback lazily**, so cost is bounded by screen size rather than buffer depth and stays flat as history accumulates (§16.4). **v1-critical**, not deferrable — the 100k ceiling (#13) rules out an eager full-buffer reflow at gesture-end; realised by the maintenance worker in §16.5. |
| 11 | Live-resize strategy | **Debounce across the gesture** via `inLiveResize`, deferring both full reflow and `SIGWINCH` to the end (§16.4). |
| 12 | Line identity | **A monotonic counter**, never an array index — eviction would otherwise shift every index and silently invalidate stored anchors (§16.2). |

These are mutually reinforcing rather than independent: logical-line storage is
what makes lazy reflow and stable line ids straightforward, and debouncing is
what makes the whole approach survive a live resize gesture. Answering them
differently in isolation would produce a design that fights itself.

### 17.3 Opened as "genuinely open," now settled

These three were left open in an earlier draft. All are now resolved.

| # | Decision | Resolution |
|---|---|---|
| 13 | Scrollback depth limit | **Bounded and configurable: default 10k lines, hard maximum 100k.** No "unlimited" in v1. The 100k ceiling is a deliberate, load-bearing choice — it is what makes lazy reflow (#10) v1-critical rather than optional (§16.5). Memory at the cap is ~20–100 MB depending on how coloured the history is. |
| 14 | Does selection survive reflow? | **Cleared on width-change reflow in v1.** Logical anchors are kept regardless — scrolling, appended output and eviction all need them — so this is specifically the width-change case. Clearing avoids a surprise synchronous reflow (a select-all across 100k lines would otherwise hitch) and costs nothing; "survive small selections, guard the pathological case" is a contained later upgrade (§16.3). |
| 15 | Mouse reporting mode | **Implementation is post-MVP, but the v1 input FFI is built to enable it.** Because selection state already lives in Rust (#5), the frontend already reports pointer gestures across the boundary. v1 uses the *general* shape — `terminal_pointer_event(phase, row, col, button, modifiers)` — so when mouse mode lands, the engine routes the *same* input to PTY-byte encoding instead of selection, mirroring key encoding (§8). A `TerminalEvent` variant for mode-change is reserved (free, per #8). Known frontend follow-ups: the Shift/Option-forces-local-selection override and a cursor-shape change. |

The narrow alternative for #15 — `terminal_set_selection(start, end)` with the
frontend tracking the drag — was rejected: it saves nothing meaningful in v1 and
would force a second, parallel input path for mouse reporting later, because the
button, phase and modifiers would never cross the boundary.

### 17.4 Consequences that rippled back into "settled" sections

Resolving §17.3 changed three things that had been treated as closed:

- **#10 (reflow eagerness) is now v1-critical, not deferrable.** The 100k ceiling
  (#13) removed the option of a single eager full-buffer reflow at gesture-end.
  The concrete worker is specified in §16.5.
- **§7 shutdown now signals and joins two threads** — the PTY reader *and* the
  reflow maintenance thread — before `terminal_destroy`.
- **The FFI input surface gains `terminal_pointer_event`** in the general shape
  (#15), which becomes the single path for both selection (v1) and mouse
  reporting (post-MVP).

### 17.5 Storage model, resolved

Decision #9 said "logical lines with derived `(line_id, offset, len)` rows" but
left the mutation model, representation and mechanics open. A grilling session
resolved them; full rationale is in `docs/adrs/2026-08-27.adr-logical-line-buffer-model.md`.

| # | Decision | Resolution |
|---|---|---|
| 16 | Mutation surface | **Hybrid.** The active screen is an owned, cell-addressable grid; scrollback is logical lines. `(line_id, offset, len)` is the scrollback/reflow representation, not the live screen's. Refines §16.1. |
| 17 | Line identity lifecycle | **`line_id` born with the logical line**, tracked through the active grid (each row tagged with owning `line_id` + soft-`wrapped`), persisted into scrollback — so on-screen selection anchors survive reflow. |
| 18 | Scrollback representation | **Packed UTF-8 text + attribute runs** (`{byte_start, byte_len, cols, fg, bg, attrs}`), immutable, converted from grid rows on eviction. Hits the low end of #13's budget and is already the §10 render-run shape. |
| 19 | Active-grid cell content | **`compact_str::CompactString`**, not heap `String` — UTF-8, but no per-cell allocation for real clusters. Reverses the earlier `String` choice. |
| 20 | Internal offset unit | **Byte offsets into the packed UTF-8, grapheme-aligned**, for both triples and anchors; §16.2's `char_offset` becomes a byte offset. Columns are computed on demand, never stored. |
| 21 | Primary-buffer type | **A dedicated `Screen` type** (row-oriented, per-row `line_id`/`wrapped`, owns cursor + scrollback). The dumb `Grid` stays the scratch rectangle. *Amended 2026-08-28:* the alternate screen is a second `Screen` buffer swapped in, not a `Grid` — that way it inherits the whole write path (wide characters, combining marks, margins, line editing) instead of needing it reimplemented. See the alternate-screen ADR. |
| 22 | Scrollback cap | **Dual: logical-line count (default 10k, max 100k) + total-bytes safety cap.** Whole oldest lines evicted from a `VecDeque`; never split or truncated. A count cap alone is not a memory bound because a logical line is unbounded. |
| 23 | Wrap-point cache | **Per-line `{width, row_starts}`, `row_starts` stored inline**, invalidated by comparison to a single global width (O(1) resize), filled lazily + by the §16.5 thread, scrollback only. This is the concrete lazy-reflow mechanism. |
| 24 | Boundary straddle | **Allow split, rejoin by `line_id`.** Rows freeze incrementally as they scroll off (active grid stays a clean rectangle); at most one line is split head/tail and reconstructed by concatenation at reflow. Frozen head byte-length tracked for split-line anchors. |
| 25 | Grapheme width authority | **`unicode-width` + `unicode-segmentation` behind one shared `grapheme_width` / segmenter.** The write path and reflow scan must agree or the display desyncs; single-source-of-truth is the correctness invariant. Ambiguous width = 1, configurable post-MVP. |

New dependencies this introduces: `compact_str`, `unicode-width`,
`unicode-segmentation`.

---

## 18. Glossary

| Term | Meaning |
|---|---|
| **ABI** | Application Binary Interface — the machine-level contract: how arguments are passed, how structs are laid out, how names are mangled. Distinct from an API, which is source-level. |
| **C ABI** | The platform's C calling convention. The lingua franca of §3. |
| **Opaque handle** | A pointer to a type the other language is told exists but never shown the contents of. |
| **`#[repr(C)]`** | Rust attribute opting a type into C's layout rules, making it safe to share. |
| **`extern "C"`** | Marks a Rust function as using the C calling convention so C can call it. |
| **`#[no_mangle]`** | Keeps Rust from renaming the symbol, so the linker can find it by its literal name. |
| **cbindgen** | Generates a C header from Rust source. |
| **staticlib** | A `.a` archive linked directly into the final binary. |
| **Damage** | The record of which rows changed since the last draw. |
| **PTY** | Pseudo-terminal: the kernel device pair that makes a program believe it is talking to a terminal. |
| **DECCKM / DECKPAM** | DEC private modes that change what arrow keys and the keypad emit — why key encoding is engine state (§5). |
| **Marked text** | Provisional, uncommitted text shown during IME composition (§8). |

---

## 19. Development workflow

This section describes *how* the engine is built, as distinct from *what* it is.
It is placed last because it is process rather than contract, but it is not
optional: the discipline here is what keeps the contract above trustworthy.

**Engine-first, and headless.** All terminal semantics live in the
`terminal-core` crate — pure Rust, no platform dependency, unit-testable without
a Mac. This is the guiding principle restated operationally: if a new piece of
work cannot be exercised by a `cargo test` on Linux, it has leaked across the
boundary (§2, and the closing principle below) and belongs on the native side
instead.

**Small increments.** Build one cohesive primitive at a time — its types and its
tests together — then check in. Decisions are expected to be revisited, so a
smaller step is cheaper to unwind than a large one. Volume is never generated
ahead of a working, tested foundation.

**Load-bearing decisions are designed before they are coded.** Anything with wide
blast radius — the buffer model (§16), threading (§7), the read path (§10) — is
settled first, stress-tested by argument where it helps, and recorded in §17.
These are chosen deliberately, not discovered midway through an implementation.

**The per-increment verification loop.** Before any increment is committed, and
in this order:

1. `cargo fmt` — formatting is not a matter of taste to be re-litigated per file.
2. `cargo clippy --all-targets` — treat every warning as a defect to fix, not to
   silence.
3. `cargo test` — the whole suite, every time.
4. **Fix every issue** the three steps surface before proceeding. The tree stays
   green and fmt-clean at every commit; a warning is never carried forward.

**Summarise every change and learning back into this document.** The PRD is the
single source of truth (§1), so each increment that changes a decision, adds a
constraint, or teaches something non-obvious updates the affected section, records
new decisions in §17, and notes any ripples. A design that lives only in a commit
message or a conversation is a design that has already started to rot.

**One commit per increment**, with a conventional message and a `Co-Authored-By`
trailer, so the history reads as a sequence of self-contained, verified steps.

**Crate layout.** `terminal-core` is the pure engine: no platform dependency,
testable with nothing but a compiler. `terminal-pty` (added 2026-08-28) holds
the pseudo-terminal, the child process and the reader thread — Rust's by §5, but
POSIX, so kept out of the engine; it depends on `terminal-core` and never the
reverse. The FFI layer — the `staticlib` plus the cbindgen-generated header of
§14 — becomes a separate `terminal-ffi` crate when the boundary is first
crossed, so that the engine crate never gains a build step or a dependency that
would compromise its headless testability.

---

## Guiding principle

> A Rust terminal emulator with a native macOS frontend — not a macOS terminal
> application implemented in Rust.

The test to apply to any future design question: **if the engine ever needs a Mac
in order to be tested, something has leaked across the boundary.**
