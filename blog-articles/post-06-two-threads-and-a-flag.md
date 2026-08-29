# Two Threads and a Flag

A terminal emulator has a producer you do not control, writing at a rate you
cannot predict, into a display that must stay responsive. `cat` a large file and
a naive implementation will freeze — not from doing too much work, but from
scheduling it badly.

The concurrency in this project is small: two threads, one mutex, one atomic
boolean. Each of those is small because the problem was narrowed first. This is
the article about that narrowing.

It is the sixth and last in this series. The
[first five](post-01-designing-the-architecture-of-a-tty-with-appkit-and-rust.md)
covered the architecture, the storage, Unicode, the C boundary, and testing.

---

## Two threads, and why not more

There is exactly one file descriptor to watch — the pty master. Nothing to
multiplex, nothing to select over, no second source of events. An async runtime
would contribute a scheduler, a dependency, and a set of lifetime puzzles in
exchange for nothing.

So: **one thread, blocking on `read()`.**

```
   ┌────────────────────────┐        ┌───────────────────────────┐
   │  AppKit main thread    │        │  pty reader thread        │
   │                        │        │                           │
   │  drawRect: ───────┐    │        │   read() ────────┐        │
   │  keyDown:  ───────┤    │        │                  │        │
   │  resize    ───────┤    │        │   parse into ────┤        │
   └───────────────────┼────┘        │   the screen     │        │
                       │             └──────────────────┼────────┘
                       │                                │
                       └────────► Mutex<Emulator> ◄─────┘
                                          │
                                          │ wake-up, coalesced
                                          ▼
                                  dispatch_async(main) -> redraw
```

The decision inside that diagram which is easy to get wrong: **the reader thread
parses.** It does not read bytes and hand them to the main thread to deal with
later. It takes the lock, runs the parser, mutates the screen, and releases.

The main thread must never parse, because parsing is unbounded work driven by a
hostile-by-accident data source. Any program can print a megabyte. If that
megabyte is parsed on the thread that also draws your window, your window stops
drawing for as long as it takes.

## The flag that stops it falling over

Here is the failure mode that this design exists to prevent, and it is the one
almost every first attempt hits.

`cat` of a large file produces thousands of small reads. The obvious thing is to
ask for a redraw after each one:

```
   read 4KB  ->  parse  ->  dispatch_async(main, redraw)
   read 4KB  ->  parse  ->  dispatch_async(main, redraw)
   read 4KB  ->  parse  ->  dispatch_async(main, redraw)
   ... a few thousand more
```

Every one of those enqueues a block on the main queue. The main thread then spends
its time draining a backlog of redundant redraw requests — each of which draws a
screen that is already stale by the time it runs. The UI goes unresponsive, and the
app appears to hang under precisely the workload it should handle best.

The fix costs one atomic boolean:

```rust
fn mark_dirty(&self) {
    if !self.shared.dirty.swap(true, Ordering::AcqRel) && self.is_running() {
        (self.shared.wake_up)();
    }
}
```

Only the `false -> true` transition calls the wake-up. Every subsequent chunk sets
a flag that is already set, and posts nothing. One redraw is ever pending; the
main thread always draws the *latest* state rather than a queue of old ones; and
the reader absorbs output at full speed while the UI repaints at whatever rate it
can sustain.

Two thousand reads, one redraw. In the engine's own trace it looks like this,
which is the clearest possible statement of the whole idea:

```
   INFO feed{seq=1}: read from the pty bytes=4096
   INFO feed{seq=1}: waking the ui              <- the only one
   INFO feed{seq=2}: read from the pty bytes=4096
   INFO feed{seq=3}: read from the pty bytes=4096
   ...
   INFO feed{seq=812}: read from the pty bytes=1131
   INFO render: frame copied for the ui runs=24 text=1840
```

## Clearing the flag before the copy, not after

The other side of that flag is a race, and it has a correct answer and a plausible
wrong one.

```rust
pub fn render_into(&self, frame: &mut Frame) {
    let was_dirty = self.shared.dirty.swap(false, Ordering::AcqRel);
    self.lock().screen.render_into(frame);
    ...
}
```

Clear **before** copying. Consider a write that lands between the clear and the
copy: the flag goes back to true, and the next redraw copies a frame that is
already up to date. One redundant repaint.

Now clear **after** copying, which reads more naturally — take the frame, then
mark it clean. The same write is now silently discarded: it happened after the
copy, and clearing afterwards erases the record that it happened at all. That text
does not appear until something else happens to set the flag, which might be the
next keystroke, or might be never.

The two orderings differ by one wasted repaint versus lost output that arrives
minutes late or not at all. Not a close call — but it is the sort of thing that
looks like a stylistic choice at three in the morning.

## The lock is small on purpose

The mutex protects the screen and the parser together, because the parser is
stateful across reads — a chunk can end mid-escape-sequence or mid-UTF-8 — and
must stay in step with the screen it drives.

It is held for microseconds: one chunk of parsing, or one frame's worth of
copying. And it is **never held across the wake-up callback**:

```rust
let replies = {
    let mut emulator = self.lock();
    screen.advance(parser, bytes)
};                       // lock released here
self.mark_dirty();       // and only then may the callback run
```

That callback crosses into Objective-C. A callback that re-entered the terminal
API while the lock was held would deadlock on a non-reentrant mutex — and it would
not be an exotic scenario, because a frontend that redraws synchronously in
response to a wake-up is doing an obvious thing.

The frontend's callback therefore has exactly one job:

```objc
static void CrusttyWakeUp(void *ctx) {
    TerminalView *view = (__bridge TerminalView *)ctx;
    dispatch_async(dispatch_get_main_queue(), ^{
        [view setNeedsDisplay:YES];
    });
}
```

Not drawing. Not asking the engine anything. Marking a view dirty and returning.
The run loop coalesces those marks too, so there are two layers of coalescing —
one in the engine, one in AppKit — and neither has to know about the other.

## Poisoning, and a terminal that keeps drawing

If a thread panics while holding a Rust mutex, the lock is poisoned and every
subsequent `lock()` returns an error. The default is to propagate that, which for
this application means: one bug in the parser and the terminal stops drawing
forever.

```rust
fn lock(&self) -> MutexGuard<'_, Emulator> {
    self.shared.emulator.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
```

Recovered rather than propagated. The screen may be inconsistent — a row half
written — but a terminal drawing one damaged row beats a terminal that has stopped,
taking your session with it. Combined with the panic guards at the FFI boundary,
the failure mode for an engine bug becomes "one frame looks wrong" instead of "the
application is gone".

## Stopping is harder than starting

Destroying a session must be ordered, or a background thread outlives the memory it
is writing into. The sequence:

```
   1. silence the wake-up      no callback can be in flight into a view
   2. fire the interrupt       the reader leaves its poll()
   3. join the reader          it is definitely not running
   4. hang up the child        SIGHUP, then SIGKILL after a grace period
   5. only now, free
```

Step two is the one with a real problem in it. A thread blocked in `read()` cannot
be asked to stop by a flag it never looks at. You could close the descriptor
underneath it — and race with descriptor reuse, so that the thread wakes up reading
whatever file got that number next.

The answer is to wait on **two** descriptors: the pty, and a pipe that nobody ever
writes to except at shutdown.

```
   reader thread:
     poll([pty_master, interrupt_pipe])
       │
       ├── pty readable        -> read, parse, reply, mark dirty
       ├── interrupt readable  -> return; we are being joined
       └── pty hung up         -> drain what is left, then stop
```

The pipe is fired once and stays readable forever, which matters: a reader that has
not reached its `poll` yet still sees it when it gets there. No timing window, no
missed signal.

And the child gets `SIGHUP` first, because that is what a terminal closing *means* —
a shell knows how to handle it and hangs up its own children in turn. A child that
ignores it is killed after a grace period, because a process that traps `SIGHUP`
must not be able to keep the pty alive, and therefore the reader thread alive, and
therefore the whole shutdown blocked.

`Drop` runs the same sequence, so a caller who forgets still cannot leave a thread
writing into freed memory.

## Parsing has an output side

One thing that surprises people, and that a naive design discovers the hard way.

`CSI 6 n` asks the terminal where its cursor is. The program **blocks** waiting for
`ESC [ row ; col R` to come back. `CSI c` — "what kind of terminal are you" — is
the same. So parsing is not a pure sink: it produces bytes that must be written
back up the pty.

Forget it, and programs mysteriously hang rather than failing loudly, which makes
it an expensive omission to debug months later.

So the API returns the bytes rather than stashing them:

```rust
#[must_use = "these bytes are owed to the program: write them to the PTY"]
pub fn feed(&self, bytes: &[u8]) -> Vec<u8>
```

`#[must_use]` makes ignoring the reply something a caller does on purpose. The
reader thread — which owns the descriptor — writes them straight back.

The queue behind that is capped, and dropping the overflow is deliberate: a program
can ask faster than anyone drains, and an unbounded queue would be a memory leak
driven entirely by untrusted output.

## The shape of it

Two threads, one mutex, one atomic boolean, one pipe. That is the entire
concurrency surface of a program handling an unbounded, adversarial input stream
while keeping a GUI responsive.

It is small because the problem was made small first — one descriptor, so one
thread; parse where the data arrives, so the UI never blocks; copy under one lock,
so no frame can tear. Every piece of machinery that is *not* there is absent
because of a decision made earlier, not because it was forgotten.

Which is the theme of this entire series, and the note to end on. The buffer model
made reflow cheap. The width authority made the display consistent. The narrow C
waist made the two halves independently changeable. The `Glue`/`Sources` split made
a GUI testable without a screen. And here, one flag makes an unbounded producer
harmless to a UI thread.

None of those are clever code. They are all the same move: **decide the shape
before writing the thing, and most of the difficulty never arrives.**

---

*That is the series. The code is on [GitHub](https://github.com/); the design
documents that these articles are drawn from — the PRDs and the architecture
decision records — are in the repository alongside it, and they are considerably
more pedantic than the articles.*
