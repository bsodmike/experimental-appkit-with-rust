# The macOS concepts behind PRD-mac

## What this is

`PRD-mac.md` records decisions. Each one rests on some piece of how macOS works,
and a decision whose foundation you cannot see is just an instruction. This
document is the foundation: the eight things you need to know to read that
document and disagree with it.

It is written for someone who knows systems programming and has never written a
Mac application. It is deliberately not a tutorial — there is no "now add a
button" — and it stops at what the terminal actually uses.

---

## 1. The `.app` bundle

A macOS application is a **directory** that Finder displays as a single icon:

```
Terminal.app/
  Contents/
    Info.plist          metadata: name, bundle id, version, minimum macOS
    MacOS/Terminal      the actual executable
    Resources/          icons, and anything else it ships with
    _CodeSignature/     the signature over everything above
```

Two consequences the terminal cares about:

**The bundle identifier is the app's real name.** `com.crustyengineer.crustty` in
`Info.plist` is what macOS uses to key preferences, keychain items and the
permission database. The name on the icon is decoration; the identifier is
identity, and changing it makes the system treat the app as a different one that
happens to look the same.

**The bundle is read-only in practice.** It is signed, so writing into it breaks
the signature. Anything an app saves goes to `~/Library/Application Support/<id>`.

The build produces this whole directory; `just release` puts one in
`native/macos/build`. `open Terminal.app` launches it, and running
`Terminal.app/Contents/MacOS/Terminal` directly runs the same binary with its
output in your shell — which is why `just run` does the latter.

---

## 2. The run loop, and the main thread

An AppKit application is a loop: wait for an event, dispatch it, repeat. The
thread running that loop is the **main thread**, and it is the only thread
allowed to touch a view.

That is not advice. Calling `setNeedsDisplay:` from another thread is undefined
behaviour that usually looks like working code until it corrupts something.

The way to cross back is `dispatch_async`, which appends a block to the main
queue for the loop to run when it next gets the chance:

```objc
dispatch_async(dispatch_get_main_queue(), ^{
    [view setNeedsDisplay:YES];
});
```

**Why the terminal cares:** the PTY reader thread is a Rust thread that knows
nothing about run loops. When output arrives it calls the wake-up callback, and
that callback's entire job is the four lines above (PRD-mac §7). Anything more
ambitious — drawing, or asking the engine a question — is either illegal or a
deadlock.

`setNeedsDisplay:` does not draw. It marks the view dirty and returns; the run
loop coalesces every such mark and draws once before the next frame. This is the
same coalescing the engine does on its side of the boundary, and it is why a
thousand small reads cost one redraw rather than a thousand.

---

## 3. Views, drawing and coordinates

An `NSView` is a rectangle that draws itself. The system calls `drawRect:` when
it needs pixels; you never call it yourself.

Inside `drawRect:` there is a **current graphics context** — a `CGContext`, the
Core Graphics drawing destination. You fill rectangles and draw glyphs into it,
and when the method returns the system puts the result on screen.

**Coordinates.** AppKit's default origin is the **bottom-left**, with y
increasing upward — the mathematical convention, not the screen convention. A
view can set `isFlipped` to `YES` to get the top-left origin most graphics APIs
use.

The terminal does not flip, and the reason is Core Text: `CTLineDraw` draws text
right-side-up in an unflipped context, and upside-down in a flipped one unless
you apply a compensating transform to the text matrix. One arithmetic expression
that computes a baseline is easier to get right, and easier to test, than a
transform that has to be applied everywhere and can be forgotten in one place
(PRD-mac §4).

**Points, not pixels.** All of this is in points. On a Retina display one point
is two pixels, and the system handles that: draw a 1-point line and get a
2-pixel line, correctly. You will not see the word "pixel" in the frontend.

---

## 4. Core Text, and why not the easy API

The easy way to draw a string in AppKit is `[string drawAtPoint:withAttributes:]`.
It is one line and it is wrong for a terminal.

**Core Text** is the layer underneath it. The pieces the terminal uses:

- **`CTFont`** — a font at a size. It can tell you a glyph's **advance**: how far
  the pen moves after drawing it. For a monospace font every advance is the same,
  and that number is where the cell width comes from.
- **`CTLine`** — a laid-out line of text, built from a string plus attributes.
  Building one performs **shaping**: turning characters into positioned glyphs,
  which is where `e` + a combining acute becomes one glyph, and where an emoji
  ZWJ sequence becomes one picture.
- **`CTRun`** — a span of a `CTLine` sharing one font. It can hand you the raw
  glyphs and, crucially, which part of the string each glyph came from.
- **`CTFontDrawGlyphs`** — draw specific glyphs at specific positions.

**Why the terminal takes the line apart.** `CTLineDraw` places each glyph where
the font says it goes. In a terminal every glyph must go where the *grid* says it
goes, and those disagree by a fraction of a point per character — a fraction that
accumulates into a visibly bent column by the right-hand side of the window.

So the line is built for its shaping and then dismantled: ask each `CTRun` for
its glyphs and their string indices, work out which grapheme cluster each glyph
belongs to, and draw it at `(column × cellWidth)`. Real terminal emulators all do
some version of this; it is the price of a grid that stays a grid (PRD-mac §5).

**Ligatures** are the same problem wearing a hat. A font that renders `!=` as one
glyph has merged two clusters, and the mapping from glyph to column no longer
holds. They are switched off.

---

## 5. Keyboard input arrives twice

macOS gives an application two views of the same keystroke, and a terminal needs
both.

**The raw event.** `keyDown:` receives an `NSEvent` with a `keyCode` — a
hardware-ish number identifying the physical key, independent of layout — and
`modifierFlags`, a bitmask of Shift/Control/Option/Command.

**The interpreted text.** Calling `interpretKeyEvents:` hands the event to the
input system, which applies the keyboard layout, dead keys and any input method,
and eventually calls `insertText:` with finished text. For most keys this happens
immediately; for an IME it happens after a composition session that may involve
many keystrokes and a candidate window.

**Why both.** Arrow keys produce no text at all, so the interpreted path never
fires for them. `Ctrl+C` produces text, but which text depends on the layout, and
none of the possibilities is the byte `0x03` that a terminal must send. Meanwhile
IME composition can only work through the interpreted path — the view shows
provisional **marked text** and must send nothing until it is committed.

So `keyDown:` decides: if this is a key with terminal meaning, encode it and send
bytes; otherwise hand it to the input system and wait for `insertText:`
(PRD-mac §6).

**`NSTextInputClient`** is the protocol a view implements to participate in that
second path. Implementing it is what makes an IME work at all; the methods that
matter are `insertText:replacementRange:`, `setMarkedText:…` and
`unmarkText`.

**Command is different.** ⌘-anything is an application command — ⌘Q, ⌘V, ⌘W — and
AppKit routes those through the menu bar before `keyDown:` ever sees them. A
terminal must not send them to the shell, which is why PRD §8 says they never
cross the boundary.

---

## 6. ARC, and Objective-C++

**ARC** (Automatic Reference Counting) is how Objective-C objects are managed:
the compiler inserts retain and release calls, and an object dies when nothing
refers to it. It is not garbage collection — it is deterministic, and a cycle
leaks.

ARC covers Objective-C objects. It does **not** cover `malloc`, C++ objects, or
Core Foundation types like `CTLine` — those follow the Core Foundation rule that
anything you got from a function with `Create` or `Copy` in its name must be
`CFRelease`d.

**Objective-C++** is what you get by naming a file `.mm`: Objective-C and C++ in
the same translation unit. The terminal uses it for one reason — a C++
destructor cannot be forgotten, and `terminal_destroy` must not be forgotten.
`Glue::Session` is a C++ class whose destructor is the entire cleanup story
(PRD-mac §9).

The constraint from PRD §13: **C++ types must never appear in the Rust-facing
ABI.** C++ has its own name mangling and layout rules. `std::vector` in the glue
is fine; `std::vector` crossing the boundary is not.

---

## 7. Xcode, targets, schemes and `xcodebuild`

The vocabulary, since it is used without explanation everywhere:

- A **project** (`.xcodeproj`) is a bundle of build settings and file references.
  Internally it is `project.pbxproj`, a plist keyed by UUIDs — machine-readable,
  human-hostile, merge-hostile. This is why it is generated (PRD-mac §10).
- A **target** is one thing to build: the app, or a test bundle.
- A **scheme** ties targets to actions — what "Build", "Run" and "Test" mean.
- **Build settings** are inherited key-value pairs. `SYMROOT` is where output
  goes, `PRODUCT_BUNDLE_IDENTIFIER` is the bundle id, `MACOSX_DEPLOYMENT_TARGET`
  is the oldest macOS the result will run on.
- An **`.xcconfig`** file is build settings as text rather than as GUI state.
  It is how a local signing identity stays out of the repository.
- A **Run Script phase** is a shell script that runs as part of a build. It does
  not run in your login shell, so it does not have your `PATH` — which is why the
  cargo script sets one explicitly.
- **`xcodebuild`** is the command-line form of all of the above. Everything
  `just` does, it does through `xcodebuild`, which is why nothing in this project
  requires opening Xcode.

**XcodeGen** reads `project.yml` and writes the `.xcodeproj`. The YAML is the
source of truth; the project is a build artifact that happens to be checked into
nobody's repository.

**Deployment target versus SDK.** The SDK is what you build against (whatever
Xcode ships with); the deployment target is the oldest system you promise to run
on. Setting it to 14.0 on a machine running 26 is normal and costs nothing: it
only means the compiler warns if you use an API newer than 14.

---

## 8. Signing, entitlements, the sandbox and notarization

Four separate mechanisms that are constantly confused with each other.

**Code signing** attaches a cryptographic signature to the bundle. Every
executable that runs on macOS is signed, including yours: with no certificate,
Xcode signs **ad-hoc** (identity `-`), which is valid locally and means nothing
to anyone else's Mac. That is all a development build needs.

**Entitlements** are a list of capabilities in the signature — "may use the
camera", "may open network connections", "is sandboxed". They are requests
granted by the system, not settings.

**The App Sandbox** is one entitlement, and the consequential one. It confines
the process to a container directory: its own files, plus whatever the user
picks in an open panel. **Child processes inherit it**, and there is no
entitlement that lets a child out.

That is the whole reason a terminal cannot go on the Mac App Store, which
requires the sandbox. Your shell would not be able to read `~/projects`, write a
file, or reach your git credentials. The product would not work, and no amount of
engineering changes that (PRD-mac §11).

**Gatekeeper and notarization.** When someone downloads an app, macOS checks it
was signed by a known developer and that Apple has seen it. **Notarization** is
that second half: you upload the signed app, Apple scans it for malware and
returns a ticket, and `stapler` attaches the ticket to the bundle. Without it the
first launch on someone else's Mac shows a dialog saying the app cannot be
opened.

Notarization requires a **Developer ID** certificate ($99/year) and the
**Hardened Runtime** — a set of restrictions (no unsigned executable memory, no
`DYLD_*` injection, library validation) that the terminal has no trouble with,
which is exactly why it is switched on from the first Release build rather than
discovered later.

**None of this is needed to develop.** `just build`, `just run` and `just test`
work with no Apple account at all.
