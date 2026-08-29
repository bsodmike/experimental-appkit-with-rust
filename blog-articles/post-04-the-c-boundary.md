# The C Boundary: Thirteen Functions Wide

Two languages have to meet somewhere. In this project it is a Rust engine and an
AppKit frontend, and the place they meet is a C ABI — because C is the only thing
both of them speak without a translator, and because the alternative is a
generated bridge that hides exactly the decisions you most need to see.

The interesting part is not that it works. It is how narrow it had to be, and what
kept trying to make it wider.

This is the fourth article about building a terminal emulator. The
[first](post-01-designing-the-architecture-of-a-tty-with-appkit-and-rust.md)
covered the architecture, the [second](post-02-the-buffer-model-why-scrollback-cannot-be-a-grid.md)
the storage, the [third](post-03-unicode-in-a-grid.md) what goes in a cell. This
one is the waist of the hourglass.

---

## Four shapes, and no fifth

Everything that crosses is one of exactly four things. The rule is worth stating
as a rule, because the moment you allow a fifth you have started designing a
second language binding rather than a boundary.

**An opaque handle.** A pointer to something Rust owns, which C stores and hands
back, and never dereferences:

```c
typedef struct TerminalSession TerminalSession;   // never defined in C

TerminalSession *terminal_create(const TerminalConfig *config);
void             terminal_destroy(TerminalSession *session);
```

C is told the type exists and never told what is inside it. That is the point: the
engine can be restructured entirely without the header changing, because the header
never described the engine in the first place.

**Plain data.** Fixed-layout structs of fixed-width integers, copied by value:

```c
typedef struct TerminalRun {
    uint32_t utf8_offset;
    uint32_t utf8_len;
    uint32_t fg;
    uint32_t bg;
    uint16_t row;
    uint16_t col;
    uint16_t cols;
    uint16_t attrs;
} TerminalRun;
```

Note what is not there: no `size_t`, no `usize`, nothing whose width depends on the
target. And the field order is not alphabetical or logical — it is chosen so the
struct packs to 24 bytes with no padding, because these cross by the arrayful.

**Byte buffers.** A pointer and an explicit length. Never NUL-terminated, because
terminal data legitimately contains NUL bytes, and never a Rust `String`.

**One callback**, with a context pointer. That `void *ctx` is easy to leave out and
impossible to work without: a bare C function pointer carries no state, so the
context is how the callback finds its way back to the right view.

## The API that looks right and is unsound

Here is the natural first sketch of the read path — the frontend needs the visible
screen, and Rust has it:

```c
const TerminalCell *terminal_get_cells(TerminalSession *s);   // unsound
```

Return a pointer into the grid. Zero copies, obviously fast.

It is a bug, and the reason is threading. A terminal has a reader thread parsing
shell output continuously. The moment that function returns, the lock is released
and the reader is free to mutate — or reallocate — the very memory the frontend is
now reading.

The pointer is valid until the next byte arrives from the shell. **There is no
lifetime you can write in C that expresses that.** Not "until you call the next
function", not "for this frame" — the frontend has no way to know when the
invalidating event happens, because the invalidating event is a program on the
other side of a pipe printing something.

You cannot make this safe with documentation. You can only stop returning the
pointer.

## Copy into buffers the caller owns

So the frame is copied, into memory the frontend allocated:

```c
typedef struct TerminalFrameBuffers {
    struct TerminalRun *runs;
    uint32_t runs_cap;
    uint8_t *text;
    uint32_t text_cap;
} TerminalFrameBuffers;

TerminalStatus terminal_copy_frame(TerminalSession *session,
                                   const TerminalFrameBuffers *buffers,
                                   TerminalFrameInfo *info);
```

Three properties follow, and each one removes a category of bug rather than
mitigating it:

**There is no second lifetime.** The frontend owns the memory. It was already
managing it. Nothing new can dangle.

**There is nothing to free.** No `terminal_free_frame()` for someone to forget in
an error path — the classic leak in every hand-written C API.

**The frame cannot tear.** One call takes the lock once, copies everything, and
releases. A frame is internally consistent by construction: row 3 and row 40 are
from the same instant, rather than from either side of a write that happened
between them.

And the steady state allocates nothing on either side. The frontend keeps its two
buffers between frames; Rust reuses one `Frame` behind the handle.

### Sizing without allocating

The buffers have to be big enough, and the caller cannot know how big before
asking. The usual answers are to allocate-and-return (a lifetime, and a free
function) or to guess.

The third answer is to make the sizing call and the copying call the *same
function*:

```c
TerminalFrameInfo info;
terminal_copy_frame(session, NULL, &info);   /* -> BufferTooSmall, info filled */

runs = calloc(info.runs_len, sizeof(TerminalRun));
text = calloc(info.text_len, 1);

terminal_copy_frame(session, &buffers, &info);   /* -> Ok */
```

`info` is filled in **even when the copy fails**, so a failure tells you exactly
what would have succeeded. The same shape does window titles, and would do
selected text. It is the pattern to reach for whenever a C API is about to grow an
allocator.

## Errors, and the panic that would kill your session

Every fallible function returns a status enum with an explicit `int32_t`
representation. Not `Result`, not `Option` — neither has a defined C layout.
Out-parameters carry results; the return value carries status.

Null is always checked, everywhere, and returns a status rather than crashing. A
frontend bug should produce a wrong pixel, not a dead process.

Then the rule that is easy to skip and expensive to skip:

```rust
fn guard(f: impl FnOnce() -> TerminalStatus) -> TerminalStatus {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(payload) => {
            set_last_error(describe_panic(&payload));
            TerminalStatus::Panicked
        }
    }
}
```

**A Rust panic must never unwind into Objective-C.** Since Rust 1.81, unwinding out
of an `extern "C"` function aborts the process rather than being undefined
behaviour — which is a genuine safety net and not a solution, because aborting
still kills the user's terminal and everything running in it.

So every entry point catches. An index-out-of-bounds deep in the parser becomes a
status code, the window keeps drawing the last good frame, and the session
survives.

There is a subtlety I got wrong here and had to fix. The panic message is worth
keeping — "Panicked" alone is the least debuggable state an application can reach —
so a panic hook records the payload and location. I installed one that recorded
silently, reasoning that a terminal's diagnostics have no business on stderr.

A panic hook is process-wide. The silent one replaced the hook the test harness
installs to capture failure messages, and every failing test in the workspace
started reporting `FAILED` with no reason attached. The fix is one word — *chain*
to the previous hook rather than replace it — and the lesson is that a library
which sets a global on behalf of the whole process should assume something else is
already there.

## Generate the header, do not write it

The C header is produced from the Rust source by cbindgen, as a build step.

A hand-maintained header is a silent-corruption hazard. If Rust says `u32` and the
header says `uint16_t`, nothing warns you. You get garbage arguments or a smashed
stack, at runtime, intermittently, and the file that is wrong is not the file you
are looking at. It *will* drift, because it is edited separately from the
signature it describes.

That decision paid for itself on the first run, in a way I did not expect. This is
idiomatic Rust for an optional callback:

```rust
pub type TerminalWakeUpFn = extern "C" fn(ctx: *mut c_void);

pub struct TerminalConfig {
    pub wake_up: Option<TerminalWakeUpFn>,   // null means "none"
}
```

`Option<fn>` is a null-checked function pointer with no overhead — the standard
way to express this. cbindgen rendered it as:

```c
typedef struct Option_TerminalWakeUpFn Option_TerminalWakeUpFn;   /* opaque! */
```

An opaque struct. C could not have constructed one, could not have assigned a
function to it, could not have used the callback at all. Spelling the field out as
`Option<extern "C" fn(ctx: *mut c_void)>` produces `void (*wake_up)(void *ctx)`,
which is what was meant.

Had the header been hand-written it would have said the correct thing while the
library expected something else — and the failure would have arrived as a corrupted
call at runtime rather than as a header that looks wrong when you read it.

## Testing a boundary from both sides

The Rust side of an FFI is easy to test: it is Rust. The temptation is to stop
there, and it leaves the actual boundary — the layout, the calling convention, the
linkage — completely unexercised.

So there is a C program:

```c
TerminalSession *session = terminal_create(&config);

/* wait for the callback to fire, then size and copy a frame */
terminal_copy_frame(session, NULL, &info);
runs = calloc(info.runs_len, sizeof(TerminalRun));
...
terminal_copy_frame(session, &buffers, &info);

for (uint32_t i = 0; i < info.runs_len; i++) {
    printf("row %u col %u  \"%.*s\"\n", runs[i].row, runs[i].col,
           (int)runs[i].utf8_len, text + runs[i].utf8_offset);
}
terminal_destroy(session);
```

Compiled with a C compiler, linked against the real static library, driving a real
shell. It runs on Linux, where there is no Xcode, no AppKit and no Objective-C —
and it exercises every property that matters: the header compiles as C, the layouts
agree, the callback fires across a thread boundary, the copy-out protocol works, and
the handle can be destroyed without the process complaining.

The link flags come from the compiler rather than from guesswork:

```sh
cargo rustc -p terminal-ffi --lib -- --print native-static-libs
```

Rust's standard library pulls in system libraries, and that command prints exactly
which. Guessing wastes an afternoon; the compiler will simply tell you.

## What the narrowness bought

Thirteen functions. Create, destroy, send text, send a key, paste, resize, copy a
frame, copy the title, ask how the child exited, read the last error, clear it,
start logging.

The engine behind them is six thousand lines and has been restructured repeatedly
— the buffer model changed shape, the alternate screen arrived, the parser grew
scroll regions and modes and replies. The header changed when the *API* changed,
which is a different and much rarer event.

That is what a narrow waist is for. Not elegance — **decoupling of change**. The
two halves of this project have different languages, different testing stories,
different failure modes and different rates of change, and thirteen functions is
the entire surface over which they can affect each other.

---

*Next in this series: testing what you cannot compile — how most of a macOS
frontend gets verified on Linux, and how a bug that only appeared on a Mac was
reproduced in a container.*
