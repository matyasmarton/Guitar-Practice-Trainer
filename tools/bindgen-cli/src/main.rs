// Minimal UniFFI 0.28 library-mode binding generator (Kotlin).
// Replaces the absent `uniffi-bindgen` CLI binary for 0.28.
//
// Usage: gtt-bindgen <library.dylib> <out_dir>

use anyhow::Result;
use camino::Utf8PathBuf;
use uniffi::KotlinBindingGenerator;
use uniffi_bindgen::cargo_metadata::CrateConfigSupplier;
use uniffi_bindgen::library_mode::generate_bindings;
use uniffi_bindgen::BindgenCrateConfigSupplier;

fn main() -> Result<()> {
    let lib = std::env::args()
        .nth(1)
        .expect("usage: gtt-bindgen <library> <out_dir>");
    let out = std::env::args()
        .nth(2)
        .expect("usage: gtt-bindgen <library> <out_dir>");
    let library_path = Utf8PathBuf::from_path_buf(lib.into()).unwrap();
    let out_dir = Utf8PathBuf::from_path_buf(out.into()).unwrap();

    // Build a config supplier from cargo metadata so uniffi.toml (package name
    // etc.) is discovered for the crate.
    let metadata = cargo_metadata::MetadataCommand::new().exec()?;
    let supplier = CrateConfigSupplier::from(metadata);

    let generator = KotlinBindingGenerator;
    generate_bindings(
        &library_path,
        None,
        &generator,
        &supplier as &dyn BindgenCrateConfigSupplier,
        None,
        &out_dir,
        true,
    )?;
    println!("bindings written to {out_dir}");
    Ok(())
}
