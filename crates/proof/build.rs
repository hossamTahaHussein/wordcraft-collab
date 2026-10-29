//! Compresses the bundled Arabic dictionary (`assets/proofing/ar`) into the build directory; the
//! proofing crate includes it and inflates it the first time an Arabic word is checked.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default()).join("../../assets/proofing/ar");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    for name in ["ar.aff", "ar.dic"] {
        let src = root.join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        // A checkout without the dictionary builds without Arabic spelling (no marks).
        let data = std::fs::read(&src).unwrap_or_default();
        let packed = if data.is_empty() { Vec::new() } else { miniz_oxide::deflate::compress_to_vec(&data, 9) };
        if let Err(e) = std::fs::write(out.join(format!("{name}.z")), packed) {
            println!("cargo:warning=couldn't write the Arabic dictionary: {e}");
        }
    }
}
