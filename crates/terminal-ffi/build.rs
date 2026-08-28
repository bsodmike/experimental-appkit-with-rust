//! Generate the C header from the Rust source (PRD §14).
//!
//! A hand-maintained header is a silent-corruption hazard: if Rust says `u32`
//! and the header says `uint16_t`, nothing warns you — you get garbage
//! arguments at runtime, intermittently. So the header is generated here and
//! the copy in `include/` is committed only so Xcode has something to point at.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let header = crate_dir.join("include").join("terminal.h");

    match cbindgen::generate(&crate_dir) {
        Ok(bindings) => {
            bindings.write_to_file(&header);
        }
        Err(e) => {
            // A header that cannot be generated must fail the build loudly:
            // silently keeping the stale one is the drift this exists to stop.
            panic!("cbindgen failed to generate {}: {e}", header.display());
        }
    }
}
