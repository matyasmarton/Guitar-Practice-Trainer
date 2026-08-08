// Build script for the guitar_trainer_core crate.
//
// UniFFI 0.28 proc-macro mode does NOT require UDL scaffolding generation
// here: `uniffi::setup_scaffolding!()` in `lib.rs` emits the metadata, and
// `uniffi-bindgen generate --library <lib>` reads it directly from the compiled
// artifact. This build.rs only declares rerun triggers.
fn main() {
    println!("cargo:rerun-if-changed=./src/ffi.rs");
    println!("cargo:rerun-if-changed=./src/engine.rs");
    println!("cargo:rerun-if-changed=./src/config.rs");
}