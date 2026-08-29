# Unicode in a Grid

A terminal is a grid of fixed-width cells. Unicode is not built for grids.

Almost everything difficult about displaying text in a terminal comes from that
one sentence. Not from the parsing, not from the escape sequences, not from the
rendering — from the fact that the abstraction a terminal offers ("a character
goes in a cell") stopped being true around 1991 and nobody updated the protocol.

This is the third article about building a terminal emulator. The
[first](post-01-designing-the-architecture-of-a-tty-with-appkit-and-rust.md) was
about what a TTY is, the
[second](post-02-the-buffer-model-why-scrollback-cannot-be-a-grid.md) about how
the text is stored. This one is about what actually goes in a cell, and it has a
single recurring theme:

> **Every layer must agree on where the cell boundaries are, or the display
> desyncs — and the program on the other end has no way to tell you that it has.**

---

## What is in a cell?

Start with the obvious answer and watch it fail three times.

**A byte?** No — a terminal is UTF-8, and `é` is two bytes. Store bytes and `é`
occupies two cells.

**A `char`, then** — a Unicode scalar value?

```
   e + U+0301 (combining acute)   →   é
```

That is one thing on screen and two `char`s. Store `char`s and the accent lands
in the cell to the right of the letter it belongs to. Worse, `👨‍👩‍👧‍👦` — a family
emoji — is *seven* scalar values joined by zero-width joiners, and would occupy
seven cells while drawing as one picture.

**A grapheme cluster, then.** A user-perceived character, as defined by Unicode
Annex #29. That is the right unit, and it is what this terminal stores:

```rust
pub struct Cell {
    /// The grapheme cluster shown in this cell, as UTF-8. A single space
    /// denotes an empty cell.
    pub content: CompactString,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttrs,
}
```

A string per cell, which sounds alarming until you notice the type: `CompactString`
stores up to 24 bytes inline, with no heap allocation. Every real grapheme cluster
fits — the family emoji is 25 bytes and is the exception that proves the
rule — so the write path never touches the heap.

But grapheme clusters only solve *what* goes in a cell. They say nothing about how
many cells it takes.

## The second problem: width

Unicode has a property called East Asian Width, and it is where a grid meets its
match. `漢` is double-width: it occupies two cells. So does an emoji. So does a
flag. Nothing in the byte stream announces this — the terminal is expected to
know, and the program at the other end is *also* expected to know, and they had
better agree.

Consider what happens if they do not. A shell prints a prompt containing an emoji,
believing it to be two columns wide. The terminal renders it as one. Every
subsequent character on that line is now one column to the left of where the shell
thinks it is. The shell redraws the line to correct a typo, using cursor
positioning that assumes its own layout, and the result is garbage — not because
either side is buggy, but because they disagreed about one character.

There is no protocol for resolving this. There is no acknowledgement, no
negotiation, no error. The only defence is to compute width the way everyone else
computes it.

## One authority, used by everything

So width and segmentation are computed in exactly one place, and it is worth
saying why that is a correctness property rather than tidiness.

Two different pieces of code need to know where cells begin: the **write path**,
which places characters as they arrive, and the **reflow scan**, which decides
where a stored line breaks when the window resizes. If those two disagree by even
one character, text lands in one place and rewraps to another.

```rust
/// Split a string into extended grapheme clusters (UAX #29) — one per cell.
pub fn graphemes(s: &str) -> impl Iterator<Item = &str>

/// How many columns a cluster occupies: 0, 1, or 2.
pub fn grapheme_width(cluster: &str) -> u16
```

Everything goes through those. Not "should" — there is no second implementation
to drift from the first.

The width function is not simply a call into a Unicode table, because the table
does not answer the question a terminal is asking:

```rust
if cluster.contains(VS16_EMOJI) { return 2; }   // U+FE0F: "draw this as emoji"
if cluster.contains(VS15_TEXT)  { return 1; }   // U+FE0E: "draw this as text"
if cluster.contains(ZWJ)        { return 2; }   // a joined sequence is one glyph
if is_regional_indicator(base) && count >= 2 { return 2; }   // a flag
base.width().unwrap_or(0).min(2)
```

Each of those lines is a case where `unicode-width` alone gives an answer a
terminal cannot use:

- **Variation selectors** change the answer for the same base character. `☂` is
  narrow; `☂️` — the same umbrella with U+FE0F after it — is an emoji and takes two
  cells. The selector is invisible and zero-width, and it changes the layout.
- **ZWJ sequences** are several emoji joined into one glyph. Summing the parts
  gives eight; the answer is two.
- **Regional indicators** are letters that become a flag in pairs. `🇬` and `🇧`
  separately are two narrow-ish symbols; `🇬🇧` together is one flag, two cells.

And one policy decision, which every terminal has to make and none can make
correctly: **East Asian *ambiguous* characters are treated as width 1.** Greek
letters, box-drawing characters, some accented Latin — Unicode says their width
depends on context that a terminal does not have. Everyone picks a default and
documents it. Picking the other one breaks different things.

## Writing a wide character

Placing a two-column cluster into a one-cell-per-column grid needs somewhere to
put the second column. This terminal writes a **spacer**: a cell with empty
content, carrying the same colours as its partner.

```
   printing "a漢b" into a grid:

   col:      0     1     2     3
           ┌─────┬─────┬─────┬─────┐
           │ "a" │ "漢"│ ""  │ "b" │
           └─────┴─────┴─────┴─────┘
                    └──── the spacer: no content, same style
```

Empty content rather than a space, and the distinction earns its keep three times:

- when the line is **packed into scrollback**, the spacer contributes no bytes —
  the stored text is `a漢b`, not `a漢 b`
- when a run is **built for the renderer**, it adds a column and no characters
- and it is **distinguishable from a real space**, which matters when something
  overwrites the first half of a wide character and the second half must not be
  left behind as a stray blank

## Where it gets subtle: the last column

A wide character needs two columns. What happens when the cursor is on the last
one?

It cannot be split across a line break, so it wraps early, and the column it
leaves behind stays empty. That is the correct behaviour and it is also visible:
a terminal displaying CJK text at an odd width has a ragged right edge, by one
column, on some lines. That is not a bug. That is the only correct answer.

Which brings up the strangest rule in terminal emulation, and one that is nothing
to do with Unicode — but you cannot get wide characters right without it.

**The deferred wrap.** When a character lands in the final column, the cursor does
*not* move to the next line. It stays where it is, and a flag is set. Only the
*next* printable character performs the wrap:

```
   width 4, printing "abcd" then "e"

   after "abcd":   [a][b][c][d]        cursor on col 3, wrap armed
                              ▲
   after "e":      [a][b][c][d]        the wrap happens now
                   [e][ ][ ][ ]
                    ▲
```

Get this wrong — wrap immediately when the last column is filled — and a line that
exactly fills the width leaves a blank line after it. Every terminal that has ever
existed does the deferred version, `vim` depends on it, and it is the single most
common source of "why is there a gap in my output".

The flag is on the cursor, and an explicit cursor movement clears it: if a program
positions the cursor somewhere, it has said where it wants to be, and a pending
wrap from three characters ago is no longer anybody's business.

## Combining marks attach backwards

A zero-width cluster is not a cell. It belongs to the cell before it.

```
   printing "e" then U+0301:

   after "e":       [ "e" ]
   after U+0301:    [ "é" ]      the same cell, its content extended
```

The write path appends the mark to the previous cell's content rather than
advancing the cursor. Which means a `Cell` cannot hold a fixed-size character even
in principle — the content grows after the fact, and there is no bound on how many
marks a program may stack onto one base character.

## Why the renderer never learns any of this

The frontend draws runs of styled text at grid positions. It uses Core Text to
shape them, because a combining mark has to be shaped together with the character
it attaches to — you cannot draw `e` and then draw an acute accent and expect them
to meet.

But shaping produces glyphs positioned by the *font*, and the grid needs them
positioned by *column*. So the renderer takes the shaped line apart and places each
glyph itself, asking: which grapheme cluster did this glyph come from? Cluster
index gives column offset.

That mapping is only valid while every cluster in a run occupies one column. So the
engine guarantees exactly that:

```
   "a漢b" becomes THREE runs, not one:

     run 0   col 0   cols 1   "a"
     run 1   col 1   cols 2   "漢"     ← alone, because it is two columns wide
     run 2   col 3   cols 1   "b"
```

A double-width cluster is never merged into a run with its neighbours. It costs a
few more runs in CJK text, and it buys something worth much more: **the renderer
never needs to know how wide a character is.** It counts clusters. Width stays
entirely inside the engine, where there is one authority for it, where it is
tested, and where the write path and the reflow scan are guaranteed to agree.

The alternative was to ship per-cluster column widths across the boundary — more
data, on every frame, so that the renderer could re-derive something the engine
already knew. The engine declining to create the problem was cheaper than solving
it.

## What this all buys

The tests that matter here read like a list of things that used to break in
terminals, and mostly still do somewhere:

- `e` + combining acute is one cell, and one cell wide
- a ZWJ family emoji is one cell, two columns
- `🇬🇧` is one cell, two columns; `🇬` alone is not two
- `☂` is one column, `☂️` is two
- a zero-width space is zero columns and attaches to nothing
- a wide character at the last column wraps early and leaves the column blank
- a line that exactly fills the width does not leave a blank line after it

None of that is visible when it works. All of it is glaring when it does not — a
prompt that smears, a redraw that leaves debris, a box-drawing frame with one
corner a column out. Terminal bugs are almost never subtle to look at, and almost
always subtle to cause.

The underlying reason is worth stating plainly. The terminal protocol assumes a
character occupies one cell. Unicode has not worked that way for thirty years. The
gap between those two facts is where a terminal emulator lives, and closing it
means agreeing — silently, with no protocol and no acknowledgement — with every
other program about exactly where the cell boundaries are.

---

*Next in this series: the C boundary — four shapes that may cross, why a pointer
into the grid is a bug you cannot write a lifetime for, and the header defect that
a generator caught on its first run.*
