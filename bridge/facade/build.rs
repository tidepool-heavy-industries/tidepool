//! Embed only the standard library owned by the generic Tidepool facade.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-env-changed=TIDEPOOL_EMBED_HASKELL");
    println!("cargo:rerun-if-env-changed=TIDEPOOL_BUILD_SOURCE_ROOT");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let workspace = std::env::var_os("TIDEPOOL_BUILD_SOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&manifest).join("../.."));
    let root = workspace
        .join("bridge/haskell/lib")
        .canonicalize()
        .expect("Haskell stdlib source root");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let identity = if std::env::var_os("TIDEPOOL_EMBED_HASKELL").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        tidepool_toolchain::source_bundle::emit_bundle(
            &root,
            "EMBEDDED_STDLIB",
            &output.join("embedded_stdlib.rs"),
        )
        .expect("emit standard library source bundle");
        None
    } else {
        tidepool_toolchain::source_bundle::write_if_changed(
            &output.join("embedded_stdlib.rs"),
            "pub static EMBEDDED_STDLIB: &[(&str, &str)] = &[];\n",
        )
        .expect("emit empty source bundle");
        println!("cargo:rerun-if-changed={}", root.display());
        Some(
            tidepool_toolchain::cache::source_roots_identity(
                b"tidepool-stdlib-dev-source-identity",
                &[root],
            )
            .expect("read standard library source identity"),
        )
    };
    tidepool_toolchain::source_bundle::write_if_changed(
        &output.join("dev_source_identity.rs"),
        &format!("pub(crate) const DEV_SOURCE_IDENTITY: Option<&str> = {identity:?};\n"),
    )
    .expect("emit standard library source identity");
}
