use proc_macro2::TokenStream;
use quote::quote;
use syn::LitStr;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Expands the `haskell_eval!` macro.
///
/// Accepts `.cbor` paths (embedded directly) or `.hs` paths (compiled via
/// `nix run .#tidepool-extract` at proc-macro expansion time).
pub fn expand(input: TokenStream) -> TokenStream {
    let path_lit = match syn::parse2::<LitStr>(input) {
        Ok(lit) => lit,
        Err(err) => return err.to_compile_error(),
    };

    let raw_path = path_lit.value();

    if raw_path.ends_with(".cbor") {
        expand_cbor(&path_lit)
    } else if raw_path.ends_with(".hs") || raw_path.contains(".hs::") {
        expand_hs(&path_lit, &raw_path)
    } else {
        syn::Error::new(path_lit.span(), "haskell_eval! path must end in .cbor or .hs")
            .to_compile_error()
    }
}

fn expand_cbor(path_lit: &LitStr) -> TokenStream {
    quote! {
        {
            static __CBOR: &[u8] = include_bytes!(#path_lit);
            let __expr = core_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction (cargo xtask extract)");
            let mut __heap = core_eval::heap::VecHeap::new();
            let __env = core_eval::env::Env::new();
            core_eval::eval::eval(&__expr, &__env, &mut __heap)
        }
    }
}

fn expand_hs(path_lit: &LitStr, raw_path: &str) -> TokenStream {
    // Parse optional ::binding suffix
    let (hs_path_str, binding_name) = match raw_path.split_once(".hs::") {
        Some((prefix, binding)) => (format!("{}.hs", prefix), Some(binding.to_string())),
        None => (raw_path.to_string(), None),
    };

    // Resolve absolute paths
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(d) => d,
        Err(_) => {
            return syn::Error::new(path_lit.span(), "CARGO_MANIFEST_DIR not set")
                .to_compile_error();
        }
    };
    let abs_hs_path = Path::new(&manifest_dir).join(&hs_path_str);
    if !abs_hs_path.exists() {
        return syn::Error::new(
            path_lit.span(),
            format!("Haskell source not found: {}", abs_hs_path.display()),
        )
        .to_compile_error();
    }

    let basename = abs_hs_path
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap();
    let output_dir = Path::new(&manifest_dir)
        .join("target")
        .join("tidepool-cbor")
        .join(basename);

    // Find flake root (where flake.nix lives)
    let flake_root = match find_flake_root(Path::new(&manifest_dir)) {
        Some(r) => r,
        None => {
            return syn::Error::new(
                path_lit.span(),
                "Could not find flake.nix in any parent directory",
            )
            .to_compile_error();
        }
    };

    // Compile via nix
    let result = Command::new("nix")
        .args([
            "run",
            &format!("{}#tidepool-extract", flake_root.display()),
            "--",
        ])
        .arg(abs_hs_path.to_str().unwrap())
        .arg("--output-dir")
        .arg(output_dir.to_str().unwrap())
        .output();

    match result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return syn::Error::new(
                path_lit.span(),
                format!(
                    "nix run tidepool-extract failed (exit {}):\n{}",
                    output.status, stderr
                ),
            )
            .to_compile_error();
        }
        Err(e) => {
            return syn::Error::new(
                path_lit.span(),
                format!("Failed to run nix: {}. Is nix installed?", e),
            )
            .to_compile_error();
        }
    }

    // Find the target .cbor file
    let cbor_path = match binding_name {
        Some(ref name) => {
            let p = output_dir.join(format!("{}.cbor", name));
            if !p.exists() {
                let available = list_bindings(&output_dir);
                return syn::Error::new(
                    path_lit.span(),
                    format!(
                        "Binding '{}' not found. Available: {:?}",
                        name, available
                    ),
                )
                .to_compile_error();
            }
            p
        }
        None => match find_single_binding(&output_dir) {
            Ok(p) => p,
            Err(msg) => {
                return syn::Error::new(path_lit.span(), msg).to_compile_error();
            }
        },
    };

    let cbor_path_str = cbor_path.to_str().unwrap();
    let hs_abs_str = abs_hs_path.to_str().unwrap();

    quote! {
        {
            const _: &[u8] = include_bytes!(#hs_abs_str);
            static __CBOR: &[u8] = include_bytes!(#cbor_path_str);
            let __expr = core_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction");
            let mut __heap = core_eval::heap::VecHeap::new();
            let __env = core_eval::env::Env::new();
            core_eval::eval::eval(&__expr, &__env, &mut __heap)
        }
    }
}

/// Expands the `haskell_expr!` macro.
///
/// Returns `(CoreExpr, DataConTable)` without evaluating. For `.cbor` paths,
/// expects a sibling `meta.cbor`. For `.hs` paths, the meta.cbor is produced
/// alongside binding CBORs by tidepool-extract.
pub fn expand_expr(input: TokenStream) -> TokenStream {
    let path_lit = match syn::parse2::<LitStr>(input) {
        Ok(lit) => lit,
        Err(err) => return err.to_compile_error(),
    };

    let raw_path = path_lit.value();

    if raw_path.ends_with(".cbor") {
        expand_expr_cbor(&path_lit)
    } else if raw_path.ends_with(".hs") || raw_path.contains(".hs::") {
        expand_expr_hs(&path_lit, &raw_path)
    } else {
        syn::Error::new(path_lit.span(), "haskell_expr! path must end in .cbor or .hs")
            .to_compile_error()
    }
}

fn expand_expr_cbor(path_lit: &LitStr) -> TokenStream {
    // For .cbor paths, expect meta.cbor in the same directory
    let cbor_path = path_lit.value();
    let cbor_dir = Path::new(&cbor_path)
        .parent()
        .expect("cbor path has no parent");
    let meta_path = cbor_dir.join("meta.cbor");
    let meta_path_str = meta_path.to_str().unwrap();

    quote! {
        {
            static __CBOR: &[u8] = include_bytes!(#path_lit);
            static __META: &[u8] = include_bytes!(#meta_path_str);
            let __expr = core_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR");
            let __table = core_repr::serial::read::read_metadata(__META)
                .expect("failed to deserialize metadata");
            (__expr, __table)
        }
    }
}

fn expand_expr_hs(path_lit: &LitStr, raw_path: &str) -> TokenStream {
    // Parse optional ::binding suffix
    let (hs_path_str, binding_name) = match raw_path.split_once(".hs::") {
        Some((prefix, binding)) => (format!("{}.hs", prefix), Some(binding.to_string())),
        None => (raw_path.to_string(), None),
    };

    // Resolve absolute paths
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(d) => d,
        Err(_) => {
            return syn::Error::new(path_lit.span(), "CARGO_MANIFEST_DIR not set")
                .to_compile_error();
        }
    };
    let abs_hs_path = Path::new(&manifest_dir).join(&hs_path_str);
    if !abs_hs_path.exists() {
        return syn::Error::new(
            path_lit.span(),
            format!("Haskell source not found: {}", abs_hs_path.display()),
        )
        .to_compile_error();
    }

    let basename = abs_hs_path
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap();
    let output_dir = Path::new(&manifest_dir)
        .join("target")
        .join("tidepool-cbor")
        .join(basename);

    // Find flake root
    let flake_root = match find_flake_root(Path::new(&manifest_dir)) {
        Some(r) => r,
        None => {
            return syn::Error::new(
                path_lit.span(),
                "Could not find flake.nix in any parent directory",
            )
            .to_compile_error();
        }
    };

    // Compile via nix — use --target for whole-module mode when binding specified
    let mut cmd = Command::new("nix");
    cmd.args([
        "run",
        &format!("{}#tidepool-extract", flake_root.display()),
        "--",
    ]);
    cmd.arg(abs_hs_path.to_str().unwrap());
    cmd.arg("--output-dir");
    cmd.arg(output_dir.to_str().unwrap());
    if let Some(ref name) = binding_name {
        cmd.arg("--target");
        cmd.arg(name);
    }
    let result = cmd.output();

    match result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return syn::Error::new(
                path_lit.span(),
                format!(
                    "nix run tidepool-extract failed (exit {}):\n{}",
                    output.status, stderr
                ),
            )
            .to_compile_error();
        }
        Err(e) => {
            return syn::Error::new(
                path_lit.span(),
                format!("Failed to run nix: {}. Is nix installed?", e),
            )
            .to_compile_error();
        }
    }

    // Find the target .cbor file
    let cbor_path = match binding_name {
        Some(ref name) => {
            let p = output_dir.join(format!("{}.cbor", name));
            if !p.exists() {
                let available = list_bindings(&output_dir);
                return syn::Error::new(
                    path_lit.span(),
                    format!(
                        "Binding '{}' not found. Available: {:?}",
                        name, available
                    ),
                )
                .to_compile_error();
            }
            p
        }
        None => match find_single_binding(&output_dir) {
            Ok(p) => p,
            Err(msg) => {
                return syn::Error::new(path_lit.span(), msg).to_compile_error();
            }
        },
    };

    let cbor_path_str = cbor_path.to_str().unwrap();
    let hs_abs_str = abs_hs_path.to_str().unwrap();
    let meta_path = output_dir.join("meta.cbor");
    let meta_path_str = meta_path.to_str().unwrap();

    quote! {
        {
            const _: &[u8] = include_bytes!(#hs_abs_str);
            static __CBOR: &[u8] = include_bytes!(#cbor_path_str);
            static __META: &[u8] = include_bytes!(#meta_path_str);
            let __expr = core_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction");
            let __table = core_repr::serial::read::read_metadata(__META)
                .expect("failed to deserialize metadata");
            (__expr, __table)
        }
    }
}

fn find_flake_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("flake.nix").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn list_bindings(output_dir: &Path) -> Vec<String> {
    std::fs::read_dir(output_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "cbor"))
        .filter(|p| p.file_stem().map_or(false, |s| s != "meta"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}

fn find_single_binding(output_dir: &Path) -> Result<PathBuf, String> {
    let entries: Vec<PathBuf> = std::fs::read_dir(output_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map_or(false, |ext| ext == "cbor"))
                .filter(|p| p.file_stem().map_or(false, |s| s != "meta"))
                .collect()
        })
        .unwrap_or_default();

    match entries.len() {
        0 => Err("No .cbor bindings produced by tidepool-extract".to_string()),
        1 => Ok(entries.into_iter().next().unwrap()),
        _ => {
            let names: Vec<String> = entries
                .iter()
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect();
            Err(format!(
                "Multiple bindings found: {:?}. Use haskell_eval!(\"path.hs::binding_name\")",
                names
            ))
        }
    }
}
