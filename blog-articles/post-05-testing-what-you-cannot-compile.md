# Testing What You Cannot Compile

Most of this macOS application was written on Linux, by something that has never
seen a Mac.

That constraint turned out to be the most useful design pressure in the project.
Not because working blind is good — it is not — but because "I cannot run this"
forces a question that is easy to avoid when you can: *which parts of this code
actually need the platform, and which parts merely live near it?*

The answer, for a GUI application, is a much smaller number than you would guess.

This is the fifth article about building a terminal emulator. The
[first four](post-01-designing-the-architecture-of-a-tty-with-appkit-and-rust.md)
covered the architecture, the storage, Unicode, and the C boundary. This one is
about verification, and it is the article I would most want to have read first.

---

## The rule

The frontend is split by directory, and the split is the whole strategy:

```
   native/macos/
     Glue/       plain C++17. No AppKit, no Objective-C, no Core Text.
     Sources/    Objective-C++. AppKit and Core Text only.
```

With one rule enforcing it:

> **A file in `Sources/` may not contain an `if` that matters.**

If a branch is worth getting right, it belongs in `Glue`, which compiles on Linux
and is tested there. What is left in `Sources` is translation: an `NSEvent` becomes
a struct, a struct becomes a `CGRect`, a callback becomes a `dispatch_async`.

The proportions are the interesting part:

```
   Glue/       1367 lines   ████████████   76 tests, all on Linux
   Sources/     808 lines   ███████        11 XCTest cases, Mac only
```

Eight hundred lines of Objective-C++ in a GUI application, and they decide almost
nothing.

## What actually needs a Mac

Go through what a terminal frontend does and ask, honestly, what the platform is
required for:

| Job | Needs a Mac? | Why |
|---|---|---|
| Deciding a keypress means `Ctrl+C` → `0x03` | **No** | Arithmetic over integers |
| Resolving palette index 9 to RGB | **No** | Arithmetic over integers |
| Working out how many rows fit in a window | **No** | Two divisions |
| Turning a run into a rectangle | **No** | Multiplication |
| Growing a buffer when a frame outgrew it | **No** | The C API, callable anywhere |
| Owning the handle, destroying it once | **No** | A destructor |
| Parsing the config file | **No** | Text |
| Measuring a font | **Yes** | Core Text owns the font |
| Drawing glyphs | **Yes** | Core Graphics owns the pixels |
| Knowing which key was pressed | **Yes** | `NSEvent` |

The first seven are the ones that break. They are pure functions of their inputs,
they have edge cases, and they are exactly what unit tests are for. The last three
have almost no logic in them — and they are the ones that need the machine.

So `Glue` contains eight units, and every one is tested on a machine with no
screen:

```
   KeyMap        (keyCode, modifierFlags, characters) -> a key event, or "this is text"
   Palette       packed colour -> RGBA, with reverse, dim, bold and hidden
   Metrics       view size + cell size -> rows/cols, run rects, baselines
   FrameBuffers  the two-call copy protocol, regrowing when a frame outgrows it
   Session       RAII over the handle
   Config        validating a config file: clamping, fallbacks, diagnostics
   ConfigFile    parsing it
   Diagnostics   what to show when the shell exits, and whether to close the window
```

## Tested against the real thing

The trick that makes this more than a mock exercise: **the C++ tests link the real
static library.**

```sh
c++ -std=c++17 Tests/glue_tests.cpp Glue/*.cpp \
    target/debug/libterminal_ffi.a $(cargo rustc -- --print native-static-libs)
```

So `FrameBuffers` is not tested against a pretend engine. It calls
`terminal_create`, spawns an actual `/bin/sh`, waits for output, copies real
frames, and asserts on real runs. `Session`'s destructor really does tear down a
real pty. On Linux, in a container, in a hundred milliseconds.

The mocks that would otherwise be necessary — and that would encode my assumptions
about the boundary rather than the boundary's behaviour — do not exist.

## Two bugs that prove the split works

### The one Linux caught for macOS

The frame copy protocol has a subtle case: the first call has empty buffers, gets
`BufferTooSmall`, grows, and retries. What if the screen is blank — no runs, no
text? Zero capacity, zero required. Does that succeed, or does it report that a
zero-length buffer is too small and loop forever?

```cpp
TEST(a_blank_screen_copies_cleanly_from_empty_buffers) {
    Session session = shell("sleep 30");
    FrameBuffers frame;
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    CHECK_EQ(frame.run_count(), 0u);
}
```

That test runs in the container. The code it protects runs on a Mac, in
`drawRect:`, on the very first frame before the shell has printed anything — which
is to say, every single launch. It was verified before the code had ever been
compiled by a Mac compiler.

### The one only a Mac could catch

The first successful launch drew a window, the right background, a cursor in the
right place — and no text at all.

Nothing was broken. Core Text's `CTFontDrawGlyphs` paints with the graphics
context's current fill colour, and the last thing to set that was the window
background. The prompt was on screen, parsed correctly, laid out correctly,
painted `#1E1E1E` on `#1E1E1E`.

No unit test would have caught it. `Metrics` computed the right positions.
`Palette` resolved the right colours. The engine produced the right runs. Every
tested component was correct, and the composition was invisible.

**The tell was the cursor.** It sat about a third of the way across the first
row — exactly where a `zsh` prompt ends. Which meant the bytes had arrived, the
parser had understood them, the screen had placed them, and the cursor had advanced
over them. Every layer had done its job; only the paint was wrong.

That is what the tested layers buy you. Not the absence of bugs — the ability to
read one symptom and know which of eight things it cannot be.

## Reproducing a failure you cannot observe

The best moment came from a bug report that was four lines long:

```
test tests::a_frame_buffer_that_is_too_small... FAILED
test tests::a_resize_crosses_and_is_validated ... FAILED
test tests::a_null_out_parameter_is_caught_too ... FAILED
test result: FAILED. 21 passed; 3 failed
```

Three failures on a Mac. All of them passed in the container. No error messages —
the panic hook bug from the [previous article](post-04-the-c-boundary.md) was
eating them.

The way in was to read the tests rather than the output. `a_null_out_parameter_is_caught_too`
does nothing platform-specific: it spawns a shell, then passes null pointers to two
functions and checks they return `NullHandle`. There is exactly one line in it that
can fail on one machine and not another — the assertion that the shell started.

So `terminal_create` was returning null. Intermittently, in a subset of tests, on a
machine with more cores than the container. That shape — arbitrary tests failing,
including one with no platform-specific code — is resource exhaustion, not logic.

Each session holds four file descriptors: the pty master, the reader thread's
duplicate, and both ends of the interrupt pipe. Cargo runs one test per core.
Linux allows 1024 descriptors by default; macOS has historically allowed 256.

Which is testable **without a Mac**:

```sh
$ (ulimit -n 64; ./terminal_ffi_tests --test-threads=16)

the shell should have started: terminal_create: Too many open files (os error 24)
test result: FAILED. 22 passed; 2 failed
```

The same failure, the same shape, an arbitrary subset each run. The cause was
confirmed on the machine that could not observe the symptom, by making the
container resemble the Mac in the one dimension that mattered.

The fix was to stop the suite depending on the ambient limit at all — capping test
threads in `.cargo/config.toml` — and it was verified the same way: the whole
workspace now passes at `ulimit -n 32`, where it previously failed at 64.

**A test suite that only passes when the environment is generous is a test suite
that fails on someone else's machine.** The Mac just found it first.

## What the Mac-only tests are for

Eleven XCTest cases, and they are deliberately unambitious:

```objc
- (void)testViewComputesAGridFromARealFont;
- (void)testZoomChangesThePreferredSize;
- (void)testAViewWithNoSessionDrawsWithoutCrashing;
- (void)testAShellDrawsItsOutput;
- (void)testResizingReachesTheEngine;
```

Construct, measure with a real font, draw into a bitmap, resize, tear down. No
assertions about pixels — a test that asserts pixels fails when the system font
changes, which is not a signal anyone wants. These check that the AppKit objects
can be assembled and driven without falling over. The decisions they carry out were
tested elsewhere.

And then a third tier, which is a checklist in a README, because pretending
otherwise would be dishonest:

```
   4. `vim`, then `:q` -- full-screen redraw, and the screen comes back
   9. `kill -9 $$` -- the window stays open so you can read the screen
```

Some things need eyes. Writing that down is what makes it happen reliably instead
of being remembered differently each time.

## The generalisation

This project had an unusual constraint, but the technique is not unusual at all.
Substitute your own version of "cannot compile it here":

- a mobile app where the simulator is slow and the device is slower
- an embedded target with a five-minute flash cycle
- anything behind a GPU, a camera, a payment processor, a physical sensor

The move is the same. **Push every decision into code that runs where iteration is
cheap, and leave only translation in the part that does not.** The measure of
success is not test count. It is: when something looks wrong on the machine you
cannot iterate on, how much of the system can you *rule out* before you start
guessing?

Here the answer was eight units and a boundary — and the invisible-text bug took
one glance at where the cursor was sitting.

---

*Next in this series:
[Two Threads and a Flag](post-06-two-threads-and-a-flag.md) — one lock, a
coalesced wake-up, and a shutdown sequence that cannot leave a thread writing
into memory that has gone.*
