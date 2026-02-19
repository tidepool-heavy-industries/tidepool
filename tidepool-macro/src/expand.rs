use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

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

    if let Err(msg) = run_tidepool_extract(
        &abs_hs_path,
        &output_dir,
        binding_name.as_deref(),
        Path::new(&manifest_dir),
    ) {
        return syn::Error::new(path_lit.span(), msg).to_compile_error();
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

    if let Err(msg) = run_tidepool_extract(
        &abs_hs_path,
        &output_dir,
        binding_name.as_deref(),
        Path::new(&manifest_dir),
    ) {
        return syn::Error::new(path_lit.span(), msg).to_compile_error();
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

// ─── haskell_inline! support ───────────────────────────────────────────────

/// Parsed input for `haskell_inline! { target = "name", include = "dir", r#"..."# }`
struct InlineInput {
    target: String,
    includes: Vec<String>,
    source: LitStr,
}

impl Parse for InlineInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse: target = "name"
        let target_ident: syn::Ident = input.parse()?;
        if target_ident != "target" {
            return Err(syn::Error::new(target_ident.span(), "expected `target`"));
        }
        input.parse::<Token![=]>()?;
        let target_lit: LitStr = input.parse()?;
        let target = target_lit.value();
        input.parse::<Token![,]>()?;

        // Parse optional: include = "dir" or include = ["d1", "d2"]
        let mut includes = Vec::new();
        if input.peek(syn::Ident) {
            let maybe_include = input.fork();
            let ident: syn::Ident = maybe_include.parse()?;
            if ident == "include" {
                // Consume from real stream
                let _: syn::Ident = input.parse()?;
                input.parse::<Token![=]>()?;
                if input.peek(syn::token::Bracket) {
                    let content;
                    syn::bracketed!(content in input);
                    while !content.is_empty() {
                        let lit: LitStr = content.parse()?;
                        includes.push(lit.value());
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                } else {
                    let lit: LitStr = input.parse()?;
                    includes.push(lit.value());
                }
                let _ = input.parse::<Token![,]>();
            }
        }

        // Parse optional Haskell source body
        let source = if input.is_empty() {
            LitStr::new("", proc_macro2::Span::call_site())
        } else {
            let _ = input.parse::<Token![,]>();
            if input.is_empty() {
                LitStr::new("", proc_macro2::Span::call_site())
            } else {
                input.parse()?
            }
        };

        Ok(InlineInput {
            target,
            includes,
            source,
        })
    }
}

/// Expands `haskell_inline!` — writes inline Haskell to a temp file, compiles,
/// returns `(CoreExpr, DataConTable)`.
pub fn expand_inline(input: TokenStream) -> TokenStream {
    let parsed = match syn::parse2::<InlineInput>(input) {
        Ok(p) => p,
        Err(err) => return err.to_compile_error(),
    };

    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(d) => d,
        Err(_) => {
            return syn::Error::new(parsed.source.span(), "CARGO_MANIFEST_DIR not set")
                .to_compile_error();
        }
    };

    // Capitalize target -> module name (e.g. "game" -> "Game")
    let module_name = capitalize(&parsed.target);

    // Resolve include dirs to absolute paths
    let abs_includes: Vec<PathBuf> = parsed
        .includes
        .iter()
        .map(|d| Path::new(&manifest_dir).join(d))
        .collect();

    // Read include files and strip their module headers/pragmas
    let mut include_bodies = String::new();
    for dir in &abs_includes {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().map_or(false, |ext| ext == "hs") {
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        include_bodies.push_str(&strip_module_header(&content));
                        include_bodies.push('\n');
                    }
                }
            }
        }
    }

    // Build single-module source: header + included definitions + user code
    let source_text = parsed.source.value();
    let full_source = format!(
        "{{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}}\nmodule {} where\nimport Control.Monad.Freer\n{}\n{}",
        module_name, include_bodies, source_text
    );

    // Write to target/tidepool-inline/<Module>.hs
    let inline_dir = Path::new(&manifest_dir)
        .join("target")
        .join("tidepool-inline");
    if let Err(e) = std::fs::create_dir_all(&inline_dir) {
        return syn::Error::new(
            parsed.source.span(),
            format!("Failed to create {}: {}", inline_dir.display(), e),
        )
        .to_compile_error();
    }
    let hs_file = inline_dir.join(format!("{}.hs", module_name));
    if let Err(e) = std::fs::write(&hs_file, &full_source) {
        return syn::Error::new(
            parsed.source.span(),
            format!("Failed to write {}: {}", hs_file.display(), e),
        )
        .to_compile_error();
    }

    // Output dir for CBOR
    let output_dir = Path::new(&manifest_dir)
        .join("target")
        .join("tidepool-cbor")
        .join(&module_name);

    if let Err(msg) = run_tidepool_extract(
        &hs_file,
        &output_dir,
        Some(&parsed.target),
        Path::new(&manifest_dir),
    ) {
        return syn::Error::new(parsed.source.span(), msg).to_compile_error();
    }

    // Find CBOR output
    let cbor_path = output_dir.join(format!("{}.cbor", parsed.target));
    if !cbor_path.exists() {
        let available = list_bindings(&output_dir);
        return syn::Error::new(
            parsed.source.span(),
            format!(
                "Binding '{}' not found after compilation. Available: {:?}",
                parsed.target, available
            ),
        )
        .to_compile_error();
    }

    let cbor_path_str = cbor_path.to_str().unwrap();
    let meta_path = output_dir.join("meta.cbor");
    let meta_path_str = meta_path.to_str().unwrap();
    let hs_path_str = hs_file.to_str().unwrap();

    // Track include dir .hs files for recompilation
    let include_tracks: Vec<TokenStream> = abs_includes
        .iter()
        .filter_map(|dir| {
            std::fs::read_dir(dir).ok().map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map_or(false, |ext| ext == "hs"))
                    .map(|p| {
                        let s = p.to_str().unwrap().to_string();
                        quote! { const _: &[u8] = include_bytes!(#s); }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect();

    quote! {
        {
            const _: &[u8] = include_bytes!(#hs_path_str);
            #(#include_tracks)*
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

/// Strip module header, language pragmas, and import lines from a Haskell source file.
/// Returns only the body (data declarations, function definitions, etc.).
fn strip_module_header(source: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut past_header = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if !past_header {
            // Skip language pragmas, module declarations, and imports
            if trimmed.starts_with("{-#")
                || trimmed.starts_with("module ")
                || trimmed.starts_with("import ")
                || trimmed.is_empty()
            {
                continue;
            }
            past_header = true;
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Run `tidepool-extract` to compile a Haskell source file.
///
/// Tries the tool from PATH first (normal workflow inside `nix develop`),
/// falls back to `nix run {flake}#tidepool-extract` if not found.
fn run_tidepool_extract(
    hs_path: &Path,
    output_dir: &Path,
    target: Option<&str>,
    manifest_dir: &Path,
) -> Result<(), String> {
    // Try tidepool-extract directly from PATH first
    let mut cmd = Command::new("tidepool-extract");
    cmd.arg(hs_path);
    cmd.arg("--output-dir");
    cmd.arg(output_dir);
    if let Some(name) = target {
        cmd.arg("--target");
        cmd.arg(name);
    }

    match cmd.output() {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(_) | Err(_) => {
            // tidepool-extract not on PATH or failed — fall back to nix run
        }
    }

    // Fall back: find flake root and use nix run
    let flake_root = find_flake_root(manifest_dir).ok_or_else(|| {
        "tidepool-extract not found on PATH and no flake.nix in any parent directory".to_string()
    })?;

    let mut cmd = Command::new("nix");
    cmd.args([
        "run",
        &format!("{}#tidepool-extract", flake_root.display()),
        "--",
    ]);
    cmd.arg(hs_path);
    cmd.arg("--output-dir");
    cmd.arg(output_dir);
    if let Some(name) = target {
        cmd.arg("--target");
        cmd.arg(name);
    }

    match cmd.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "nix run tidepool-extract failed (exit {}):\n{}",
                output.status, stderr
            ))
        }
        Err(e) => Err(format!(
            "Failed to run nix: {}. Is nix installed?",
            e
        )),
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
