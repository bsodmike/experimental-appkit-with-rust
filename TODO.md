No slice is half-finished. PRD and the five ADRs are current, so the next step is a choice rather than a continuation:

- Selection state (§5, §17 #15) — needs terminal_pointer_event; the engine already has the line_id anchors it depends on
- Damage tracking (§5) — which rows changed since the last frame, so redraws get cheaper
- Scrolled-back viewport — rendering above the live rows, plus the §16.5 reflow maintenance thread (it joins the same shutdown)
- The AppKit frontend (§13) — Obj-C++ is the PRD's recommendation, and it can't be tested from here

Verification

Here, before the user builds anything:

- cargo test --workspace — the existing 338 plus the new environment tests
- just test-glue — the C++ core, compiled with g++ and linked against libterminal_ffi.a
- just smoke — the existing C program end to end

On the Mac, in this order:

1.  just doctor — reports the toolchain and what is missing
2.  just bootstrap — brew install just xcodegen watchexec
3.  just test — cargo tests, glue tests, then xcodebuild test
4.  just run — the app launches, ls produces output, vim redraws, ⌘V pastes, ⌘Q exits clean
5.  just xcode — the project opens, breakpoints work
6.  just release — an ad-hoc signed .app in native/macos/build that launches from Finder,
    with Hardened Runtime already on, so nothing about notarization is a surprise later
7.  codesign -dv --entitlements - native/macos/build/Release/Terminal.app — confirms sandbox off
    and the hardened runtime flag, without needing a certificate

The README carries the same list as a checklist, plus the failure modes worth naming: cargo
not on Xcode's PATH, a stale generated project, and a first launch with no TERM.
