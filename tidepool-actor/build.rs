//! Embeds the shipped Shoal example workspace's worked Haskell checks and
//! skill sections so `src/usage_pointer.rs` can derive a "see it used here"
//! index without a hand-maintained file list — see that module's doc comment.
//! Re-globs on every build; adding, removing, or editing a file under either
//! directory changes the generated index automatically.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.join("..");
    let checks_dir = workspace_root.join("examples/shoal-workspace/.shoal/checks");
    let skills_dir = workspace_root.join("examples/shoal-workspace/.shoal/skills");

    println!("cargo:rerun-if-changed={}", checks_dir.display());
    println!("cargo:rerun-if-changed={}", skills_dir.display());

    let mut entries: Vec<(String, PathBuf)> = Vec::new();

    if let Ok(read_dir) = fs::read_dir(&checks_dir) {
        let mut paths: Vec<PathBuf> = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "hs"))
            .collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            entries.push((format!(".shoal/checks/{name}"), path));
        }
    }

    if let Ok(read_dir) = fs::read_dir(&skills_dir) {
        let mut skill_dirs: Vec<PathBuf> = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        skill_dirs.sort();
        for skill_dir in skill_dirs {
            let skill_md = skill_dir.join("SKILL.md");
            if skill_md.is_file() {
                let name = skill_dir
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                entries.push((format!("skill: {name}"), skill_md));
            }
        }
    }

    let mut generated =
        String::from("pub(crate) static SHOAL_USAGE_SOURCES: &[(&str, &str)] = &[\n");
    for (locator, path) in &entries {
        println!("cargo:rerun-if-changed={}", path.display());
        let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        generated.push_str(&format!("    ({locator:?}, include_str!({absolute:?})),\n"));
    }
    generated.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("shoal_usage_sources.rs");
    fs::write(dest, generated).unwrap();
}
