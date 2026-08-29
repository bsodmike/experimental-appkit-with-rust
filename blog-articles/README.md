# The series

Six articles on building a terminal emulator: a Rust engine, a C boundary, an
AppKit frontend. They are drawn from the design documents in
[`../docs`](../docs), which are more pedantic and less readable.

| | | |
|---|---|---|
| 1 | [Designing the Architecture of a TTY with AppKit and Rust](post-01-designing-the-architecture-of-a-tty-with-appkit-and-rust.md) | What a TTY is, why `CR` and `LF` are two characters, and how a pty makes a shell believe in a terminal that does not exist |
| 2 | [The Buffer Model: Why Scrollback Cannot Be a Grid](post-02-the-buffer-model-why-scrollback-cannot-be-a-grid.md) | What happens to your text when you drag the window edge, and why the storage layout decides it |
| 3 | [Unicode in a Grid](post-03-unicode-in-a-grid.md) | Grapheme clusters, double-width characters, and agreeing about cell boundaries with no protocol for doing so |
| 4 | [The C Boundary: Thirteen Functions Wide](post-04-the-c-boundary.md) | Four shapes that may cross, an API that looks right and is unsound, and a header defect a generator caught |
| 5 | [Testing What You Cannot Compile](post-05-testing-what-you-cannot-compile.md) | Verifying a macOS frontend on Linux, and reproducing a Mac-only failure in a container |
| 6 | [Two Threads and a Flag](post-06-two-threads-and-a-flag.md) | One lock, one atomic boolean, and a shutdown that cannot leave a thread writing into freed memory |

They can be read in any order, though each assumes the vocabulary of the ones
before it.
