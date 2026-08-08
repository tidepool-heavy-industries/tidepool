use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A resolved local `.hs` input: its canonicalized path and content.
type HsDep = (PathBuf, Vec<u8>);
/// A path that couldn't be read, paired with the underlying io error.
type PathReadError = (PathBuf, std::io::Error);

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
        syn::Error::new(
            path_lit.span(),
            "haskell_eval! path must end in .cbor or .hs",
        )
        .to_compile_error()
    }
}

fn expand_cbor(path_lit: &LitStr) -> TokenStream {
    quote! {
        {
            static __CBOR: &[u8] = include_bytes!(#path_lit);
            let __expr = tidepool_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction (cargo xtask extract)");
            let mut __heap = tidepool_eval::heap::VecHeap::new();
            let __env = tidepool_eval::env::Env::new();
            tidepool_eval::eval::eval(&__expr, &__env, &mut __heap)
        }
    }
}

/// Shared front-end for the `hs!`/`expr_hs!` macros: parse an optional
/// `.hs::binding` suffix, resolve the `.hs` path against `CARGO_MANIFEST_DIR`,
/// run the extractor, and locate the target `.cbor`. Returns
/// `(abs_hs_path, cbor_path, output_dir, transitive_deps)`, or a
/// compile-error `TokenStream` to be returned verbatim from the caller.
/// `transitive_deps` is the sorted set of local `.hs` files (beyond the entry
/// file itself) that the entry file's `import`s resolve to — see
/// `resolve_transitive_hs_deps`.
fn resolve_hs_path(
    path_lit: &LitStr,
    raw_path: &str,
) -> Result<(PathBuf, PathBuf, PathBuf, Vec<PathBuf>), TokenStream> {
    // Parse optional ::binding suffix
    let (hs_path_str, binding_name) = match raw_path.split_once(".hs::") {
        Some((prefix, binding)) => (format!("{}.hs", prefix), Some(binding.to_string())),
        None => (raw_path.to_string(), None),
    };

    // Resolve absolute paths
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(d) => d,
        Err(_) => {
            return Err(
                syn::Error::new(path_lit.span(), "CARGO_MANIFEST_DIR not set").to_compile_error(),
            );
        }
    };
    let abs_hs_path = Path::new(&manifest_dir).join(&hs_path_str);
    if !abs_hs_path.exists() {
        return Err(syn::Error::new(
            path_lit.span(),
            format!("Haskell source not found: {}", abs_hs_path.display()),
        )
        .to_compile_error());
    }

    let basename = abs_hs_path.file_stem().unwrap().to_str().unwrap();
    let src_bytes = match std::fs::read(&abs_hs_path) {
        Ok(b) => b,
        Err(e) => {
            return Err(syn::Error::new(
                path_lit.span(),
                format!("Failed to read {}: {}", abs_hs_path.display(), e),
            )
            .to_compile_error());
        }
    };

    // `run_tidepool_extract` never passes `--include`, so the extractor's GHC
    // session resolves every non-entry import against `importPaths = ["."]`
    // (the extractor subprocess's cwd, inherited unchanged from this process
    // — see `extractionDynFlags` in haskell/src/Tidepool/GhcPipeline.hs). The
    // cache key must walk the SAME search order or it can miss a real input.
    //
    // RESIDUAL GAP: `[cwd]` below is that search order ONLY because no
    // includes are passed today — that coupling is not enforced by anything
    // in this function. See the `run_tidepool_extract` doc comment before
    // ever adding `--include` support to either this macro or that call.
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            return Err(syn::Error::new(
                path_lit.span(),
                format!("failed to resolve current directory for Haskell import search: {e}"),
            )
            .to_compile_error());
        }
    };
    let entry_imports = import_module_names(&String::from_utf8_lossy(&src_bytes));
    let mut seen = BTreeSet::new();
    seen.insert(
        abs_hs_path
            .canonicalize()
            .unwrap_or_else(|_| abs_hs_path.clone()),
    );
    let mut deps = match resolve_transitive_hs_deps(&entry_imports, &[cwd], &mut seen) {
        Ok(d) => d,
        Err((path, e)) => {
            return Err(syn::Error::new(
                path_lit.span(),
                format!(
                    "failed to read imported Haskell module {}: {e}",
                    path.display()
                ),
            )
            .to_compile_error());
        }
    };
    deps.sort_by(|a, b| a.0.cmp(&b.0));

    // Content-addressed cache dir: same (source bytes, transitive deps,
    // target, extractor identity) → same dir. Parallel rustc targets
    // expanding this macro converge on one result instead of clobbering a
    // shared dir. Reusing an existing dir is sound exactly to the extent this
    // key covers what the extractor actually reads: the entry file, every
    // local module transitively reachable from its `import`s (heuristic
    // textual resolution, not GHC's own module graph — see
    // `resolve_transitive_hs_deps`), the target binding, and the producer
    // identity (see `extract_identity`). `run_tidepool_extract` publishes the
    // dir with an atomic rename, so a partially-written dir is never visible
    // under the final name.
    let key = content_key(&src_bytes, binding_name.as_deref(), &deps);
    let dep_paths: Vec<PathBuf> = deps.into_iter().map(|(p, _)| p).collect();
    let output_dir = Path::new(&manifest_dir)
        .join("target")
        .join("tidepool-cbor")
        .join(format!("{basename}-{key:016x}"));

    if let Err(msg) = run_tidepool_extract(
        &abs_hs_path,
        &output_dir,
        binding_name.as_deref(),
        Path::new(&manifest_dir),
    ) {
        return Err(syn::Error::new(path_lit.span(), msg).to_compile_error());
    }

    // Find the target .cbor file
    let cbor_path = match binding_name {
        Some(ref name) => {
            let p = output_dir.join(format!("{}.cbor", name));
            if !p.exists() {
                let available = list_bindings(&output_dir);
                return Err(syn::Error::new(
                    path_lit.span(),
                    format!("Binding '{}' not found. Available: {:?}", name, available),
                )
                .to_compile_error());
            }
            p
        }
        None => match find_single_binding(&output_dir) {
            Ok(p) => p,
            Err(msg) => {
                return Err(syn::Error::new(path_lit.span(), msg).to_compile_error());
            }
        },
    };

    Ok((abs_hs_path, cbor_path, output_dir, dep_paths))
}

fn expand_hs(path_lit: &LitStr, raw_path: &str) -> TokenStream {
    let (abs_hs_path, cbor_path, _output_dir, dep_paths) = match resolve_hs_path(path_lit, raw_path)
    {
        Ok(t) => t,
        Err(e) => return e,
    };

    let cbor_path_str = cbor_path.to_str().unwrap();
    let hs_abs_str = abs_hs_path.to_str().unwrap();
    let dep_tracks: Vec<TokenStream> = dep_paths
        .iter()
        .map(|p| {
            let s = p.to_str().unwrap().to_string();
            quote! { const _: &[u8] = include_bytes!(#s); }
        })
        .collect();

    quote! {
        {
            const _: &[u8] = include_bytes!(#hs_abs_str);
            #(#dep_tracks)*
            static __CBOR: &[u8] = include_bytes!(#cbor_path_str);
            let __expr = tidepool_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction");
            let mut __heap = tidepool_eval::heap::VecHeap::new();
            let __env = tidepool_eval::env::Env::new();
            tidepool_eval::eval::eval(&__expr, &__env, &mut __heap)
        }
    }
}

// ─── haskell_inline! support ───────────────────────────────────────────────

/// Parsed input for `haskell_inline! { target = "name", include = "dir", r#"..."# }`
struct InlineInput {
    target: String,
    /// Kept as `LitStr` (not `.value()`-collapsed) so a bad include dir can
    /// be reported at the span of the literal that named it.
    includes: Vec<LitStr>,
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
                        includes.push(lit);
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                } else {
                    let lit: LitStr = input.parse()?;
                    includes.push(lit);
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

    // Validate every `include = "…"` dir up front and enumerate its `.hs`
    // files exactly once — the resulting list feeds module-name collection,
    // header splicing, AND the Cargo tracker generation below, so the three
    // consumers can never disagree about what's in a dir the way two
    // separate `read_dir` calls could.
    let include_dirs = match collect_include_dirs(Path::new(&manifest_dir), &parsed.includes) {
        Ok(d) => d,
        Err(e) => return e,
    };

    // Collect module names from included files so we can filter out inter-module imports
    let included_module_names: Vec<String> = include_dirs
        .iter()
        .flat_map(|d| &d.hs_files)
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .map(str::to_string)
        .collect();

    // Read include files, collecting their pragmas, imports, and body
    let mut all_extensions = vec![
        "GADTs".to_string(),
        "DataKinds".to_string(),
        "TypeOperators".to_string(),
        "FlexibleContexts".to_string(),
    ];
    let mut all_imports = vec!["import Control.Monad.Freer".to_string()];
    let mut include_bodies = String::new();
    for dir in &include_dirs {
        for p in &dir.hs_files {
            let content = match std::fs::read_to_string(p) {
                Ok(c) => c,
                Err(e) => {
                    return syn::Error::new(
                        dir.span,
                        format!(
                            "failed to read included Haskell module {}: {e}",
                            p.display()
                        ),
                    )
                    .to_compile_error();
                }
            };
            let header = strip_module_header(&content);
            for ext in header.extensions {
                if !all_extensions.contains(&ext) {
                    all_extensions.push(ext);
                }
            }
            for imp in header.imports {
                // Skip imports for modules being inlined
                let is_internal = included_module_names.iter().any(|m| {
                    imp.trim().starts_with(&format!("import {}", m))
                        || imp.trim().starts_with(&format!("import qualified {}", m))
                });
                if !is_internal && !all_imports.contains(&imp) {
                    all_imports.push(imp);
                }
            }
            include_bodies.push_str(&header.body);
            include_bodies.push('\n');
        }
    }

    // Build single-module source: header + included definitions + user code
    let source_text = parsed.source.value();
    let extensions_str = all_extensions.join(", ");
    let imports_str = all_imports.join("\n");
    let full_source = format!(
        "{{-# LANGUAGE {} #-}}\nmodule {} where\n{}\n{}\n{}",
        extensions_str, module_name, imports_str, include_bodies, source_text
    );

    // Same import-search mirroring as `resolve_hs_path`: `run_tidepool_extract`
    // never passes `--include` for the inline path either, so any import
    // left in `all_imports` after filtering out inlined siblings resolves
    // only against the extractor subprocess's cwd. Splicing already folds
    // every included file's own content into `full_source` (and thus into
    // the hash below); this closes the remaining gap — an import that
    // survives filtering and points at a local (non-package) module.
    //
    // RESIDUAL GAP: `[cwd]` below is the search order ONLY because
    // `include_dirs` (validated above) is never forwarded to
    // `run_tidepool_extract` as `--include` — it is a text-splicing input
    // here, not a GHC search path. That's an easy trap: passing
    // `include_dirs`'s paths to `run_tidepool_extract` for extra robustness
    // WITHOUT also adding them to the `roots` slice below would silently
    // reopen the exact bug this file exists to close. See the
    // `run_tidepool_extract` doc comment.
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            return syn::Error::new(
                parsed.source.span(),
                format!("failed to resolve current directory for Haskell import search: {e}"),
            )
            .to_compile_error();
        }
    };
    let remaining_imports = import_module_names(&all_imports.join("\n"));
    let mut seen = BTreeSet::new();
    let mut extra_deps = match resolve_transitive_hs_deps(&remaining_imports, &[cwd], &mut seen) {
        Ok(d) => d,
        Err((path, e)) => {
            return syn::Error::new(
                parsed.source.span(),
                format!(
                    "failed to read imported Haskell module {}: {e}",
                    path.display()
                ),
            )
            .to_compile_error();
        }
    };
    extra_deps.sort_by(|a, b| a.0.cmp(&b.0));

    // Content-addressed staging dir (same rationale as `resolve_hs_path`):
    // parallel targets expanding this macro write identical content, and the
    // tmp+rename below keeps every path GHC reads complete at all times. The
    // key covers `full_source` (which already embeds every spliced include
    // file's bytes) plus any transitively-resolved import left outside the
    // splice, the target, and the producer identity.
    let key = content_key(full_source.as_bytes(), Some(&parsed.target), &extra_deps);
    let extra_dep_paths: Vec<PathBuf> = extra_deps.into_iter().map(|(p, _)| p).collect();
    let inline_dir = Path::new(&manifest_dir)
        .join("target")
        .join("tidepool-inline")
        .join(format!("{key:016x}"));
    if let Err(e) = std::fs::create_dir_all(&inline_dir) {
        return syn::Error::new(
            parsed.source.span(),
            format!("Failed to create {}: {}", inline_dir.display(), e),
        )
        .to_compile_error();
    }
    let hs_file = inline_dir.join(format!("{}.hs", module_name));
    let hs_tmp = inline_dir.join(format!("{}.hs.tmp-{}", module_name, std::process::id()));
    if let Err(e) =
        std::fs::write(&hs_tmp, &full_source).and_then(|()| std::fs::rename(&hs_tmp, &hs_file))
    {
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
        .join(format!("{module_name}-{key:016x}"));

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

    // Track every spliced include-dir `.hs` file (the same validated list
    // used to build `full_source` above — one enumeration, so this can never
    // disagree with what was actually spliced) plus every transitively
    // resolved import left outside the splice.
    let include_tracks: Vec<TokenStream> = include_dirs
        .iter()
        .flat_map(|d| &d.hs_files)
        .chain(extra_dep_paths.iter())
        .map(|p| {
            let s = p.to_str().unwrap().to_string();
            quote! { const _: &[u8] = include_bytes!(#s); }
        })
        .collect();

    quote! {
        {
            const _: &[u8] = include_bytes!(#hs_path_str);
            #(#include_tracks)*
            static __CBOR: &[u8] = include_bytes!(#cbor_path_str);
            static __META: &[u8] = include_bytes!(#meta_path_str);
            let __expr = tidepool_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction");
            let (__table, _warnings) = tidepool_repr::serial::read::read_metadata(__META)
                .expect("failed to deserialize metadata");
            (__expr, __table)
        }
    }
}

/// Parsed header info from a Haskell source file.
struct HaskellHeader {
    /// LANGUAGE extensions found in pragmas
    extensions: Vec<String>,
    /// Import lines (verbatim)
    imports: Vec<String>,
    /// Body (everything after header)
    body: String,
}

/// Strip module header from a Haskell source file, collecting pragmas and imports.
fn strip_module_header(source: &str) -> HaskellHeader {
    let mut extensions = Vec::new();
    let mut imports = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut past_header = false;
    // True from a `module Foo` line (export list not yet closed) until a line
    // containing `where` closes it — a multi-line header
    // (`module Foo\n  ( x )\n  where`) must skip every line in between, or the
    // export-list/`where` fragment leaks into the inlined body (#F4).
    let mut in_module_clause = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if !past_header {
            if in_module_clause {
                if trimmed.contains("where") {
                    in_module_clause = false;
                }
                continue;
            }
            if trimmed.starts_with("{-#") && trimmed.contains("LANGUAGE") {
                // Extract extensions from pragma like {-# LANGUAGE Foo, Bar #-}
                if let Some(start) = trimmed.find("LANGUAGE") {
                    let after = &trimmed[start + "LANGUAGE".len()..];
                    if let Some(end) = after.find("#-}") {
                        let exts = &after[..end];
                        for ext in exts.split(',') {
                            let ext = ext.trim();
                            if !ext.is_empty() {
                                extensions.push(ext.to_string());
                            }
                        }
                    }
                }
                continue;
            }
            if trimmed.starts_with("{-#") || trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("module ") {
                if !trimmed.contains("where") {
                    in_module_clause = true;
                }
                continue;
            }
            if trimmed.starts_with("import ") {
                imports.push(line.to_string());
                continue;
            }
            past_header = true;
        }
        body_lines.push(line);
    }
    HaskellHeader {
        extensions,
        imports,
        body: body_lines.join("\n"),
    }
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
/// falls back to `nix run {flake}#tidepool-extract` only when that spawn
/// itself fails to find the binary — a binary that RAN and exited nonzero
/// (a real GHC diagnostic) is reported directly, never masked behind a
/// redundant (and slower) nix re-run that would only reproduce the same
/// error (#F3).
///
/// RESIDUAL GAP (named, not enforced): neither invocation below passes
/// `--include`, so both callers' `resolve_transitive_hs_deps` roots are
/// exactly `[cwd]` today — that's a load-bearing assumption this function's
/// signature does NOT enforce. If this function ever grows an
/// `extra_includes: &[PathBuf]` parameter to pass `--include <dir>` here,
/// EVERY caller must extend the exact same `roots` list it already passes to
/// `resolve_transitive_hs_deps` — not a separately-maintained list — or the
/// cache key silently stops covering the real input set again, which is the
/// exact bug this file exists to close. There is no compiler-enforced
/// coupling between "what GHC searches" and "what gets hashed"; whoever adds
/// `--include` here owns re-establishing it by construction (single source
/// list), not by remembering to update two places.
fn run_tidepool_extract(
    hs_path: &Path,
    output_dir: &Path,
    target: Option<&str>,
    manifest_dir: &Path,
) -> Result<(), String> {
    // The output dir name already encodes the resolved input set (entry file
    // plus every transitively resolved local import — see
    // `resolve_transitive_hs_deps`), the target, and the producer identity
    // (see `extract_identity`), so reusing an existing dir is sound to the
    // extent that key covers what the extractor actually reads; any gap is
    // a heuristic-resolution miss, not an unkeyed input (see the callers'
    // comments for what is and isn't covered). The publish rename below is
    // atomic, so a partially-written dir is never visible under the final
    // name. Concurrent expansions (e.g. `--all-targets` compiling a bin and
    // its test harness in parallel) converge on one dir instead of
    // clobbering a shared one.
    if output_dir.exists() {
        return Ok(());
    }
    let tmp_dir = tmp_sibling(output_dir);
    if let Err(e) = std::fs::remove_dir_all(&tmp_dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(format!(
                "failed to clear stale tmp dir {}: {e}",
                tmp_dir.display()
            ));
        }
    }

    // $TIDEPOOL_EXTRACT (the same override every test tier honors) wins over
    // PATH — a repo with a freshly built extract must never be trumped by a
    // stale installed one. A SET-but-unreadable $TIDEPOOL_EXTRACT is a hard
    // error here, not a silent fall-through to PATH/nix: falling through
    // would run a DIFFERENT binary than `extract_identity()` hashed into the
    // content key, a producer/key divergence. An UNSET env still falls back
    // to PATH then nix below, same as always.
    let extract_env = std::env::var_os("TIDEPOOL_EXTRACT").map(std::path::PathBuf::from);
    let extract_bin = match &extract_env {
        Some(path) => {
            if !path.is_file() {
                return Err(format!(
                    "$TIDEPOOL_EXTRACT is set to {} but that is not a readable file",
                    path.display()
                ));
            }
            path.clone()
        }
        None => std::path::PathBuf::from("tidepool-extract"),
    };
    let mut cmd = Command::new(&extract_bin);
    cmd.arg(hs_path);
    cmd.arg("--output-dir");
    cmd.arg(&tmp_dir);
    if let Some(name) = target {
        cmd.arg("--target");
        cmd.arg(name);
    }

    match cmd.output() {
        Ok(output) if output.status.success() => return publish_extract_dir(&tmp_dir, output_dir),
        Ok(output) => {
            // The binary ran and failed — this IS the diagnostic (a GHC type
            // error, a missing binding, ...). Surface it verbatim; falling
            // back to nix here would only re-run the SAME failing compile.
            return Err(format!(
                "tidepool-extract failed (exit {}):\n{}",
                output.status,
                extract_failure_text(&output.stdout, &output.stderr)
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && extract_env.is_none() => {
            // Bare "tidepool-extract" not on PATH, and $TIDEPOOL_EXTRACT was
            // never set — fall back to nix run below.
        }
        Err(e) => {
            // Either a genuine spawn failure, or $TIDEPOOL_EXTRACT was set
            // (and passed the `is_file` check above, so this is a race —
            // e.g. removed between check and spawn). Either way: fail loud,
            // never silently fall back to a different binary.
            return Err(format!("failed to spawn {}: {e}", extract_bin.display()));
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
    cmd.arg(&tmp_dir);
    if let Some(name) = target {
        cmd.arg("--target");
        cmd.arg(name);
    }

    match cmd.output() {
        Ok(output) if output.status.success() => publish_extract_dir(&tmp_dir, output_dir),
        Ok(output) => Err(format!(
            "nix run tidepool-extract failed (exit {}):\n{}",
            output.status,
            extract_failure_text(&output.stdout, &output.stderr)
        )),
        Err(e) => Err(format!("Failed to run nix: {}. Is nix installed?", e)),
    }
}

/// Stable 64-bit key for the extract content cache. `DefaultHasher` is
/// deterministic for a given toolchain, which is all a `target/`-local cache
/// needs — every rustc process in one build converges on the same directory.
/// `deps` must already be sorted by path (callers own the sort so the same
/// input set always hashes to the same key regardless of resolution order).
fn content_key(bytes: &[u8], target: Option<&str>, deps: &[HsDep]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    for (path, content) in deps {
        path.hash(&mut h);
        content.hash(&mut h);
    }
    target.hash(&mut h);
    extract_identity().hash(&mut h);
    h.finish()
}

/// Extracts the dotted module name from a single `import` line, e.g.
/// `"import qualified Data.Text as T"` -> `Some("Data.Text")`. Returns `None`
/// for non-import lines. A purely textual heuristic — it does not handle
/// CPP-generated imports, package-qualified import strings, or hs-boot
/// files.
fn import_module_name(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("import")?;
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("qualified")
        .map(str::trim_start)
        .unwrap_or(rest);
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Extracts every imported module's dotted name from a Haskell source text.
fn import_module_names(source: &str) -> Vec<String> {
    source.lines().filter_map(import_module_name).collect()
}

/// Resolves a dotted module name (`Foo.Bar`) to a `.hs` file under one of
/// `roots`, in order — the first root that has it wins, mirroring GHC's own
/// `importPaths` search order.
fn resolve_module_file(name: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let rel = format!("{}.hs", name.replace('.', "/"));
    roots.iter().map(|r| r.join(&rel)).find(|p| p.is_file())
}

/// Resolves the transitive closure of LOCAL `.hs` modules reachable from
/// `imports` by walking `roots` — the same search order the real extractor
/// uses (see the callers' comments for what `roots` is in each case). A
/// module not found under any root is a package/external import, resolved
/// by the extractor's package database rather than a local file, and
/// contributes no entry here — its identity is covered by
/// `extract_identity`, not this hash. `already_seen` is both the entry
/// point's own canonicalized path (pre-seeded by the caller so the entry
/// file is never re-hashed as its own dependency) and the growing
/// walked-set; passing it in lets independent calls within one expansion
/// (there are none today, but future callers may need to) share dedup.
///
/// A resolved path that exists but can't be read is a hard error — that IS
/// one of the extractor's real inputs, so silently dropping it would
/// reintroduce the exact silent-gap failure mode this cache key exists to
/// close.
fn resolve_transitive_hs_deps(
    imports: &[String],
    roots: &[PathBuf],
    already_seen: &mut BTreeSet<PathBuf>,
) -> Result<Vec<HsDep>, PathReadError> {
    let mut out = Vec::new();
    let mut queue: Vec<String> = imports.to_vec();
    while let Some(name) = queue.pop() {
        let Some(path) = resolve_module_file(&name, roots) else {
            continue;
        };
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !already_seen.insert(canon.clone()) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| (path.clone(), e))?;
        queue.extend(import_module_names(&String::from_utf8_lossy(&bytes)));
        out.push((canon, bytes));
    }
    Ok(out)
}

/// Recursively collects every file under `dir` for which `pred` holds,
/// sorted for determinism. An unreadable directory anywhere in the tree is a
/// hard error naming the offending path — never a silent partial scan.
fn collect_files_recursive(
    dir: &Path,
    pred: &dyn Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>, PathReadError> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| (d.clone(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| (d.clone(), e))?;
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if pred(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Identity of the Haskell→Core extractor this process will invoke — hashed
/// once per process. Folded into every content key: the address must
/// include the PRODUCER, not just the inputs, or an extractor upgrade (e.g.
/// a wire-format major bump) silently serves output in the old format from
/// an existing cache dir.
///
/// A SET-but-unreadable `$TIDEPOOL_EXTRACT` panics here rather than falling
/// back to a PATH-resolved binary: this function runs FIRST (via
/// `content_key`, before `run_tidepool_extract`'s own check), and its result
/// picks the content-addressed `output_dir`. If that dir already exists
/// (published by a past, correctly-configured run), `run_tidepool_extract`
/// short-circuits on the existence check and never reaches its own
/// fail-loud path — so silently keying against the wrong binary here would
/// let a misconfigured `$TIDEPOOL_EXTRACT` silently serve a stale/foreign
/// cache hit instead of erroring.
///
/// When neither `$TIDEPOOL_EXTRACT` nor a PATH binary resolves,
/// `run_tidepool_extract` falls back to `nix run <flake>#tidepool-extract`
/// — that IS a different producer, so the key must track it too. Actually
/// resolving the nix derivation would mean invoking nix from every macro
/// expansion just to compute a cache key, so this hashes `flake.lock` +
/// `flake.nix` + the extractor's own source inputs
/// (`haskell/{app,src}/**/*.hs`, `haskell/*.cabal`, `haskell/cabal.project*`)
/// instead — anything that changes what `nix run` would build. If even that
/// can't be resolved (no flake.nix found, or a source file can't be read),
/// this panics rather than returning a placeholder: an unresolved producer
/// identity must never be able to select — or worse, silently reuse — a
/// cache directory. Panicking inside a proc macro surfaces as a loud compile
/// error, same as any other `expect`/`panic!` in this crate.
fn extract_identity() -> u64 {
    use std::hash::{Hash, Hasher};
    use std::sync::OnceLock;
    static ID: OnceLock<u64> = OnceLock::new();
    *ID.get_or_init(|| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Same resolution order as `run_tidepool_extract`: $TIDEPOOL_EXTRACT,
        // then PATH — the key must hash the binary that will actually run.
        let resolved = match std::env::var_os("TIDEPOOL_EXTRACT") {
            Some(path) => {
                let path = std::path::PathBuf::from(path);
                if !path.is_file() {
                    panic!(
                        "$TIDEPOOL_EXTRACT is set to {} but that is not a readable file",
                        path.display()
                    );
                }
                Some(path)
            }
            None => std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|d| d.join("tidepool-extract"))
                    .find(|p| p.is_file())
            }),
        };
        match resolved {
            Some(path) => {
                let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                    panic!("failed to read extractor binary {}: {e}", path.display())
                });
                bytes.hash(&mut h);
            }
            None => {
                let manifest_dir =
                    std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
                let flake_root = find_flake_root(Path::new(&manifest_dir)).unwrap_or_else(|| {
                    panic!(
                        "tidepool-extract not found on PATH/$TIDEPOOL_EXTRACT and no flake.nix \
                         found in any parent of {manifest_dir} to resolve a nix producer identity \
                         — refusing to key the cache on an unresolved producer"
                    )
                });
                hash_nix_extractor_identity(&flake_root, &mut h);
            }
        }
        h.finish()
    })
}

/// Hashes the nix-fallback producer's identity into `h`: `flake.lock` (pins
/// nixpkgs/rust-overlay/flake-utils), `flake.nix` (the derivation
/// definition), and the extractor's own source inputs under `haskell/`. See
/// `extract_identity` for why this exists instead of resolving the actual
/// derivation. Panics (naming the path) rather than silently hashing a
/// partial or placeholder identity.
fn hash_nix_extractor_identity(flake_root: &Path, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    for name in ["flake.lock", "flake.nix"] {
        let p = flake_root.join(name);
        let bytes = std::fs::read(&p).unwrap_or_else(|e| {
            panic!(
                "failed to read {} to resolve the nix producer identity: {e}",
                p.display()
            )
        });
        bytes.hash(h);
    }
    let is_hs = |p: &Path| p.extension().is_some_and(|e| e == "hs");
    let mut sources = Vec::new();
    for sub in ["app", "src"] {
        let dir = flake_root.join("haskell").join(sub);
        if dir.is_dir() {
            let files = collect_files_recursive(&dir, &is_hs).unwrap_or_else(|(p, e)| {
                panic!(
                    "failed to enumerate extractor sources under {}: {e} (at {})",
                    dir.display(),
                    p.display()
                )
            });
            sources.extend(files);
        }
    }
    if let Ok(entries) = std::fs::read_dir(flake_root.join("haskell")) {
        for entry in entries.flatten() {
            let p = entry.path();
            let is_project_file = p.extension().is_some_and(|e| e == "cabal")
                || p.file_name().and_then(|n| n.to_str()) == Some("cabal.project");
            if is_project_file {
                sources.push(p);
            }
        }
    }
    sources.sort();
    for p in sources {
        p.hash(h);
        let bytes = std::fs::read(&p).unwrap_or_else(|e| {
            panic!(
                "failed to read {} to resolve the nix producer identity: {e}",
                p.display()
            )
        });
        bytes.hash(h);
    }
}

/// One validated `include = "…"` directory: its absolute path, the span of
/// the literal that named it (for error attribution), and its `.hs` files
/// (sorted for determinism). Built once by `collect_include_dirs` and reused
/// by every consumer (module-name collection, header splicing, Cargo
/// trackers) so they can't disagree about what's in the directory.
#[derive(Debug)]
struct IncludeDir {
    span: proc_macro2::Span,
    hs_files: Vec<PathBuf>,
}

/// Validates every `include = "…"` literal as a readable directory and
/// enumerates its `.hs` files up front. A missing/unreadable directory, or a
/// directory-entry read failure mid-enumeration, is a compile error naming
/// the offending path — never a silent empty include.
fn collect_include_dirs(
    manifest_dir: &Path,
    includes: &[LitStr],
) -> Result<Vec<IncludeDir>, TokenStream> {
    let mut out = Vec::new();
    for lit in includes {
        let path = manifest_dir.join(lit.value());
        let entries = std::fs::read_dir(&path).map_err(|e| {
            syn::Error::new(
                lit.span(),
                format!("include directory {} is not readable: {e}", path.display()),
            )
            .to_compile_error()
        })?;
        let mut hs_files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| {
                syn::Error::new(
                    lit.span(),
                    format!(
                        "failed to read an entry in include directory {}: {e}",
                        path.display()
                    ),
                )
                .to_compile_error()
            })?;
            let p = entry.path();
            if p.extension().is_some_and(|ext| ext == "hs") {
                hs_files.push(p);
            }
        }
        hs_files.sort();
        out.push(IncludeDir {
            span: lit.span(),
            hs_files,
        });
    }
    Ok(out)
}

/// Per-process scratch sibling of a content-addressed dir. Keyed by pid so
/// concurrent processes never share a scratch dir; a leftover from a killed
/// build with the same pid is cleared before use.
fn tmp_sibling(dir: &Path) -> PathBuf {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    dir.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

/// Atomically publish a finished extract dir under its content-addressed
/// name. Losing the rename race to another process is success — the winner
/// published identical content.
fn publish_extract_dir(tmp_dir: &Path, output_dir: &Path) -> Result<(), String> {
    match std::fs::rename(tmp_dir, output_dir) {
        Ok(()) => Ok(()),
        Err(_) if output_dir.exists() => {
            let _ = std::fs::remove_dir_all(tmp_dir);
            Ok(())
        }
        Err(e) => Err(format!(
            "failed to publish extract output {}: {e}",
            output_dir.display()
        )),
    }
}

/// Render a failed extract invocation's diagnostic text: parse `stdout` as the
/// structured diagnostics report (`{"version":1,"diagnostics":[...]}`) and
/// join the messages when it parses; otherwise (an older `tidepool-extract`
/// predating the structured contract, or any other malformed stdout) fall
/// back to the raw stderr text. This is the ONE call site in the workspace
/// allowed that graceful fallback — a dev-convenience macro-expansion tool
/// talking to whatever `tidepool-extract` happens to be on a user's PATH,
/// potentially a much older build.
fn extract_failure_text(stdout: &[u8], stderr: &[u8]) -> String {
    let parsed = serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()
        .and_then(|v| {
            let diags = v.get("diagnostics")?.as_array()?;
            let messages: Vec<String> = diags
                .iter()
                .filter_map(|d| d.get("message")?.as_str().map(str::to_string))
                .collect();
            (!messages.is_empty()).then(|| messages.join("\n\n"))
        });
    parsed.unwrap_or_else(|| String::from_utf8_lossy(stderr).into_owned())
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
        .filter(|p| p.extension().is_some_and(|ext| ext == "cbor"))
        .filter(|p| p.file_stem().is_some_and(|s| s != "meta"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}

fn find_single_binding(output_dir: &Path) -> Result<PathBuf, String> {
    let entries: Vec<PathBuf> = std::fs::read_dir(output_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "cbor"))
                .filter(|p| p.file_stem().is_some_and(|s| s != "meta"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test scratch dir under the OS temp dir, unique by test label +
    /// pid + thread id (nextest gives each test its own process, but plain
    /// `cargo test` runs unit tests as threads in one process).
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tidepool-macro-test-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn single_line_module_header_still_strips_cleanly() {
        let src =
            "{-# LANGUAGE OverloadedStrings #-}\nmodule Foo where\nimport Data.Text\nfoo = 1\n";
        let header = strip_module_header(src);
        assert_eq!(header.extensions, vec!["OverloadedStrings".to_string()]);
        assert_eq!(header.imports, vec!["import Data.Text".to_string()]);
        assert_eq!(header.body.trim(), "foo = 1");
    }

    /// #F4: a `module Foo\n  ( x )\n  where` header spanning multiple lines
    /// must be skipped IN FULL — a naive single-line check leaks the
    /// `( x )`/`where` continuation lines into the inlined body.
    #[test]
    fn multi_line_module_header_does_not_leak_into_body() {
        let src = "module Foo\n  ( x\n  , y\n  )\n  where\n\nx = 1\ny = 2\n";
        let header = strip_module_header(src);
        assert!(
            !header.body.contains("where"),
            "multi-line module header leaked into body: {:?}",
            header.body
        );
        assert!(
            !header.body.contains('('),
            "export-list fragment leaked into body: {:?}",
            header.body
        );
        assert_eq!(header.body.trim(), "x = 1\ny = 2");
    }

    /// The `where` closing a multi-line header may share a line with the last
    /// export (`  ) where`), not just stand alone.
    #[test]
    fn multi_line_module_header_where_on_closing_paren_line() {
        let src = "module Foo\n  ( x\n  ) where\n\nx = 1\n";
        let header = strip_module_header(src);
        assert_eq!(header.body.trim(), "x = 1");
    }

    #[test]
    fn import_module_name_handles_qualified_hiding_and_selector_lists() {
        assert_eq!(
            import_module_name("import qualified Data.Text as T"),
            Some("Data.Text".to_string())
        );
        assert_eq!(
            import_module_name("import Tidepool.Prelude hiding (error)"),
            Some("Tidepool.Prelude".to_string())
        );
        assert_eq!(
            import_module_name("import Data.Map.Strict (Map)"),
            Some("Data.Map.Strict".to_string())
        );
        assert_eq!(import_module_name("x = 1"), None);
    }

    /// Pins the headline silent-stale-cache bug (finding 1): a change to a
    /// module the entry file transitively imports must change the cache
    /// key, even though the entry file's own bytes never change.
    #[test]
    fn transitive_dependency_change_busts_the_cache_key() {
        let dir = temp_dir("transitive-dep");
        let entry_path = dir.join("Entry.hs");
        let dep_path = dir.join("Dep.hs");
        std::fs::write(&entry_path, "module Entry where\nimport Dep\nx = Dep.y\n").unwrap();
        std::fs::write(&dep_path, "module Dep where\ny = 1\n").unwrap();

        let entry_src = std::fs::read_to_string(&entry_path).unwrap();
        let imports = import_module_names(&entry_src);
        assert_eq!(imports, vec!["Dep".to_string()]);

        let compute_key = || {
            let mut seen = BTreeSet::new();
            seen.insert(entry_path.canonicalize().unwrap());
            let deps = resolve_transitive_hs_deps(&imports, std::slice::from_ref(&dir), &mut seen)
                .unwrap();
            assert_eq!(
                deps.len(),
                1,
                "Dep.hs must resolve as a local transitive input"
            );
            content_key(entry_src.as_bytes(), None, &deps)
        };

        let key_before = compute_key();

        // Change ONLY the transitively imported module — Entry.hs is untouched.
        std::fs::write(&dep_path, "module Dep where\ny = 2\n").unwrap();
        let key_after = compute_key();

        assert_ne!(
            key_before, key_after,
            "changing Dep.hs must invalidate the cache key even though Entry.hs never changed"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pins finding 2's core property: the nix-fallback producer identity
    /// tracks the extractor's actual source inputs — it is never a constant
    /// that two different producers could collide on.
    #[test]
    fn nix_identity_changes_with_extractor_source() {
        let dir_a = temp_dir("nix-identity-a");
        let dir_b = temp_dir("nix-identity-b");
        for dir in [&dir_a, &dir_b] {
            std::fs::write(dir.join("flake.lock"), "{}").unwrap();
            std::fs::write(dir.join("flake.nix"), "{ }").unwrap();
            std::fs::create_dir_all(dir.join("haskell/app")).unwrap();
        }
        std::fs::write(dir_a.join("haskell/app/Main.hs"), "main = putStrLn \"a\"\n").unwrap();
        std::fs::write(dir_b.join("haskell/app/Main.hs"), "main = putStrLn \"b\"\n").unwrap();

        let hash_of = |root: &Path| {
            use std::hash::Hasher;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hash_nix_extractor_identity(root, &mut h);
            h.finish()
        };

        assert_ne!(
            hash_of(&dir_a),
            hash_of(&dir_b),
            "the nix-fallback producer identity must change when the extractor's own \
             source changes"
        );

        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }

    /// The other half of finding 2: when identity genuinely can't be
    /// resolved (no flake.lock here), that must panic — never fall back to
    /// hashing a constant that a differently-configured producer could
    /// silently share.
    #[test]
    fn nix_identity_panics_rather_than_hashing_a_placeholder_when_unresolved() {
        let dir = temp_dir("nix-identity-missing");
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // silence the expected panic's stderr noise
        let result = std::panic::catch_unwind(|| {
            use std::hash::Hasher;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hash_nix_extractor_identity(&dir, &mut h);
            h.finish()
        });
        std::panic::set_hook(prev_hook);
        assert!(
            result.is_err(),
            "an unresolvable nix producer identity must panic rather than silently \
             returning a placeholder hash"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pins finding 3: a misspelled/missing `include = "…"` directory is a
    /// compile error naming the offending path — never a silent empty
    /// include.
    #[test]
    fn misspelled_include_directory_is_a_compile_error_naming_the_path() {
        let dir = temp_dir("bad-include");
        let bad_include = LitStr::new("definitely_missing_dir", proc_macro2::Span::call_site());
        let result = collect_include_dirs(&dir, std::slice::from_ref(&bad_include));
        let err_tokens =
            result.expect_err("a missing include directory must be reported as a compile error");
        let rendered = err_tokens.to_string();
        assert!(
            rendered.contains("definitely_missing_dir"),
            "the compile error must name the offending path, got: {rendered}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
