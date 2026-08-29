# The Buffer Model: Why Scrollback Cannot Be a Grid

Drag the edge of a terminal window and the text rewraps. Long lines that were
broken across three rows become two. Nothing is lost, nothing is truncated, and
your selection — if the terminal is good — is still highlighting the same words
it was before.

That is a much harder thing than it looks, and whether it is possible at all was
decided long before any code was written, by how the text is stored.

This is the second article about building a terminal emulator. The
[first](post-01-designing-the-architecture-of-a-tty-with-appkit-and-rust.md)
covered what a TTY is and how the pieces fit together. This one is about the data
structure underneath it, and the thesis is:

> **Reflow is not a feature you add. It is a consequence of a storage decision
> you already made.**

---

## The obvious design, and where it dies

A terminal displays a grid. So store a grid: `rows × cols` cells, each with a
character and its colours. Scrollback is the same thing continued upward — a long
list of rows, each exactly `cols` wide.

This is clean, it is fast, and it is what a terminal without reflow does. It
survives until the first time someone resizes the window.

Here is a line of output, as stored at width 20:

```
   ┌────────────────────┐
   │the quick brown fox │
   │jumps over the lazy │
   │dog                 │
   └────────────────────┘
```

Now the window is dragged wider, to 40 columns. What should appear is one line
broken in two. What you have is three rows of twenty characters, and no
information about which of those breaks were *decisions by the program* and
which were merely *where the edge of the window happened to be*.

You cannot tell them apart afterwards. The moment you stored "the quick brown fox
" as a row, you threw away the fact that it did not end there.

Some terminals paper over this with a "wrapped" flag per row, and then join rows
back together at resize time. That works, and it is how you end up rebuilding
your entire scrollback buffer on every drag of the mouse.

## Logical lines

The alternative is to store what the program actually wrote.

A **logical line** is the text between two explicit newlines. It has no width; it
is as long as it is. Display rows are not stored at all — they are *derived* by
wrapping a logical line to whatever the window is now:

```
   logical line 41:  "the quick brown fox jumps over the lazy dog"
                      │
        width 20      ├──► "the quick brown fox "
                      ├──► "jumps over the lazy "
                      └──► "dog"

        width 40      ├──► "the quick brown fox jumps over the lazy "
                      └──► "dog"

        width 80      └──► "the quick brown fox jumps over the lazy dog"
```

Same bytes, three different displays, no data moved. The `wrapped` flag still
exists, but it has changed from *stored state* into an *output* of the wrapping
computation — which is the whole difference. State can be wrong. An output cannot
disagree with its input.

And crucially, a display row is an **index, not a copy**: a `(line_id, offset,
length)` triple pointing into the line. Reflowing recomputes indices. It never
moves cell data. That is the difference between reflow being expensive and reflow
being cheap, and it is decided by the storage layout rather than by the reflow
algorithm.

## But the live screen is not that

Here is where the tidy version of this story stops being true, and where the
interesting decision lives.

If logical lines are so good, store everything as logical lines. Except the
active screen is not a log of text — it is a *mutation surface*. Look at what a
program does to it:

```
   ESC [ 12 ; 40 H     put the cursor at row 12, column 40
   ESC [ 2 K           clear this line
   ESC [ 5 L           insert 5 blank lines here, push the rest down
   ESC [ 3 ; 20 r      only rows 3-20 scroll from now on
```

Every one of those is trivial against a rectangle you can address by row and
column. Every one of them is miserable against a rope of logical lines, because
"row 12" is a question you can only answer by wrapping every line above it first,
and "insert a blank line" does not correspond to anything a logical line has.

So the design is a **hybrid**, and it has a boundary in the middle:

```
   ┌─────────────────────────────────────────────────────────┐
   │  SCROLLBACK: logical lines, packed, immutable           │
   │                                                         │
   │    line 39  "cargo build --release"                     │
   │    line 40  "   Compiling terminal-core v0.1.0"         │
   │    line 41  "the quick brown fox jumps over the lazy…"  │
   │                                                         │
   ├───────────── rows freeze as they scroll off ────────────┤
   │                                                         │
   │  ACTIVE SCREEN: an owned grid, rows × cols              │
   │                                                         │
   │    row 0   [t][h][e][ ][q][u][i][c][k][ ]...            │
   │    row 1   [j][u][m][p][s][ ][o][v][e][r]...            │
   │    row 2   [$][ ][_][ ][ ][ ][ ][ ][ ][ ]...            │
   │                                                         │
   │    each row tagged: { line_id, wrapped }                │
   └─────────────────────────────────────────────────────────┘
```

The cost of logical lines is paid **only at the boundary** — when a row scrolls
off the top, it is converted and appended to its logical line — and during
reflow, which is exactly when you are willing to pay it. In between, the cursor
writes into a plain array.

The rows carry `{ line_id, wrapped }` so that the grid can be turned back into
logical lines when reflow needs it. A logical line on screen is simply a run of
consecutive rows sharing a `line_id`.

## The straddling line

That boundary creates one edge case, and it is worth looking at because it is the
kind of thing that only appears once you commit to a design.

A long line is being printed. Its first rows scroll off the top while its last row
is still on screen and still being written to. Half of it is frozen history and
half is live:

```
                  ┌──────────────────────────────┐
   scrollback     │ line 41 (head, frozen)        │
                  │  "the quick brown fox jumps " │
                  └──────────────────────────────┘
   ═══════════════════════ boundary ══════════════════
                  ┌──────────────────────────────┐
   active grid    │ row 0: "over the lazy dog and"│  line_id 41
                  │ row 1: "  more text still be" │  line_id 41
                  │ row 2: "ing written here_"    │  line_id 41  ← cursor
                  └──────────────────────────────┘
```

Three options: refuse to split a line (the grid stops being a rectangle), copy the
whole line back down (unbounded work at an arbitrary moment), or allow the split
and rejoin on demand.

The third is what this does. At most **one** line is ever in this state — the
topmost visible one — and reflow reconstructs it by concatenating head and tail
whenever it needs the whole thing. The frozen head is safe to treat as immutable
for a reason that falls out of how terminals work: **the cursor cannot address
above the viewport.** No escape sequence can reach back and edit text that has
scrolled off. So the head cannot change, and head-plus-tail can never disagree.

The active grid stays a clean fixed rectangle, which is the property the whole
hybrid exists to protect.

## Two coordinate systems

Reflow means text moves. Anything that remembers a position has to remember it in
terms that survive the move.

There are therefore two ways to name a character, and confusing them is how
selection breaks:

```
   Before resize (width 20)          After resize (width 40)

   row 1, col 5 ─────┐               row 0, col 25 ─────┐
                     ▼                                  ▼
   "the quick brown fox "            "the quick brown fox jumps over the lazy "
   "jumps over the lazy "            "dog"
   "dog"

   DISPLAY  (1, 5)  ✗ now names a different character
   LOGICAL  (line 41, byte 25)  ✓ names the same 'o' in "over", either way
```

| | Display coordinate | Logical coordinate |
|---|---|---|
| Shape | `(row, col)` in the viewport | `(line_id, byte_offset)` |
| Survives a resize | **No** | **Yes** |
| Used for | drawing, mouse hit-testing, cursor reports | selection anchors, scroll position |

And one rule that matters more than it looks:

> **`line_id` is a monotonic counter, never an array index.**

Scrollback evicts its oldest lines. If `line_id` were an index, every eviction
would shift every subsequent index, and every stored selection anchor would
quietly start pointing at the wrong text. With a counter that only ever increases,
an anchor into an evicted line is *detectably stale* rather than silently wrong —
and "wrong in a way you can detect" is the entire difference between a bug you fix
and a bug you never find.

The frontend never sees any of this. It says "the drag started at cell (6, 51)"
and "draw me the visible rows". The conversion happens inside the engine, which
keeps the boundary from the first article intact: the frontend knows about pixels
and cells, the engine knows about text.

## What it costs in memory

Scrollback is capped at 100,000 logical lines. That number decides the
representation, because the obvious one does not survive it.

A `Cell` that can hold a grapheme cluster needs to hold a string — `e` plus a
combining acute is one cell and does not fit in a `char`. Do that as a heap
`String` per cell and an 80-column line costs eighty allocations before you count
the text. Multiply by 100,000 lines:

```
   Vec<Cell> with a heap String per cell
      80 cells × 100k lines × (24 bytes struct + 24 bytes alloc + text)
      = hundreds of megabytes, and millions of live allocations

   packed UTF-8 + attribute runs
      "the quick brown fox jumps over the lazy dog"   ← one String
      [{ byte_start: 0,  byte_len: 43, fg: default, bg: default, attrs: 0 }]
      = a few megabytes total
```

So a scrollback line is one packed UTF-8 string plus a list of attribute runs:

```rust
pub struct AttrRun {
    pub byte_start: u32,
    pub byte_len: u32,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttrs,
}
```

Real terminal output is overwhelmingly one style at a time, so the run list is
usually one or two entries for an entire line.

There is a second reason this shape was chosen, and it is the better one: **it is
already what the renderer wants.** The frame handed to the frontend is runs of
styled text slicing a shared buffer. Scrollback is stored in nearly that form, so
the read path is close to a memcpy rather than a conversion. The storage decision
and the rendering decision turned out to be the same decision, which is usually a
sign you have found the right one.

The active grid is different — it is being mutated constantly, so it stays cells —
but its cells use an inline string type that stores clusters up to 24 bytes
without allocating. Every real grapheme cluster fits. The heap is never touched
in the write path.

Two caps, not one: 10,000 lines by default (100,000 maximum), **and** a total
byte ceiling. A line count alone is not a memory bound, because a single logical
line is unbounded — one `cat` of a file with no newlines would be one line and all
of your RAM. Eviction removes whole oldest lines from the front of a deque; lines
are never split or truncated.

## Making resize cost nothing

Wrapping a line means scanning it and accumulating display width until the row is
full. Doing that for 100,000 lines on every mouse-move during a window drag is not
viable.

So each line caches where its rows begin, for one width:

```rust
struct WrapCache {
    width: u16,
    /// Byte offsets where display rows 2..N begin; row 1 is implicitly at 0.
    continuation_starts: Vec<u32>,
}
```

The interesting part is the invalidation, which is that **there isn't any**. A
resize does no work on scrollback at all. Each line notices for itself, the next
time anything asks it to wrap:

```rust
let stale = !matches!(&self.wrap_cache, Some(c) if c.width == width);
```

Cached at the width you are asking for? Use it. Cached at some other width?
Recompute, and cache that instead. Dragging a window from 80 columns to 120
invalidates 100,000 cached lines by doing nothing whatsoever to any of them, and
the cost is paid only by lines someone actually looks at.

The empty `Vec` for a line that fits on one row performs no allocation, which is
the common case by a wide margin.

## What must survive a rewrap

Three things, and naming them is what makes the design testable:

- **The cursor.** Re-derived from its logical position, so it stays on the same
  character rather than the same coordinates.
- **The selection.** Anchored logically, which is the entire reason for
  `(line_id, offset)`.
- **The viewport.** If you have scrolled up 200 lines, you should still be looking
  at the same text afterwards, not at wherever 200 rows now lands.

And one thing that deliberately does not reflow: **the alternate screen** — the
full-screen buffer `vim` and `less` run in. A full-screen program owns its own
layout and repaints completely when it gets `SIGWINCH`. Rewrapping its contents
would corrupt a display it is about to redraw anyway. Reflow applies to the
primary screen and its scrollback, and nowhere else.

## What is not built

Two things, both deliberate and both recorded as deferred rather than forgotten.

**Growing the window's height pads with blank rows** at the bottom instead of
pulling lines back down out of scrollback. Getting that right needs the viewport
anchor above, which is the same machinery scrolling back will need, so it waits
for that.

**The background maintenance thread does not exist.** The design has one whose job
is to fill wrap caches proactively so that scrolling back through a large history
never stalls. Today every cache is filled lazily, on demand. That is correct, and
it will be visibly slower the first time someone scrolls through 100,000 lines.

Saying so is worth more than implying a finished system. The design has room for
both; the code has neither yet.

## The thesis, again

None of this is a reflow algorithm. The reflow algorithm is about fifteen lines:
regroup rows by `line_id`, concatenate, wrap to the new width, re-derive the
cursor.

Everything difficult was decided earlier, in the shape of the data:

- storing logical lines rather than rows made reflow *possible*
- deriving display rows as indices rather than copies made it *cheap*
- keeping the active screen a grid kept the write path *simple*
- packing scrollback made 100,000 lines *affordable*
- a monotonic `line_id` made selection anchors *safe across eviction*
- and caching wrap points per width made a window drag *free*

Each of those is a property of a struct definition, not of a function. By the time
you are writing the code that rewraps text, the interesting question has already
been answered — well or badly — by the layout you chose.

Which is the general lesson, and the reason this article exists: in systems like
this, the data structure is the design. The algorithms are what is left over.

---

*Next in this series: [Unicode in a Grid](post-03-unicode-in-a-grid.md) —
grapheme clusters, double-width characters, and why the engine refuses to let a
wide character share a run with its neighbours.*
