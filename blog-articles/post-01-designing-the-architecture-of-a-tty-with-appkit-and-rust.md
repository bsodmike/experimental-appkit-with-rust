# Designing the Architecture of a TTY with AppKit and Rust

There is a moment, building a terminal emulator, when the thing finally launches
and a shell prompt appears in your own window, and the honest reaction is: *how?*
You did not write a shell. You did not teach anything to print `~/projects %`.
You started a process and a prompt appeared.

The answer is more interesting than the question, and it is fifty years old.

A terminal emulator does not emulate a shell. It does not interpret commands, it
does not know what `ls` is, and it has no opinion about your prompt. What it
emulates is a **VT100** — a beige box from 1978 — and the reason a shell talks to
it at all is that the kernel will manufacture, on request, a device that is
indistinguishable from a serial line with a teletype on the end.

Everything else is bookkeeping. Very particular bookkeeping, which is what the
rest of this article is about.

---

## Part 1: What a TTY actually is

### It was a typewriter

`TTY` is short for *teletypewriter*. Not a metaphor — an actual machine, the
Teletype Model 33, introduced in 1963: a keyboard, a roll of paper, a print head,
and a current loop running at 110 baud. You typed, the characters went down the
wire, and whatever was on the other end sent characters back which the machine
printed. There was no screen, no cursor, and no way to erase anything, because
the output was ink on paper.

Early Unix, in 1969, used one of these as its console. Which explains a great
deal of what a terminal still does today:

**Carriage return and line feed are two different characters** because they were
two different mechanical actions. `CR` (0x0D) slammed the print head back to the
left margin. `LF` (0x0A) advanced the paper by one line. You wanted both, in that
order, and they were separate because sometimes you wanted only one — `CR` alone
let you overstrike a line to make bold text.

**The kernel has a "line discipline"** because the machine had none. The Model 33
had no editing: it sent every keystroke immediately. So the kernel had to
implement backspace, echo the characters back so the user could see what they
typed, and buffer a line until Return was pressed. On early Unix the erase
character was `#` and the line-kill character was `@`, because the Teletype had
no backspace key worth the name.

**`stty` still has settings for mechanical delays.** `NL1`, `CR2`, `TAB3` — those
are instructions to pad output with NUL characters, to give the print head time
to physically travel back across the page before the next character arrives. You
can set them today. They will do nothing, on a machine that no longer exists.

**`SIGHUP` is a hangup** — as in the modem hanging up, because your teletype was
frequently at the other end of a phone line. When the carrier dropped, the kernel
sent every process on that terminal a signal saying so. Close a terminal window
today and the same signal, with the same name, is sent for the same reason.

### Then it grew a screen

The DEC VT52 arrived in 1975 and the VT100 in 1978, and everything changed —
because a screen can be *redrawn*, and a program can move a cursor around it.

But the wire between the computer and the terminal was still a serial line
carrying bytes. There was no second channel for commands. So the commands were
smuggled into the byte stream itself, introduced by a character nobody would type
on purpose: `ESC`, 0x1B.

```
      "hello"           →  five printable characters
      ESC [ 2 J         →  clear the screen
      ESC [ 3 1 m       →  everything after this is red
      ESC [ 1 2 ; 4 H   →  put the cursor at row 12, column 4
```

That convention was standardised as ECMA-48 and ANSI X3.64, and it is *still what
your terminal speaks*. When `vim` redraws, it is sending sequences a VT100 would
have understood. The `ESC [ A` your up-arrow key produces is a VT100 sequence.
Terminals are so committed to this that `TERM=xterm-256color` — the value almost
every terminal claims today — describes a 1984 X11 program emulating a 1978
DEC terminal.

Programs learned what their terminal could do from a database: **termcap**,
written by Bill Joy around 1978 so that `vi` could work on more than one model,
and later **terminfo**. The `TERM` environment variable is the key into it. This
is why setting `TERM` matters, and why a terminal that lies about what it is will
have programs sending it sequences it cannot honour.

### And then it stopped being a device at all

Once you have windowing systems, `ssh`, and `screen`, you want a terminal that is
not a piece of hardware. But every program in Unix assumes its terminal is a
device file: it calls `isatty()`, it sends `ioctl`s to ask how wide the screen
is, it expects `Ctrl+C` to arrive as a signal.

So the kernel learned to fabricate one. A **pseudo-terminal** is a pair of file
descriptors:

- the **slave** (`/dev/ttys004`), which is a terminal device in every respect a
  program can test for
- the **master**, held by whichever program wants to *be* the terminal

Anything written to one comes out of the other, with the line discipline sitting
in between doing what it always did.

```
        the terminal emulator                    the shell
        ─────────────────────                    ─────────
                                  KERNEL
   master fd  ─── write ────────┐        ┌──────────────►  fd 0  stdin
                                │        │                 fd 1  stdout
              ◄──── read ───────┤        ├───────────────  fd 2  stderr
                                │        │
                        ┌───────┴────────┴───────┐
                        │    line discipline     │
                        │  echo, erase, ^C→SIGINT│
                        │  \n → \r\n on output   │
                        └────────────────────────┘
```

**This is the whole trick.** The shell holds an ordinary terminal device. It does
not know, and cannot find out, that the other end is a program rather than a
serial port. `isatty(0)` is true, so it runs interactively and prints a prompt.
It has a controlling terminal, so job control works and `Ctrl+C` becomes a signal.
It asks the kernel for the window size and gets an answer.

The shape has not changed in fifty years — only what is at the top:

```
             1970                              today
      ──────────────────              ──────────────────────
       ┌──────────────┐                ┌──────────────────┐
       │ Teletype 33  │ paper, keys    │   Crustty.app    │ pixels, NSEvent
       └──────┬───────┘                └────────┬─────────┘
              │ 110 baud                        │ read()/write()
       ┌──────┴───────┐                ┌────────┴─────────┐
       │  UART driver │  kernel        │   pty master     │  kernel
       │ line discipl.│                │  line discipl.   │
       └──────┬───────┘                └────────┬─────────┘
              │                                 │
       ┌──────┴───────┐                ┌────────┴─────────┐
       │    shell     │                │       zsh        │
       └──────────────┘                └──────────────────┘
```

The left-hand column is hardware. The right-hand column is the same diagram with
the hardware replaced by a program and a kernel device. That program is what we
are building.

### Three moves make it believe

Handing a process a pty slave is not enough. Three things happen in the child
between `fork()` and `exec()`, and all three matter:

```rust
// 1. the slave becomes stdin, stdout and stderr
(pts.try_clone()?, pts.try_clone()?, pts.try_clone()?)

// 2. a new session, detached from whatever started us
rustix::process::setsid()?;

// 3. this pty becomes the controlling terminal
rustix::process::ioctl_tiocsctty(pts_fd)?;
```

Skip the second and the shell inherits our session, so job control lands in the
wrong place. Skip the third and there is no controlling terminal, so `Ctrl+C`
signals nobody and full-screen programs misbehave in ways that take an afternoon
to trace.

Then the environment, which is the part everyone forgets. An app launched from
Finder inherits almost nothing: no `TERM`, and a `PATH` of `/usr/bin:/bin`. So:

- `TERM=xterm-256color`, or `vim` refuses to start
- `COLORTERM=truecolor`, or 24-bit colour is not attempted
- the shell runs as a **login** shell, so it reads your profile and rebuilds `PATH`

Only then does zsh print a prompt. And the prompt is not special: it is bytes on
a file descriptor, exactly like any other output.

---

## Part 2: The architecture

Everything above is true of every terminal emulator. What follows is one specific
set of decisions about where to put the work.

### The hourglass

The design has a waist. Above it, a native macOS application that knows about
pixels, fonts and `NSEvent` and nothing about terminals. Below it, a Rust engine
that knows about VT100 sequences, wrapped lines and wide characters and nothing
about macOS. Between them, a C ABI narrow enough to hold in your head.

```
   ┌────────────────────────────────────────────────────────┐
   │  native/macos/Sources    AppKit, Core Text             │
   │    NSView, drawRect:, NSEvent, NSTextInputClient       │
   ├────────────────────────────────────────────────────────┤
   │  native/macos/Glue       plain C++, no AppKit          │
   │    key mapping, colour resolution, cell metrics        │
   ├══════════════════ THE WAIST ═══════════════════════════┤
   │  terminal-ffi            13 extern "C" functions       │
   │    opaque handle, repr(C) structs, byte buffers        │
   ├────────────────────────────────────────────────────────┤
   │  terminal-pty            POSIX: pty, fork, reader      │
   ├────────────────────────────────────────────────────────┤
   │  terminal-core           the engine. No platform.      │
   │    VT parser, screen, scrollback, reflow, key encoding │
   └────────────────────────────────────────────────────────┘
```

Why a waist at all? Because the two halves have completely different failure
modes and completely different testing stories. The engine can be tested with a
compiler and nothing else — no window server, no Mac, no human. The frontend
cannot be tested without a screen and a pair of eyes. Putting a hard boundary
between them means you can make the untestable half as small as possible.

The proportions, in lines:

```
   terminal-core   6024   ████████████████████████████  engine
   terminal-ffi    1518   ███████                       boundary
   Glue            1367   ██████                        frontend logic
   terminal-pty    1096   █████                         POSIX
   Sources          808   ████                          AppKit
```

Eight hundred lines of Objective-C++, and they contain no decisions. That is the
point of the whole arrangement.

### terminal-core — the engine

Pure Rust, no dependencies on any operating system. It owns everything that is
*terminal semantics*:

- **The VT parser.** Bytes to typed commands, streaming — a read can end in the
  middle of `ESC [ 3 1 m` or in the middle of a UTF-8 character, so the parser
  holds state across calls and keeps the incomplete tail.
- **The screen.** A grid of cells, each holding a grapheme cluster rather than a
  character, because `e` plus a combining acute is one cell and no `char` can
  hold it. Plus the cursor, the deferred end-of-line wrap that stops a full line
  wrapping one row early, scroll regions, the alternate screen.
- **Scrollback and reflow.** Scrollback is not a grid; it is logical lines stored
  as packed UTF-8 with attribute runs, so that resizing the window rewraps them
  the way the text actually is rather than the way it happened to be displayed.
- **Key encoding.** `Ctrl+C` becoming byte `0x03`, and the arrow keys producing
  `ESC [ A` or `ESC O A` depending on a mode the program can change, are
  properties of the *terminal*, not of macOS.

The guiding test: *if the engine ever needs a Mac in order to be tested,
something has leaked.* It has 304 tests and they run anywhere.

### terminal-pty — the POSIX half

The pty, the child process, and the reader thread. Separate from the engine
because it is platform code, even though it is not macOS code.

The interesting decision here is threading. There is exactly one file descriptor
to watch, so there is nothing to multiplex and an async runtime would buy
nothing. Instead: one thread, blocking on `read()`.

That thread **parses**. It does not shuttle bytes to the main thread to be dealt
with later — it takes the engine's lock, runs the parser, mutates the screen and
releases. The UI thread must never parse, because parsing is unbounded work
driven by a hostile-by-accident data source: any program can print a megabyte.

Stopping it cleanly is fiddlier than starting it. A thread blocked in `read()`
cannot be asked to stop by a flag it never looks at, and closing the descriptor
underneath it races with descriptor reuse. So the reader waits on **two**
descriptors — the pty and a pipe nobody writes to except at shutdown:

```
   reader thread:
     poll([pty_master, interrupt_pipe])
       │
       ├── pty readable      → read, parse, maybe reply, mark dirty
       ├── interrupt readable → return; the thread is being joined
       └── pty hung up        → drain what is left, then stop
```

### terminal-ffi — the waist

Thirteen `extern "C"` functions and four kinds of thing that may cross:

| Shape | Example |
|---|---|
| Opaque handle | `TerminalSession *` — C stores it, never dereferences it |
| Plain data | `#[repr(C)]` structs of fixed-width integers |
| Byte buffers | pointer plus explicit length, never NUL-terminated |
| Callback | function pointer plus a `void *` context |

Three rules make it survivable. **Null is always checked** and returns a status,
never a crash. **No panic escapes**: since Rust 1.81 unwinding out of an
`extern "C"` function aborts the process, which kills the user's session, so
every entry point catches. And **data crosses by copying into caller-owned
buffers** — there is no second lifetime to get wrong and no free-function to
forget.

The header is generated by cbindgen rather than written, and it earned that on
the first run: an idiomatic `Option<TerminalWakeUpFn>` came out as an opaque
struct that C could not have filled in. A hand-written header would have said the
right thing while the library expected something else.

### The frontend, split in two

The Objective-C++ is divided by directory, and the rule is enforced by which
folder a file lives in:

- **`Glue/`** — plain C++17, no AppKit. Key mapping, colour resolution, cell
  metrics, the frame copy protocol, config parsing, the RAII session handle.
  Compiles and is tested **on Linux**, linked against the real static library.
- **`Sources/`** — Objective-C++, AppKit only. Draws, routes events, and decides
  nothing.

The working rule: *a file in `Sources` may not contain an `if` that matters.* If
a branch is worth getting right, it belongs in `Glue`, where 75 tests run on a
machine with no screen.

---

## Part 3: The loop

Here is the whole thing, in both directions, with the names you would grep for.

### Output: from `printf` to pixels

```
  zsh writes its prompt to fd 1
        │
        ▼
  kernel line discipline            \n becomes \r\n
        │
        ▼
  pty master becomes readable
        │
        ▼
  "pty-reader" thread wakes         terminal-pty/src/terminal.rs
        │
        ▼
  Session::feed(&bytes)             takes the lock  session.rs
        │
        ├─► VtParser: bytes → Command                parsers/vt.rs
        ├─► Screen::apply: print, CR, LF, SGR        screen.rs
        └─► replies owed (CSI 6n) written back to the pty
        │
        ▼
  dirty flag flips false → true     ONCE per burst
        │
        ▼
  wake-up callback                  runs on the reader thread
        │
        ▼
  dispatch_async(main queue)        TerminalView.mm
        │
        ▼
  setNeedsDisplay: → drawRect:      the run loop decides when
        │
        ▼
  terminal_copy_frame               ONE lock, one consistent frame
        │
        ▼
  CTFontDrawGlyphs at col × cellWidth
```

Two details in there are load-bearing.

**The dirty flag is coalesced.** `cat` of a large file produces thousands of
small reads. If each one asked the UI to redraw, the main thread would spend its
life draining a backlog of redundant redraws and the app would appear to hang
under exactly the workload it should handle best. Only the `false → true`
transition posts a redraw; every read after that sets a flag that is already set
and costs nothing.

**The frame is copied under one lock.** Not a pointer into the grid — a copy into
buffers the caller owns, taken in a single critical section, so the frame cannot
tear between row 3 and row 40 while the reader thread is writing.

### Input: from a keypress to the shell

```
  keyDown: NSEvent
        │
        ▼
  Glue::map_key(keyCode, modifiers, chars)      KeyMap.cpp
        │
        ├── Command held?      → an app command; never crosses
        ├── a terminal key?    → terminal_send_key
        └── otherwise          → interpretKeyEvents: → insertText:
        │                          (dead keys, IME composition)
        ▼
  keys.rs encodes it against the current modes  terminal-core
        │      Ctrl+C     → 0x03
        │      Up         → ESC [ A, or ESC O A in application mode
        ▼
  write to the pty master
        │
        ▼
  kernel line discipline       echo, ^C → SIGINT to the foreground group
        │
        ▼
  zsh's stdin
```

Keyboard input arrives twice on macOS and a terminal needs both views of it. The
raw `NSEvent` is the only thing that can tell you the up arrow was pressed, since
it produces no text at all. The interpreted path is the only thing that can
handle an input method composing Japanese, where provisional text must be shown
but not sent until it is committed. Deciding which is which is `map_key`, and it
is tested without a keyboard.

### Why a run is not a cell

The obvious frame format is one struct per cell. It cannot work: a single `u32`
codepoint cannot hold `e` plus a combining acute, and per-cell glyph drawing is
both slow and wrong, because a mark has to be shaped together with the character
it attaches to.

So the engine emits **runs**: spans of consecutive columns sharing one style,
described as a slice of a shared UTF-8 buffer.

```
  row 3:  "  " + "error:" in red + " no such file"

  ┌──────────────────────────────────────────────────────────┐
  │ text:  "  error: no such file"                           │
  └──────────────────────────────────────────────────────────┘
      run 0  off 0  len 8   col 0  cols 8   fg default
      run 1  off 8  len 6   col 8  cols 6   fg 0x01_000001   ← indexed red
      run 2  off 14 len 13  col 14 cols 13  fg default
```

Which leaves the frontend with one genuinely subtle job. A `CTLine` places each
glyph at the font's own advance, and those advances are fractional: at 13pt, SF
Mono advances 7.8 points per character, so a line laid out that way sits sixteen
points left of the row above it by column 80. The grid stops being a grid.

So the line is built for its *shaping* and then taken apart — ask each run for its
glyphs and which part of the string they came from, work out which grapheme
cluster that is, and draw it at `column × cellWidth`:

```
   what the font wants          what the grid demands
   ─────────────────────        ─────────────────────
   h  e  l  l  o                h   e   l   l   o
   ↑7.8↑7.8↑7.8↑7.8             ↑ 8 ↑ 8 ↑ 8 ↑ 8
   drifts 16pt by col 80        every column exact
```

Counting clusters only works while every cluster in a run is one column wide —
which is why the engine never merges a double-width character into a run with its
neighbours. `a漢b` is three runs, not one. The renderer does not know that `漢` is
two columns and must never have to learn.

---

## Bringing it together

Start the app and this is what happens, end to end:

1. `main.mm` builds a menu bar and starts the run loop
2. the delegate makes a window and a view, and the view measures the font
3. the view divides its size by the cell size and asks for a terminal that shape
4. `terminal_create` crosses the waist into Rust
5. Rust opens `/dev/ptmx`, gets a master and a slave
6. it forks; the child sets up a session, claims the pty as its controlling
   terminal, points stdin/stdout/stderr at it, and execs `/bin/zsh -l`
7. zsh sees a terminal, reads your profile, and prints a prompt
8. those bytes come back through the master to a thread blocked in `poll`
9. the parser turns them into commands, the screen turns commands into cells
10. a flag flips, a block is posted to the main queue
11. `drawRect:` copies a frame and draws it with Core Text

You did not write a shell. You wrote one end of a pipe, and a very careful
renderer for what comes out of it.

### The bug that proves the point

On the first successful run, the window opened, the background was right, the
cursor was in the right place — about a third of the way across the first row —
and there was no text at all.

Nothing was broken. `CTFontDrawGlyphs` paints with the graphics context's fill
colour, and the last thing to set that was the window background. The prompt was
on screen, faithfully parsed, correctly laid out, painted `#1E1E1E` on `#1E1E1E`.
The cursor was visible only because filling a rectangle sets its own colour.

The tell was the cursor's position. It sat exactly where a `zsh` prompt ends —
which meant the bytes had arrived, the parser had understood them, the screen had
placed them and the cursor had advanced over them. Every layer had done its job.
Only the paint was wrong.

That is the clearest possible statement of what a terminal emulator is. The
terminal was complete and correct while showing you absolutely nothing, because
the terminal is not the pixels. The terminal is the pty, the agreement about
escape sequences, and a data structure that a program on the other side is
mutating one byte at a time.

The pixels are just the last, optional step.

---

*Next in this series:
[The Buffer Model](post-02-the-buffer-model-why-scrollback-cannot-be-a-grid.md) —
why scrollback cannot be a grid of rows, and what happens to your text when you
drag the window edge.*
