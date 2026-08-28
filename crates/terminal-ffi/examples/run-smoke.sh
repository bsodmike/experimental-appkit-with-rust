#!/bin/sh
# Build the static library and the C smoke test, then run it. This is the
# pipeline of PRD §14 in miniature: cargo produces the .a, cbindgen produces
# the header, and a C compiler links the two.
set -e
cd "$(dirname "$0")/../../.."
cargo build -p terminal-ffi
LIBS=$(cargo rustc -q -p terminal-ffi --lib -- --print native-static-libs 2>&1 |
    sed -n 's/^note: native-static-libs: //p' | tail -1)
cc -Wall -Wextra -Werror -o target/ffi-smoke \
    crates/terminal-ffi/examples/smoke.c \
    -Icrates/terminal-ffi/include \
    target/debug/libterminal_ffi.a ${LIBS}
exec target/ffi-smoke
