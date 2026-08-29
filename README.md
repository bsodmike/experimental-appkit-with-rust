# Crustty

A terminal emulator for macOS. A Rust engine that knows terminals, an AppKit
frontend that knows pixels, and a deliberately narrow C boundary between them.

The engine has no idea macOS exists, and the frontend has no idea what a wrapped
line is. That separation is the whole design, and it is why most of this project
can be built and tested on a machine with no screen attached.

## Confessing my sins

Father, I have committed grave sin, publicly for the very first time. I have used agentic models code an AppKit frontend and lean into the "harness" — (this emdash has been manually placed as an ironic pun) generating slop, that seems to work.

However, I have been a good boy and ensured the `terminal-core` rust crate isn't too ugly, although I have not spent as much time cleaning it up, as I normally would when working as an _artisanal rust coder_.

I hope this will not affect my rust street-cred and since this slop has generated a working terminal, you will absolve me of my transgressions against the rust community.

## What it does today

One window, one shell, drawing a real terminal: colours, wide characters,
combining marks, full-screen programs, reflow as you drag the window edge,
bracketed paste, and a configuration file you can reload without relaunching.

Not yet: mouse selection and copy, scrollback beyond the visible screen, tabs.

## Architecture

The shape is an hourglass. Everything that decides anything lives at the top or
the bottom; the waist is thirteen C functions wide.

```
   ┌───────────────────────────────────────────────────────────┐
   │  native/macos/Sources        Objective-C++, AppKit          │
   │    NSView, drawRect:, NSEvent, NSTextInputClient            │
   │    translates. Decides nothing.                             │
   ├───────────────────────────────────────────────────────────┤
   │  native/macos/Glue           plain C++17, no AppKit         │
   │    key mapping, colour resolution, cell metrics,            │
   │    config parsing, the frame protocol, the RAII handle      │
   │    compiles and is tested on Linux                          │
   ├══════════════════ the C ABI ══════════════════════════════─┤
   │  crates/terminal-ffi         13 extern "C" functions        │
   │    opaque handle, repr(C) structs, byte buffers,            │
   │    one callback. Catches every panic.                       │
   ├───────────────────────────────────────────────────────────┤
   │  crates/terminal-pty         POSIX                          │
   │    the pty, the shell process, the reader thread            │
   ├───────────────────────────────────────────────────────────┤
   │  crates/terminal-core        pure Rust, no platform         │
   │    VT parser, screen, scrollback, reflow, key encoding      │
   └───────────────────────────────────────────────────────────┘
```

### How the halves interoperate

The frontend never calls the engine directly. It holds an opaque pointer and
passes it back, and data crosses by being **copied into buffers the caller owns**
— so there is no second lifetime for either side to get wrong.

```
   SHELL OUTPUT going up                    KEYSTROKES going down
   ─────────────────────                    ─────────────────────

   zsh writes to fd 1                       keyDown: NSEvent
        │                                        │
        ▼                                        ▼
   kernel line discipline                   Glue::map_key
        │                                        │  a key? text? a Cmd
        ▼                                        ▼
   pty master readable                      terminal_send_key ──┐
        │                                                       │
        ▼                                    ┌──────────────────┘
   reader thread wakes                       │  crosses the C ABI
   (terminal-pty)                            ▼
        │                                   keys.rs encodes it
        ▼                                   against the current modes
   Session::feed  ── takes the lock              │
        │                                        ▼
        ├─► VtParser: bytes → commands      write to the pty master
        ├─► Screen: commands → cells             │
        └─► replies owed, written back           ▼
        │                                   kernel line discipline
        ▼                                        │
   dirty flag false → true                       ▼
   ONCE per burst                            zsh's stdin
        │
        ▼
   wake-up callback ── crosses the C ABI
        │
        ▼
   dispatch_async(main queue)
        │
        ▼
   drawRect: → terminal_copy_frame ── one lock, one frame
        │
        ▼
   Core Text draws runs at column × cellWidth
```

Three details in there are load-bearing:

- **The reader thread parses.** Parsing is unbounded work driven by whatever a
  program decides to print, so it never happens on the UI thread.
- **The wake-up is coalesced.** Only the clean-to-dirty edge asks for a redraw, so
  `cat` of a large file costs one repaint rather than thousands.
- **A frame is copied under one lock**, so it can never tear between the top of
  the screen and the bottom.

## Repository layout

```
crates/terminal-core     the engine, pure Rust
crates/terminal-pty      pty, child process, reader thread
crates/terminal-ffi      the C boundary; builds the staticlib and the header
native/macos             the AppKit app: Glue (C++), Sources (Obj-C++), XcodeGen
docs                     the PRDs, the concepts primer, and the ADRs
blog-articles            long-form write-ups of the design
logs                     where a traced run writes; gitignored
```

## Developing with Claude Code in a container

This project was built with [Claude Code](https://github.com/anthropics/claude-code)
running inside a devcontainer, and the arrangement is worth reproducing if you
want to work the same way: **the container has no Xcode and no AppKit, and that
is the point.** Everything Claude can build is everything that does not need a
Mac — which, by design, is most of this repository.

The two sides share one directory through a bind mount, so nothing is ever
copied, pushed or pulled between them. You edit and test in the container; you
build and run the app on the host; both are looking at the same files.

```
   ┌──────────────────────────────────────────────────────────────┐
   │  macOS host, Apple Silicon                                   │
   │                                                              │
   │    your terminal                                             │
   │      just build      just run       just release             │
   │      just test-mac   just xcode     just notarize            │
   │                                                              │
   │    Xcode, XcodeGen, codesign, the window server              │
   │                                                              │
   │    ~/workspace ──────────────┐                               │
   └──────────────────────────────┼───────────────────────────────┘
                                  │  bind mount
                                  │  one directory, not a copy
   ┌──────────────────────────────┼───────────────────────────────┐
   │  devcontainer, linux/arm64   ▼                               │
   │                          /workspace                          │
   │                                                              │
   │    Claude Code                                               │
   │      cargo test      just test-glue     just smoke           │
   │      just fmt        just lint                               │
   │                                                              │
   │    rustup, a C++ compiler. No Xcode, no AppKit, no Metal.    │
   │    Network: an allowlist, not the internet.                  │
   └──────────────────────────────────────────────────────────────┘
```

### Setting it up

**1. Take the devcontainer from the Claude Code repository.**

```sh
git clone https://github.com/anthropics/claude-code.git
mkdir -p ~/workspace
cp -r claude-code/.devcontainer ~/workspace/
```

`~/workspace` is the folder you will open in VS Code. Anything inside it is
visible to the container; anything outside it is not.

**2. Check the mount targets `/workspace`.** In `.devcontainer/devcontainer.json`:

```json
"workspaceMount": "source=${localWorkspaceFolder},target=/workspace,type=bind,consistency=delegated",
"workspaceFolder": "/workspace"
```

`${localWorkspaceFolder}` is whichever folder you opened, so opening
`~/workspace` puts it at `/workspace` inside. Keeping that path identical on both
sides is what makes the instructions in this README work in either place.

**3. Clone this repository underneath it.**

```sh
cd ~/workspace
git clone <this-repo> crustty
```

It is then `~/workspace/crustty` on the Mac and `/workspace/crustty` in the
container — the same directory, twice.

**4. Open the folder in VS Code and choose "Reopen in Container".** On Apple
Silicon the image builds natively for `arm64`; do not force `linux/amd64` unless
you enjoy watching cargo run under emulation.

**5. Install the Rust toolchain inside the container.** It is not in the image:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

`sh.rustup.rs` and `static.rust-lang.org` are already in the firewall allowlist.
If `cargo fetch` later hangs or fails to resolve, add `index.crates.io` and
`static.crates.io` to the `for domain in ...` list in
`.devcontainer/init-firewall.sh` and rebuild — the container's egress is an
allowlist, so anything not named there is simply unreachable.

**6. Bootstrap the host separately.** The Mac needs its own tools, and they are
not shared with the container:

```sh
cd ~/workspace/crustty     # the same directory, from the host side
just doctor
just bootstrap
```

### Which side runs what

| In the container | On the macOS host |
|---|---|
| `just test-rust` — the whole Rust workspace | `just build`, `just run`, `just watch` |
| `just test-glue` — the frontend's platform-free core | `just test-mac` — the XCTest suite |
| `just smoke` — the C boundary end to end | `just xcode` — breakpoints and Instruments |
| `just fmt`, `just lint` | `just release`, `just dmg`, `just notarize` |

The division is not arbitrary. `Glue/` is plain C++ with no AppKit precisely so
that the frontend's decisions — key mapping, colour resolution, cell metrics, the
frame protocol — can be tested on the Linux side. What is left for the Mac is the
part that genuinely needs a screen.

### Do not try to escape the box

The container cannot build the app, and it should stay that way. Mounting the
host's Xcode into it, or running Claude directly on the host to get at
`xcodebuild`, gives up the isolation that makes the arrangement safe — an agent
with a restricted network and no access to anything outside one directory.

The friction is small and deliberate: when a change needs the app rebuilt, switch
to your host terminal, `cd` to the same directory, and run `just run` there. The
files are already saved, because there is only one copy of them.

## Setup

### Requirements

- Rust (stable) — everything under `crates/`
- A C++ compiler — the frontend's platform-free core, on any OS
- **macOS only:** Xcode (not just the Command Line Tools), plus
  [XcodeGen](https://github.com/yonaskolb/XcodeGen) and
  [just](https://github.com/casey/just)

```sh
just doctor      # what is installed, what is missing, and where it came from
just bootstrap   # installs only what is missing; never duplicates an existing just
```

`just doctor` is worth running first on a Mac: `/usr/bin/xcodebuild` exists even
with no Xcode installed, so "command not found" is not the error you get when
Xcode is merely unselected. `just check-xcode` prints the exact fix.

### Development and testing

Most of the project needs no Mac. The engine, the C boundary and the frontend's
decision-making all build and test anywhere:

```sh
just test-rust     # the Rust workspace: engine, pty, boundary
just test-glue     # the frontend's platform-free core, linked against the real staticlib
just smoke         # a C program driving the whole boundary end to end
```

On a Mac, add the app:

```sh
just build         # cargo, then XcodeGen, then xcodebuild (Debug)
just run           # build and launch, with the engine's trace going to ./logs
just watch         # rebuild and relaunch on any change
just test          # everything above, plus the XCTest suite
just xcode         # generate the project and open it, for breakpoints
```

Before committing, in this order — the discipline is described in
[`docs/PRD.md`](docs/PRD.md) §19:

```sh
just fmt
just lint          # clippy, warnings treated as defects
just test
```

`just --list` prints every recipe with what it does.

### Release builds

```sh
just release             # Release build, signed ad-hoc: runs on this Mac
just release-universal   # arm64 + x86_64, combined with lipo
```

Ad-hoc signing needs no Apple account. To ship it to another machine you need a
Developer ID certificate and notarisation:

```sh
just release-signed      # needs native/macos/Local.xcconfig
just dmg
just notarize
```

Hardened Runtime is on in Release from the first build, so nothing about
notarisation is a surprise later. The App Sandbox is deliberately off, and
`native/macos/Crustty.entitlements` says why: a sandboxed process's children
inherit the sandbox, which would leave the shell unable to read your home
directory. See [`native/macos/README.md`](native/macos/README.md) for the
certificate setup.

## Configuration

One file, in Ghostty's format, at `~/.config/crustty/config`. ⌘R reloads it.

```ini
font-family = SF Mono
font-size = 15

background   = #2d2a2e
foreground   = #fcfcfa
cursor-color = #ff6188

palette-1 = #ff6188
palette-2 = #a9dc76

shell = /opt/homebrew/bin/fish
option-is-meta = true
```

`native/macos/crustty.example.conf` documents every key. Mistakes are shown on
screen with their line numbers rather than silently ignored.

## Watching the loop

The engine traces itself, and the trace is the clearest way to understand what a
terminal actually does:

```sh
just run     # one terminal
just logs    # another
```

```
INFO terminal-pty: shell started on a new pty program=/bin/zsh pid=13324 rows=24 cols=80
INFO feed{seq=1}: terminal-core::session: read from the pty bytes=42 preview=\e]0;~\a%
INFO feed{seq=1}: terminal-core::vt: decoded fed=42 commands=6 printable=12 pending=0
INFO feed{seq=1}: terminal-core::screen: applied applied=6 cursor=0,12
INFO feed{seq=1}: terminal-core::session: waking the ui
INFO terminal-core::render: frame copied for the ui runs=1 text=12 cursor=0,12
```

Each chunk read from the shell gets a `feed{seq=N}` span, so one burst can be
followed from the file descriptor to the repaint. `RUST_LOG` filters by crate or
module: `RUST_LOG=terminal-core::vt=info`.

## Documentation

|                                                              |                                                                                         |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| [`docs/PRD.md`](docs/PRD.md)                                 | The Rust half: the boundary, threading, the buffer model, and why each is what it is    |
| [`docs/PRD-mac.md`](docs/PRD-mac.md)                         | The native half: metrics, the draw path, event routing, distribution                    |
| [`docs/PRD-mac-01-concepts.md`](docs/PRD-mac-01-concepts.md) | The macOS concepts those decisions rest on, for someone who has never written a Mac app |
| [`docs/adrs/`](docs/adrs)                                    | Architecture decision records, one per load-bearing choice                              |
| [`native/macos/README.md`](native/macos/README.md)           | Building, configuring and debugging the app                                             |
| [`blog-articles/`](blog-articles)                            | A six-part series on the design: the TTY, the buffer model, Unicode, the C boundary, testing, and threading |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option. This is the convention across the Rust ecosystem, and it means
the crates here can be depended on by projects under either licence.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work shall be dual licensed as above, without any
additional terms or conditions.
