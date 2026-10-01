//! Materialize the current generated effect vocabulary for native GHC checks.
//!
//! The Rust declarations remain authoritative. This copies their generated
//! modules into an isolated caller-owned directory for plain GHC consumers.

use std::fs;
use std::path::{Path, PathBuf};

#[path = "../../facade/src/actor_host/effect_vocabulary.rs"]
mod effect_vocabulary;

fn copy_module(source_root: &Path, output_root: &Path, relative: &str) {
    let source = source_root.join("Tidepool").join(relative);
    let output = output_root.join("Tidepool").join(relative);
    fs::create_dir_all(output.parent().expect("module parent"))
        .expect("create generated module directory");
    fs::copy(&source, &output).unwrap_or_else(|error| {
        panic!("copy generated effect module {}: {error}", source.display())
    });
}

fn main() {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: effect-support <isolated-output-directory>"),
    );
    let mut declarations = effect_vocabulary::exomonad_effect_declarations();
    declarations.push(tidepool_mcp::recipe_check_decl());
    let generated = tidepool_mcp::ensure_effects_module(&declarations)
        .expect("materialize current Exomonad effects");
    fs::create_dir_all(&output).expect("create effect support output");

    for relative in ["Effects/Core.hs", "Effects/Authored.hs"] {
        copy_module(&generated.core, &output, relative);
    }
    for relative in ["Effects.hs", "Orchestrate.hs"] {
        copy_module(&generated.shim, &output, relative);
    }

    let core = fs::read_to_string(output.join("Tidepool/Effects/Core.hs"))
        .expect("read generated Core module");
    assert!(
        core.contains("commandSourceCapture :: CommandSourceCapture"),
        "generated Commands vocabulary is missing source_capture"
    );
    println!("generated current effect support in {}", output.display());
}
