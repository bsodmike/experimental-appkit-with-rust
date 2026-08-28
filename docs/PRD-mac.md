# PRD: The macOS frontend

## 1. What this document is for

`docs/PRD.md` is the blueprint for the Rust half — the engine, the PTY, and the C
boundary between them and everything else. This document is the blueprint for
what sits on the other side of that boundary: an AppKit application that draws a
terminal and feeds it events.

It is written in the same spirit. It records decisions and the reasons for them,
so that a decision is not re-litigated every time someone reads the code, and so
that the reason survives longer than the memory of making it.

It assumes the reader knows the Rust half. It does **not** assume the reader
knows AppKit: `PRD-mac-01-concepts.md` explains the macOS concepts each decision
rests on, and every section here that depends on one names it.

The guiding sentence from PRD §2 still applies, and cuts harder on this side:

> A Rust terminal emulator with a native macOS frontend — not a macOS terminal
> application implemented in Rust.

---

## 2. What the native half is

Four things, and nothing else:

| Concern | Why it is native |
|---|---|
| Font metrics | Core Text owns the font. Nobody else can measure it. |
| Drawing | Core Graphics and Core Text own the pixels. |
| Event translation | `NSEvent` is the only source of "which key, which modifiers". |
| Colour resolution | The theme is the app's, not the terminal's (PRD §5). |

Everything else has already been decided in Rust before the view sees it. The
view is handed runs of styled text at columns and rows; it does not know what a
wrapped line is, what a wide character is, what a scroll region is, or what
`ESC [ 2 J` means. If any of those concepts appears in `native/macos/Sources`,
something has leaked across the boundary and the fix is in Rust, not here.

The test to apply, mirroring PRD §5's:

> **Could this decision be wrong in a way that a screenshot would reveal?**
> If yes, it is the frontend's. If it would be wrong in a way only a terminal
> program could notice, it is the engine's.

---

## 3. Layout: `Glue` and `Sources`

**Decision: the frontend is split into platform-free C++ and AppKit shims, and
the split is enforced by where the file lives.**

```
native/macos/
  Glue/        pure C++17. No AppKit, no Core Text, no Objective-C.
  Sources/     Objective-C++. AppKit and Core Text only.
```

The reason is verifiability. `Glue` compiles on Linux, where it is unit-tested
against the real `libterminal_ffi.a` on every change. `Sources` compiles only on
macOS and is exercised by hand and by a thin XCTest suite. So every line that
*decides* something is tested, and the untested lines are the ones that only
translate — a selector, a callback, a `CGContext`.

The rule that keeps it honest: **a file in `Sources` may not contain an `if` that
matters.** If a branch is worth getting right, it belongs in `Glue`.

What lives in `Glue`:

| Unit | Responsibility |
|---|---|
| `KeyMap` | `(keyCode, modifierFlags, characters)` → `TerminalKeyEvent`, or "send as text" |
| `Palette` | packed colour → RGBA, with the reverse, dim and hidden rules |
| `Metrics` | view size + cell size → rows/cols; run → rect; baseline for a row |
| `FrameBuffers` | the two-call copy protocol, regrowing on `BufferTooSmall` |
| `Session` | RAII over `TerminalSession *` — the destructor calls `terminal_destroy` |

---

## 4. The cell grid, and why rounding is load-bearing

**Concept:** Core Text measures a glyph's *advance* — how far the pen moves after
drawing it — as a floating-point number in points. See concepts §3.

**Decision: the cell width is the font's advance rounded to a whole point, once,
and every column is placed at `col × cellWidth` from that rounded value.**

The naive alternative — let Core Text lay out a line and place the next glyph
wherever the font says — drifts. SF Mono at 13pt advances 7.8pt per character;
by column 80 a line laid out that way sits 16pt left of where the row above it
sits. The grid stops being a grid, box-drawing characters stop meeting, and the
cursor lands between cells.

So:

```
cellWidth  = round(advance of "M")           // whole points
cellHeight = ceil(ascent + descent + leading)
baseline for row r = viewHeight - r × cellHeight - ascent
```

`round` rather than `ceil` because a consistent half-point of extra tracking is
invisible, while a whole point of it is not.

**Decision: the view is *not* flipped.** AppKit's default coordinate system has
the origin at the bottom-left and y increasing upward, which is also what
`CTLineDraw` expects. Flipping the view means every text draw needs a
compensating transform, and forgetting one draws the text upside down. Row 0 is
therefore at `viewHeight - cellHeight`, which `Metrics` computes and tests.

### Where the font comes from

**Decision: the font and its default size come from `NSUserDefaults`, and are
validated in `Glue::Config` before anything believes them.**

`defaults write com.inertialbox.crustty fontSize -int 15` should not require a
rebuild, and `NSUserDefaults` is the platform's answer to that — no file format
to invent, no parser to write, no reload story. What arrives is not trusted: a
size of zero is not a preference, and a named font that is not installed falls
back to the system monospaced font, which is always present and actually
monospaced.

**Decision: ⌘+, ⌘− and ⌘0 change the size, and the change is not persisted.**
Zoom takes the same path a window resize already takes — remeasure the font,
re-derive the grid, tell the engine — so it costs almost nothing. It is not
saved because v0 saves nothing at all (§11); the size returns to the default on
relaunch, and persisting it later is one number in a file that does not exist
yet.

**Decision: the terminal size is derived from the view, not the other way
round.** `rows = floor(height / cellHeight)`, `cols = floor(width / cellWidth)`,
both clamped to at least 1. Leftover pixels are padding at the bottom and right.
A window is never resized to fit the grid, because fighting the window manager
is a losing game and a few pixels of margin are invisible.

---

## 5. The draw path

**Concept:** Core Text turns a string plus attributes into positioned glyphs.
See concepts §4.

One redraw is:

1. `terminal_copy_frame` into the reusable buffers — **one call, one lock**, so
   the frame cannot tear between row 3 and row 40 (PRD §10).
2. Fill the background: for each run whose `bg` is not the default, fill
   `Metrics::rect_for(run)`.
3. Draw the text: for each run, one `CTLine`, drawn **glyph by glyph at explicit
   positions**.
4. Draw the cursor, if `cursor_visible`.

Step 3 is the one with a trap in it. A `CTLine` drawn with `CTLineDraw` uses the
font's own advances, which is exactly the drift §4 exists to prevent. So the
line is built for shaping only — combining marks must be shaped with their base
character, which is why runs exist at all — and then taken apart:

```
CTLineGetGlyphRuns          →  the shaped glyphs
CTRunGetStringIndices       →  where each glyph came from in the string
                               → cluster index, via CFStringGetRangeOfComposed…
position.x = (run.col + clusterIndex) × cellWidth
CTFontDrawGlyphs            →  drawn where the grid says, not where the font says
```

**This is why the engine never merges a double-width cluster into a run**
(PRD §10, render-frame ADR): counting clusters gives the column only while every
cluster in the run is one column wide. The frontend does not know that `漢` is
two columns and must never learn.

**Decision: ligatures are disabled** (`kCTLigatureAttributeName = 0`). A ligature
merges two clusters into one glyph, which breaks the cluster count and makes text
land in the wrong cell. Programming ligatures are a preference to add later, on
the explicit understanding that they cost the cluster mapping a special case.

**Decision: the cursor is a filled rectangle drawn with the cell's colours
swapped, and does not blink.** Blink is a timer, a redraw region and a
preference; none of it is v0. `cursor_visible` from the frame is obeyed, because
a full-screen program that hides the cursor while redrawing means it.

---

## 6. Input: three channels, and one that never crosses

**Concept:** AppKit delivers keyboard input twice — once as a raw `NSEvent`, and
once, after the input system has had its say, as committed text. See concepts §5.

PRD §8 settles the routing; this is what it means in AppKit terms:

| Source | Goes to | Because |
|---|---|---|
| `insertText:` (`NSTextInputClient`) | `terminal_send_text` | Already UTF-8, already committed |
| `keyDown:` for terminal keys | `terminal_send_key` | Arrows have no characters; `Ctrl+C` has the wrong one |
| ⌘-anything | Nowhere | It is an application command (PRD §8) |
| ⌘V | `terminal_paste` | The engine frames it, and strips its own end marker |

**Decision: `keyDown:` asks `Glue::map_key` first, and calls
`interpretKeyEvents:` only when the answer is "this is text".** The alternative —
always interpreting, and inspecting what comes back — loses `Ctrl+C`, which
different layouts turn into different characters or none at all.

**Decision: while marked text exists, nothing is sent.** An IME composing
Japanese shows provisional text in the view; sending it to the shell as it
changes would type garbage and then not be able to take it back. `insertText:`
fires once, on commit, and that is the only thing that crosses.

---

## 7. The wake-up

**Concept:** AppKit draws on the main thread, and only the main thread may touch
a view. See concepts §2.

**Decision: the callback does nothing but `dispatch_async` a `setNeedsDisplay:`
onto the main queue.**

It runs on the reader thread, which is inside the engine's lock discipline
(PRD §7): it must not draw, must not block, and must not call back into the
terminal API — a re-entrant call while the lock is held would deadlock on a
non-reentrant mutex.

The coalescing has already happened in Rust: one wake-up per burst of output, not
one per read. The frontend must not add a timer to "throttle" redraws on top of
it. That would be solving a solved problem in the one place with the least
information about it.

**Decision: the context pointer is the view, unretained.** It must outlive the
session, which the ordered teardown in §9 guarantees. Retaining it would make the
view own itself through the callback and never deallocate.

---

## 8. Colour

**Concept:** none — this is arithmetic. See PRD §10 for the wire format.

A run's `fg` and `bg` are packed, not resolved: `0x00_000000` is the terminal
default, `0x01_0000II` a palette index, `0x02_RRGGBB` truecolour. The engine owns
no theme (PRD §5); resolving is the frontend's job and `Glue::Palette` does it.

**Decision: one hard-coded dark theme in v0**, as a table in `Palette`. Reading a
theme from disk means a file format, a search path, a reload story and a
migration; it is a whole feature and it is not this one. The theme is a constant
until it is a preference.

The rules that are easy to get wrong, and are therefore tested:

- **Reverse** swaps resolved foreground and background — *after* resolution, so
  reverse on default colours gives the theme's background on its foreground.
- **Dim** blends the resolved foreground toward the background by a fixed factor.
  It is not a different palette entry.
- **Hidden** draws the background colour as the foreground, so the text is there,
  selectable and copyable, and simply invisible.
- The 256-colour cube is `16 + 36r + 6g + b` with the levels `0, 95, 135, 175,
  215, 255`, and the greyscale ramp is `8 + 10n`. These are conventions, not
  arithmetic anyone should re-derive.

---

## 9. Lifetime and teardown

**Decision: the `TerminalSession *` is owned by a C++ RAII wrapper in
`Glue::Session`, and by nothing else.**

PRD §6 gives the frontend exactly one obligation — call `terminal_destroy` once —
and a destructor is the only construct that cannot forget. `Session` is
non-copyable, movable, and its destructor is the whole of the cleanup.

**Decision: teardown is ordered explicitly at `applicationWillTerminate:`**, not
left to deallocation order. `terminal_destroy` joins the reader thread and hangs
up the shell (PRD §7); it must happen while the view it might wake is still
alive. The sequence is: destroy the session, then let the window go.

---

### When the shell goes away

**Decision: a clean exit closes the window; anything else keeps it.**

Typing `exit` ends the session and the window goes, which is what Alacritty and
kitty do and what the gesture means. But a shell that died from an error printed
that error immediately before dying, and closing the window is the least helpful
possible response to it. So a non-zero status, or a signal, leaves the last
frame on screen with the reason in the title.

This is the rule Terminal.app has had for years, and the one Ghostty arrived at
from the other direction. It is not a compromise between two behaviours; it is
the behaviour, and the two halves only look separate.

There is a third case this project has and single-process terminals do not: the
reader thread stopping for reasons of its own, with the shell's fate unknown.
**That is never treated as a clean exit** — closing the window there would hide
precisely the case where the engine itself is what broke.

`Glue::present_exit` decides all of it from `TerminalChildStatus`, which is why
the rule is three comparisons in a tested function rather than three conditions
scattered through a view.

**Decision: a Debug build draws the engine's own last error on screen.**
`terminal_copy_last_error` carries the message and location that `catch_unwind`
would otherwise discard, because "Panicked" on its own is the least debuggable
state the app can reach. Release builds draw nothing, and **whether the window
closes never depends on how the app was compiled** — only whether you can read
why.

---

## 10. Build

**Concept:** Xcode, schemes, targets and `xcodebuild`. See concepts §7.

**Decision: the Xcode project is generated by XcodeGen from
`native/macos/project.yml`, and the `.xcodeproj` is not committed.**

A `.pbxproj` is a UUID-keyed plist of over a thousand lines that no one can
review and every merge conflicts in. `project.yml` is fifty lines that say what
the project is. The generated project is disposable: `just xcode` recreates it.

The consequence to know: **changes made in Xcode's GUI are lost on the next
generation.** That is the trade, and it is the right way round — the file you can
read is the truth.

**Decision: a pre-build Run Script phase invokes cargo**, so ⌘B in Xcode and
`just build` in a terminal both produce a correct app from a clean tree. Xcode's
`PATH` does not include `~/.cargo/bin` (PRD §14, gotcha 1), so the script sets it
explicitly, and declares its inputs and outputs or Xcode re-runs it every build.

**Decision: `just` is the entry point for everything**, including the things
Xcode can also do. One list of commands, one place to change them, and the
workflow is legible to someone who has never opened Xcode.

**Decision: dev builds are the host architecture only.** A universal binary
doubles build time to serve a machine nobody is developing on. `just
release-universal` exists for when that machine matters.

---

## 11. Distribution

**Concept:** signing, entitlements, the sandbox, Gatekeeper and notarization. See
concepts §8.

**Decision: the target is a notarized direct download. The Mac App Store is out
of scope.**

Not for effort — because it cannot work. App Store distribution requires the App
Sandbox, and a sandboxed process's children inherit the sandbox. The shell would
be confined to the app's container: no reading `~/projects`, no writing a file,
no `git push`. There is no entitlement that lets a child escape, and the
temporary exceptions that once did are not granted any more. Every terminal
emulator on macOS ships as a direct download for exactly this reason.

Notarized direct distribution does **not** want the sandbox, so this is not a
compromise; it is the same choice everyone else made.

**Decision: the scaffolding for that exists now, and signs ad-hoc until it is
needed.**

- `Terminal.entitlements` exists, with the sandbox explicitly disabled and the
  reason written next to it. An empty file would be a question; an explicit
  `false` is an answer.
- **Hardened Runtime is on for Release from the first build.** Notarization
  requires it, it costs nothing locally, and if it were ever going to break the
  way we spawn a shell, that should surface now rather than on shipping day.
  Debug builds keep Xcode's automatic `get-task-allow`, so breakpoints still work.
- `CODE_SIGN_IDENTITY` defaults to `-` (ad-hoc) and is overridden from an
  untracked `Local.xcconfig`. Nothing in the repository needs an Apple account.
- `just release-signed`, `just notarize` and `just dmg` are written and fail with
  a useful message until a certificate exists.

**Decision: the bundle identifier is permanent from the first shipped build.** It
keys preferences, keychain items and the TCC permission database; changing it
later orphans all three. It is `com.inertialbox.crustty` until someone says
otherwise, and the README says to say so before the first signed release.

**Decision: application state lives only in
`~/Library/Application Support/<bundle-id>`**, resolved through `NSFileManager`
rather than a built path. v0 stores nothing at all, which is precisely when this
rule is free to adopt.

---

## 12. Testing

**Decision: the frontend's tests are in three tiers, and the tier is chosen by
what can be *known* rather than by what is convenient.**

| Tier | Runs where | Covers |
|---|---|---|
| `just test-glue` | Linux and macOS | Every decision in `Glue`, linked against the real staticlib |
| `xcodebuild test` | macOS | That the app launches, draws, resizes and tears down |
| The README checklist | A human | That it looks right and feels right |

The first tier is the one that matters, and it is deliberately large. The frame
protocol, the key mapping, the palette and the metrics are all pure functions of
their inputs; there is no reason for any of them to be verified by squinting at
a window.

The second tier is small on purpose. An `NSView` test that asserts pixels is a
test that fails when the font changes; a test that asserts the app can construct
a view, feed it a frame and tear it down is a test that fails when something is
actually broken.

The third tier is the honest name for what a human has to do. It is written down
so that it happens, rather than being remembered differently each time.

---

## 13. What v0 is

**One window, one shell, and the pipeline proved end to end.**

Works: type and see output; resize and see reflow; full-screen programs; the
window title from OSC; paste, bracketed if the program asked; quit with an
ordered shutdown and no orphan process.

Not in v0, in the order they are likely to arrive:

| Deferred | Blocked on |
|---|---|
| Mouse selection, ⌘C | Selection state in the engine, `terminal_pointer_event` |
| Scrollback, scroll wheel | A frame rendered at a scroll offset |
| Bell | An engine event for it; today `Command::Bell` is parsed and dropped |
| Cursor blink and shapes | A timer, and DECSCUSR in the parser |
| Find | Selection, plus a search over scrollback |
| Tabs, splits, preferences | All of the above, and a window controller worth the name |
| Theme from disk, ligatures | §8 and §5 respectively, both deliberate |

None of these requires the frontend to be rebuilt. They are additions to a shape
that already holds them, which is the point of stopping here.

---

## 14. Decisions

| # | Decision |
|---|---|
| 1 | The frontend owns metrics, drawing, event translation and colour. Nothing else. |
| 2 | Platform-free `Glue` versus AppKit `Sources`, enforced by directory |
| 3 | Cell width is the advance rounded once; every column derives from it |
| 4 | The view is not flipped; row 0 is at the top by arithmetic, not by transform |
| 5 | Glyphs are drawn at explicit positions, never at the font's advances |
| 6 | Ligatures off, because they break the cluster-to-column mapping |
| 7 | Three input channels; ⌘ never crosses; nothing is sent while marked text exists |
| 8 | The wake-up only dispatches; no frontend-side throttling |
| 9 | One hard-coded dark theme until it is a preference |
| 10 | `Session` is RAII; teardown is explicit and ordered |
| 11 | XcodeGen generates the project; `just` is the entry point |
| 12 | Notarized direct download; the App Store is out of scope because the sandbox breaks the product |
| 13 | Hardened Runtime on from the first Release build; ad-hoc signing until a certificate exists |
| 14 | Tests are tiered by what can be known, and the Linux-testable tier is the large one |
| 15 | Font and size come from `NSUserDefaults`, validated in `Glue`; zoom is not persisted |
| 16 | A clean exit closes the window; a crash keeps it, and an unexplained hangup is never read as clean |
| 17 | Debug builds draw the engine's last error; the closing rule never depends on the build |
