use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tidepool_extract_cmd::{BinSource, CompilerEndpoint, ExtractCmd};

const EXTRACT_COMPLETE_FILE: &str = ".tidepool-extract-complete";
/// A resolved local `.hs` input: its canonicalized path and content.
type HsDep = (PathBuf, Vec<u8>);
/// A path that couldn't be read, paired with the underlying io error.
type PathReadError = (PathBuf, std::io::Error);

struct ExtractArtifact {
    name: String,
    bytes: Vec<u8>,
}

struct ExtractArtifacts {
    artifacts: Vec<ExtractArtifact>,
}

/// Expands the `haskell_eval!` macro.
///
/// Accepts `.cbor` paths (embedded directly) or `.hs` paths (compiled by
/// `run_tidepool_extract` at proc-macro expansion time).
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

    #[allow(
        clippy::unwrap_used,
        reason = "abs_hs_path is derived from the crate's own build environment, which this project assumes is UTF-8"
    )]
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

    // `resolve_hs_path` has no include feature today, so the extractor's GHC
    // session resolves every non-entry import against `importPaths = ["."]`
    // (the extractor subprocess's cwd, inherited unchanged from this process
    // — see `extractionDynFlags` in haskell/src/Tidepool/GhcPipeline.hs). The
    // cache key must walk the SAME search order or it can miss a real input.
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
    let roots = vec![cwd];
    let entry_imports = import_module_names(&String::from_utf8_lossy(&src_bytes));
    let mut seen = BTreeSet::new();
    seen.insert(
        abs_hs_path
            .canonicalize()
            .unwrap_or_else(|_| abs_hs_path.clone()),
    );
    let mut deps = match resolve_transitive_hs_deps(&entry_imports, &roots, &mut seen) {
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
    // identity reported by the bound endpoint. `run_tidepool_extract` validates the
    // complete artifact set before publishing the dir with an atomic rename.
    let dep_paths: Vec<PathBuf> = deps.iter().map(|(p, _)| p.clone()).collect();
    let output_dir = match retry_safe_refusals(
        || bind_compiler_endpoint(Path::new(&manifest_dir)),
        |compiler| {
            let key = content_key(
                &src_bytes,
                binding_name.as_deref(),
                &deps,
                compiler.endpoint.identity().as_bytes(),
            );
            let output_dir = Path::new(&manifest_dir)
                .join("target")
                .join("tidepool-cbor")
                .join(format!("{basename}-{key}"));
            run_tidepool_extract(&abs_hs_path, &output_dir, binding_name.as_deref(), compiler)?;
            Ok(output_dir)
        },
    ) {
        Ok(output_dir) => output_dir,
        Err(message) => return Err(syn::Error::new(path_lit.span(), message).to_compile_error()),
    };

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

    #[allow(
        clippy::unwrap_used,
        reason = "cbor_path is derived from the crate's own build environment, which this project assumes is UTF-8"
    )]
    let cbor_path_str = cbor_path.to_str().unwrap();
    #[allow(
        clippy::unwrap_used,
        reason = "abs_hs_path is derived from the crate's own build environment, which this project assumes is UTF-8"
    )]
    let hs_abs_str = abs_hs_path.to_str().unwrap();
    let dep_tracks: Vec<TokenStream> = dep_paths
        .iter()
        .map(|p| {
            #[allow(clippy::unwrap_used, reason = "dep_paths are derived from the crate's own build environment, which this project assumes is UTF-8")]
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

    // Same import-search mirroring as `resolve_hs_path`: any import left in
    // `all_imports` after filtering out inlined siblings resolves only
    // against the extractor subprocess's cwd. Splicing already folds every
    // included file's own content into `full_source` (and thus into the hash
    // below); this closes the remaining gap — an import that survives
    // filtering and points at a local (non-package) module.
    //
    // `include_dirs` (validated above) is a text-splicing input here, not a
    // GHC search path: passing its paths as `--include` too, for extra
    // robustness, would be a real behavior change (widening what GHC itself
    // resolves, on top of what's already spliced verbatim), not just a
    // plumbing one.
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
    let roots = vec![cwd];
    let remaining_imports = import_module_names(&all_imports.join("\n"));
    let mut seen = BTreeSet::new();
    let mut extra_deps = match resolve_transitive_hs_deps(&remaining_imports, &roots, &mut seen) {
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
    let extra_dep_paths: Vec<PathBuf> = extra_deps.iter().map(|(p, _)| p.clone()).collect();
    let (hs_file, output_dir) = match retry_safe_refusals(
        || bind_compiler_endpoint(Path::new(&manifest_dir)),
        |compiler| {
            let key = content_key(
                full_source.as_bytes(),
                Some(&parsed.target),
                &extra_deps,
                compiler.endpoint.identity().as_bytes(),
            );
            let inline_dir = Path::new(&manifest_dir)
                .join("target")
                .join("tidepool-inline")
                .join(&key);
            std::fs::create_dir_all(&inline_dir).map_err(|error| {
                RetryError::Fatal(format!(
                    "Failed to create {}: {error}",
                    inline_dir.display()
                ))
            })?;
            let hs_file = inline_dir.join(format!("{}.hs", module_name));
            let hs_tmp = inline_dir.join(format!("{}.hs.tmp-{}", module_name, std::process::id()));
            std::fs::write(&hs_tmp, &full_source)
                .and_then(|()| std::fs::rename(&hs_tmp, &hs_file))
                .map_err(|error| {
                    RetryError::Fatal(format!("Failed to write {}: {error}", hs_file.display()))
                })?;
            let output_dir = Path::new(&manifest_dir)
                .join("target")
                .join("tidepool-cbor")
                .join(format!("{module_name}-{key}"));
            run_tidepool_extract(&hs_file, &output_dir, Some(&parsed.target), compiler)?;
            Ok((hs_file, output_dir))
        },
    ) {
        Ok(paths) => paths,
        Err(message) => return syn::Error::new(parsed.source.span(), message).to_compile_error(),
    };

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

    #[allow(
        clippy::unwrap_used,
        reason = "cbor_path is derived from the crate's own build environment, which this project assumes is UTF-8"
    )]
    let cbor_path_str = cbor_path.to_str().unwrap();
    let meta_path = output_dir.join("meta.cbor");
    #[allow(
        clippy::unwrap_used,
        reason = "meta_path is derived from the crate's own build environment, which this project assumes is UTF-8"
    )]
    let meta_path_str = meta_path.to_str().unwrap();
    #[allow(
        clippy::unwrap_used,
        reason = "hs_file is derived from the crate's own build environment, which this project assumes is UTF-8"
    )]
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
            #[allow(clippy::unwrap_used, reason = "include_dirs are derived from the crate's own build environment, which this project assumes is UTF-8")]
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
fn run_tidepool_extract(
    hs_path: &Path,
    output_dir: &Path,
    target: Option<&str>,
    compiler: BoundCompiler,
) -> Result<(), RetryError> {
    // The output dir name already encodes the resolved input set (entry file
    // plus every transitively resolved local import — see
    // `resolve_transitive_hs_deps`), the target, and the producer identity
    // reported by the bound endpoint. A hit must additionally carry a valid
    // completion manifest for every required, decodable artifact. The publish
    // rename below is atomic, so a partially-written dir is never visible under
    // the final name. Concurrent expansions converge on one validated dir.
    let Some(tmp_path) = scratch_for_extract(output_dir, target).map_err(RetryError::Fatal)? else {
        return Ok(());
    };
    let tmp_dir = ScratchDir(tmp_path);

    // $TIDEPOOL_EXTRACT (the same override every test tier honors) wins over
    // PATH — a repo with a freshly built extract must never be trumped by a
    // stale installed one. A SET-but-unreadable $TIDEPOOL_EXTRACT is a hard
    // error, not a silent fall-through to PATH/nix: falling through would run
    // a DIFFERENT endpoint than the content key names. An UNSET env still falls back to PATH
    // then nix below, same as always. That policy is now `ExtractCmd`'s
    // DEFAULT (`tidepool-extract-cmd`) rather than this function's local rule.
    let BoundCompiler { mut cmd, endpoint } = compiler;
    // The argument list is built once and executed by the endpoint whose
    // identity selected the output directory.
    cmd.input(hs_path).output_dir(tmp_dir.path());
    if let Some(name) = target {
        cmd.target(name);
    }

    match endpoint.execute(&cmd) {
        Ok(run) if run.success() => {
            prepare_extract_dir(tmp_dir.path(), target).map_err(RetryError::Fatal)?;
            publish_extract_dir(tmp_dir.path(), output_dir, target).map_err(RetryError::Fatal)
        }
        Ok(run) => {
            // The binary ran and failed — this IS the diagnostic (a GHC type
            // error, a missing binding, ...). Surface it verbatim; falling
            // back to nix here would only re-run the SAME failing compile.
            Err(RetryError::Fatal(format!(
                "tidepool-extract failed (exit {}):\n{}",
                run.output.status,
                extract_failure_text(&run.output.stdout, &run.output.stderr)
            )))
        }
        Err(e) if e.permits_rebind() => Err(RetryError::SafeRefusal),
        Err(e) => {
            // The endpoint may have accepted the request, so its outcome is
            // indeterminate. Fail loud and never replay it elsewhere.
            Err(RetryError::Fatal(e.to_string()))
        }
    }
}

enum RetryError {
    SafeRefusal,
    Fatal(String),
}

fn retry_safe_refusals<B, T>(
    mut bind: impl FnMut() -> Result<B, String>,
    mut attempt: impl FnMut(B) -> Result<T, RetryError>,
) -> Result<T, String> {
    let mut rebinds = 0;
    loop {
        match attempt(bind()?) {
            Err(RetryError::SafeRefusal) if rebinds < 2 => rebinds += 1,
            Err(RetryError::SafeRefusal) => {
                return Err(
                    "compiler endpoint repeatedly refused the request before acceptance".into(),
                );
            }
            Err(RetryError::Fatal(message)) => return Err(message),
            Ok(value) => return Ok(value),
        }
    }
}

struct BoundCompiler {
    cmd: ExtractCmd,
    endpoint: CompilerEndpoint,
}

fn bind_compiler_endpoint(manifest_dir: &Path) -> Result<BoundCompiler, String> {
    let cmd = ExtractCmd::new().map_err(|error| error.to_string())?;
    match cmd.bind() {
        Ok(endpoint) => Ok(BoundCompiler { cmd, endpoint }),
        Err(error) if error.is_not_found() && cmd.bin_source() == BinSource::PathLookup => {
            let flake_root = find_flake_root(manifest_dir).ok_or_else(|| {
                "tidepool-extract not found on PATH and no flake.nix in any parent directory"
                    .to_owned()
            })?;
            let endpoint = cmd.bind_nix_fallback(&flake_root).map_err(|error| {
                format!("Failed to run nix: {}. Is nix installed?", error.source)
            })?;
            Ok(BoundCompiler { cmd, endpoint })
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Feed one field into `h` framed with its own byte length first, so two
/// different field splits can never hash identically (e.g. `"ab"` + `"c"`
/// colliding with `"a"` + `"bc"` under bare concatenation).
fn frame_field(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Stable key for the extract content cache, as a lowercase hex digest
/// (blake3, truncated to 128 bits — collision-safe for a `target/`-local
/// cache, matching the codebase's `blake3_hex` truncation convention).
/// `deps` must already be sorted by path (callers own the sort so the same
/// input set always hashes to the same key regardless of resolution order).
fn content_key(
    bytes: &[u8],
    target: Option<&str>,
    deps: &[HsDep],
    endpoint_identity: &[u8],
) -> String {
    let mut h = blake3::Hasher::new();
    frame_field(&mut h, bytes);
    for (path, content) in deps {
        frame_field(&mut h, path.to_string_lossy().as_bytes());
        frame_field(&mut h, content);
    }
    // Distinguish `None` from `Some("")`: a bare length-0 frame is ambiguous
    // between the two, so the presence flag rides as its own byte.
    h.update(&[target.is_some() as u8]);
    frame_field(&mut h, target.unwrap_or("").as_bytes());
    frame_field(&mut h, endpoint_identity);
    h.finalize().to_hex()[..32].to_string()
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
/// contributes no entry here; the bound endpoint identity covers that
/// compiler environment. `already_seen` is both the entry
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

/// A scratch sibling unique across processes and same-process macro expansions.
fn unique_sibling(dir: &Path, role: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
    dir.with_file_name(format!("{name}.{role}-{}-{sequence}", std::process::id()))
}

/// Owns an unpublished extract directory. A successful publish renames the
/// path away; every error path automatically removes whatever the extractor
/// left behind.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = remove_cache_entry(&self.0);
    }
}

fn remove_cache_entry(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn create_scratch(output_dir: &Path) -> Result<PathBuf, String> {
    let parent = output_dir.parent().ok_or_else(|| {
        format!(
            "extract output has no parent directory: {}",
            output_dir.display()
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "failed to create extract cache parent {}: {e}",
            parent.display()
        )
    })?;
    loop {
        let scratch = unique_sibling(output_dir, "tmp");
        match std::fs::create_dir(&scratch) {
            Ok(()) => return Ok(scratch),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "failed to reserve extract scratch directory {}: {e}",
                    scratch.display()
                ));
            }
        }
    }
}

/// Return a private scratch directory for a miss, or `None` for a valid hit.
/// Invalid entries are atomically moved aside before rebuilding. If another
/// publisher replaced the entry between validation and rename, its valid
/// directory is restored (or the still-present winner is used).
fn scratch_for_extract(output_dir: &Path, target: Option<&str>) -> Result<Option<PathBuf>, String> {
    loop {
        if !output_dir.exists() {
            return create_scratch(output_dir).map(Some);
        }
        if validate_extract_dir(output_dir, target).is_ok() {
            return Ok(None);
        }

        let rejected = unique_sibling(output_dir, "rejected");
        match std::fs::rename(output_dir, &rejected) {
            Ok(()) => {
                if validate_extract_dir(&rejected, target).is_ok() {
                    match std::fs::rename(&rejected, output_dir) {
                        Ok(()) => return Ok(None),
                        Err(_) if validate_extract_dir(output_dir, target).is_ok() => {
                            let _ = std::fs::remove_dir_all(&rejected);
                            return Ok(None);
                        }
                        Err(e) => {
                            return Err(format!(
                                "failed to restore concurrent extract cache entry {}: {e}",
                                output_dir.display()
                            ));
                        }
                    }
                }
                remove_cache_entry(&rejected).map_err(|e| {
                    format!(
                        "failed to remove invalid extract cache entry {}: {e}",
                        rejected.display()
                    )
                })?;
                return create_scratch(output_dir).map(Some);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "failed to reject invalid extract cache entry {}: {e}",
                    output_dir.display()
                ));
            }
        }
    }
}

/// Atomically publish a finished extract dir under its content-addressed
/// name. Losing the rename race to another process is success — the winner
/// published identical content.
fn publish_extract_dir(
    tmp_dir: &Path,
    output_dir: &Path,
    target: Option<&str>,
) -> Result<(), String> {
    match std::fs::rename(tmp_dir, output_dir) {
        Ok(()) => Ok(()),
        Err(_) if output_dir.exists() => {
            let _ = std::fs::remove_dir_all(tmp_dir);
            validate_extract_dir(output_dir, target).map(|_| ())
        }
        Err(e) => Err(format!(
            "failed to publish extract output {}: {e}",
            output_dir.display()
        )),
    }
}

/// Validate a successful extractor output and write its completion manifest last.
/// The containing scratch directory is not published until this succeeds.
fn prepare_extract_dir(dir: &Path, target: Option<&str>) -> Result<(), String> {
    let validated = required_extract_artifacts(dir, target)?;
    for artifact in &validated.artifacts {
        validate_artifact(&artifact.name, &artifact.bytes)?;
    }
    let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::from_artifacts(
        validated
            .artifacts
            .iter()
            .map(|artifact| (artifact.name.as_str(), Some(artifact.bytes.as_slice()))),
    );
    std::fs::write(dir.join(EXTRACT_COMPLETE_FILE), manifest.encode()).map_err(|e| {
        format!(
            "failed to write extract completion metadata in {}: {e}",
            dir.display()
        )
    })
}

/// A hit is valid only when its manifest matches this expansion mode and every
/// required artifact is readable, unchanged, and decodes at macro-expansion time.
fn validate_extract_dir(dir: &Path, target: Option<&str>) -> Result<ExtractArtifacts, String> {
    let manifest_path = dir.join(EXTRACT_COMPLETE_FILE);
    let bytes = std::fs::read(&manifest_path).map_err(|e| {
        format!(
            "incomplete tidepool macro cache entry {}: cannot read {}: {e}",
            dir.display(),
            manifest_path.display()
        )
    })?;
    let manifest = tidepool_extract_report::artifact_manifest::ArtifactManifest::decode(&bytes)
        .map_err(|error| {
            format!(
                "incompatible or corrupt tidepool macro cache completion metadata {}: {error}",
                manifest_path.display()
            )
        })?;
    let validated = required_extract_artifacts(dir, target)?;
    let expected_names: Vec<&str> = validated
        .artifacts
        .iter()
        .map(|artifact| artifact.name.as_str())
        .collect();
    let stored_names: Vec<&str> = manifest
        .entries()
        .iter()
        .map(|entry| entry.name())
        .collect();
    if stored_names != expected_names {
        return Err(format!(
            "incomplete tidepool macro cache entry {}: artifact set does not match completion metadata",
            dir.display()
        ));
    }
    for (entry, artifact) in manifest.entries().iter().zip(&validated.artifacts) {
        if !entry.matches(&artifact.bytes) {
            return Err(format!(
                "corrupt tidepool macro cache artifact: {}",
                dir.join(&artifact.name).display()
            ));
        }
        validate_artifact(&artifact.name, &artifact.bytes)?;
    }
    Ok(validated)
}

fn required_extract_artifacts(
    dir: &Path,
    target: Option<&str>,
) -> Result<ExtractArtifacts, String> {
    let mut names = vec!["meta.cbor".to_string()];
    if let Some(target) = target {
        names.push(format!("{target}.cbor"));
        names.push("asks.json".to_string());
    } else {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot enumerate extract output {}: {e}", dir.display()))?;
        let mut bindings = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot enumerate extract output {}: {e}", dir.display()))?
                .path();
            if path.extension().is_some_and(|ext| ext == "cbor")
                && path.file_name().is_some_and(|name| name != "meta.cbor")
            {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        format!("extract artifact name is not UTF-8: {}", path.display())
                    })?;
                bindings.push(name.to_string());
            }
        }
        bindings.sort();
        if bindings.is_empty() {
            return Err(format!(
                "incomplete extract output {}: no binding .cbor was produced",
                dir.display()
            ));
        }
        let multiple = bindings.len() > 1;
        for binding in bindings {
            if multiple {
                let base = binding
                    .strip_suffix(".cbor")
                    .ok_or_else(|| format!("invalid binding artifact name {binding}"))?;
                names.push(format!("{base}.asks.json"));
            } else {
                names.push("asks.json".to_string());
            }
            names.push(binding);
        }
    }
    names.sort();
    let artifacts = names
        .into_iter()
        .map(|name| {
            let path = dir.join(&name);
            std::fs::read(&path)
                .map(|bytes| (name, bytes))
                .map_err(|e| {
                    format!(
                        "incomplete extract output: cannot read {}: {e}",
                        path.display()
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(name, bytes)| ExtractArtifact { name, bytes })
        .collect();
    Ok(ExtractArtifacts { artifacts })
}

fn validate_artifact(name: &str, bytes: &[u8]) -> Result<(), String> {
    if name == "meta.cbor" {
        tidepool_repr::serial::read_metadata(bytes)
            .map(|_| ())
            .map_err(|e| format!("invalid extractor metadata {name}: {e}"))
    } else if name.ends_with(".cbor") {
        tidepool_repr::serial::read_cbor(bytes)
            .map(|_| ())
            .map_err(|e| format!("invalid extractor artifact {name}: {e}"))
    } else if name.ends_with("asks.json") && bytes.is_empty() {
        Err(format!("invalid extractor artifact {name}: empty file"))
    } else {
        Ok(())
    }
}

/// Render a failed extract invocation's diagnostic text from the shared typed
/// worker report and join its messages. Otherwise (an older `tidepool-extract`
/// predating the structured contract, or any other malformed stdout) fall
/// back to the raw stderr text. This is the ONE call site in the workspace
/// allowed that graceful fallback — a dev-convenience macro-expansion tool
/// talking to whatever `tidepool-extract` happens to be on a user's PATH,
/// potentially a much older build.
fn extract_failure_text(stdout: &[u8], stderr: &[u8]) -> String {
    let parsed = tidepool_extract_report::decode_report(stdout)
        .ok()
        .and_then(|report| {
            (report.outcome != tidepool_extract_report::ExtractOutcome::Success)
                .then(|| {
                    report
                        .diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.message.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .filter(|messages| !messages.is_empty())
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
        #[allow(
            clippy::unwrap_used,
            reason = "match arm already established entries.len() == 1"
        )]
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

    fn valid_extract_dir(label: &str, target: &str) -> PathBuf {
        let dir = temp_dir(label);
        write_valid_extract(&dir, target);
        dir
    }

    fn write_valid_extract(dir: &Path, target: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/Identity_cbor");
        std::fs::write(
            dir.join(format!("{target}.cbor")),
            std::fs::read(fixtures.join("identity.cbor")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("meta.cbor"),
            std::fs::read(fixtures.join("meta.cbor")).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("asks.json"), "[]").unwrap();
    }

    fn assert_cache_miss(dir: &Path, target: Option<&str>) {
        let scratch = scratch_for_extract(dir, target)
            .expect("cache lookup must succeed")
            .expect("invalid cache entry must become a miss");
        assert!(!dir.exists(), "invalid final entry must be removed");
        assert!(scratch.is_dir(), "private scratch must be reserved");
        assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
    }

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
            content_key(entry_src.as_bytes(), None, &deps, b"test-endpoint")
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

    #[test]
    fn content_key_tracks_bound_endpoint_identity() {
        let a = content_key(b"source", Some("result"), &[], b"endpoint-a");
        let b = content_key(b"source", Some("result"), &[], b"endpoint-b");
        assert_ne!(a, b);
    }

    #[test]
    fn safe_refusal_rebinds_before_selecting_the_cache_directory() {
        let identities = [b"refused".as_slice(), b"rebound".as_slice()];
        let mut next = 0;
        let mut selected = Vec::new();
        let output = retry_safe_refusals(
            || {
                let identity = identities[next];
                next += 1;
                Ok(identity)
            },
            |identity| {
                let key = content_key(b"source", Some("result"), &[], identity);
                let dir = PathBuf::from("target/tidepool-cbor").join(key);
                selected.push(dir.clone());
                if selected.len() == 1 {
                    Err(RetryError::SafeRefusal)
                } else {
                    Ok(dir)
                }
            },
        )
        .expect("known-unsubmitted refusal must rebind");

        assert_eq!(selected.len(), 2);
        assert_ne!(selected[0], selected[1]);
        assert_eq!(output, selected[1]);
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

    #[test]
    fn completed_extract_directory_is_a_valid_hit() {
        let dir = valid_extract_dir("complete-hit", "result");
        prepare_extract_dir(&dir, Some("result")).unwrap();
        assert!(validate_extract_dir(&dir, Some("result")).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_required_cbor_turns_a_completed_hit_into_a_miss() {
        let dir = valid_extract_dir("missing-cbor", "result");
        prepare_extract_dir(&dir, Some("result")).unwrap();
        std::fs::remove_file(dir.join("result.cbor")).unwrap();
        assert_cache_miss(&dir, Some("result"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_cbor_turns_a_completed_hit_into_a_miss() {
        let dir = valid_extract_dir("corrupt-cbor", "result");
        prepare_extract_dir(&dir, Some("result")).unwrap();
        std::fs::write(dir.join("result.cbor"), "bad").unwrap();
        assert_cache_miss(&dir, Some("result"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn untargeted_mode_records_every_produced_binding() {
        let dir = valid_extract_dir("untargeted", "identity");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../haskell/test/Identity_cbor");
        std::fs::write(
            dir.join("apply.cbor"),
            std::fs::read(fixtures.join("apply.cbor")).unwrap(),
        )
        .unwrap();
        std::fs::remove_file(dir.join("asks.json")).unwrap();
        std::fs::write(dir.join("identity.asks.json"), "[]").unwrap();
        std::fs::write(dir.join("apply.asks.json"), "[]").unwrap();
        prepare_extract_dir(&dir, None).unwrap();
        let validated = validate_extract_dir(&dir, None).unwrap();
        for name in [
            "identity.cbor",
            "identity.asks.json",
            "apply.cbor",
            "apply.asks.json",
        ] {
            assert!(
                validated
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.name == name),
                "manifest omitted {name}"
            );
        }

        std::fs::remove_file(dir.join("apply.asks.json")).unwrap();
        assert!(validate_extract_dir(&dir, None).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_or_corrupt_metadata_turns_a_completed_hit_into_a_miss() {
        for (label, replacement) in [
            ("missing-meta", None),
            ("corrupt-meta", Some(b"bad".as_slice())),
        ] {
            let dir = valid_extract_dir(label, "result");
            prepare_extract_dir(&dir, Some("result")).unwrap();
            match replacement {
                Some(bytes) => std::fs::write(dir.join("meta.cbor"), bytes).unwrap(),
                None => std::fs::remove_file(dir.join("meta.cbor")).unwrap(),
            }
            assert_cache_miss(&dir, Some("result"));
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn freshly_produced_invalid_artifacts_fail_before_publication() {
        let dir = valid_extract_dir("fresh-invalid", "result");
        std::fs::write(dir.join("meta.cbor"), "bad").unwrap();
        assert!(prepare_extract_dir(&dir, Some("result")).is_err());
        assert!(!dir.join(EXTRACT_COMPLETE_FILE).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn absent_corrupt_or_stale_completion_metadata_becomes_a_miss() {
        let absent = valid_extract_dir("absent-completion", "result");
        assert_cache_miss(&absent, Some("result"));
        std::fs::remove_dir_all(absent).ok();

        let corrupt = valid_extract_dir("corrupt-completion", "result");
        prepare_extract_dir(&corrupt, Some("result")).unwrap();
        std::fs::write(corrupt.join(EXTRACT_COMPLETE_FILE), "bad").unwrap();
        assert_cache_miss(&corrupt, Some("result"));
        std::fs::remove_dir_all(corrupt).ok();

        let stale = valid_extract_dir("stale-completion", "result");
        prepare_extract_dir(&stale, Some("result")).unwrap();
        assert_cache_miss(&stale, Some("other"));
        std::fs::remove_dir_all(stale).ok();
    }

    #[test]
    fn interrupted_scratch_directory_is_never_a_hit() {
        let final_dir = temp_dir("interrupted-final").join("final");
        let scratch = unique_sibling(&final_dir, "tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("result.cbor"), "partial").unwrap();
        let next = scratch_for_extract(&final_dir, Some("result"))
            .unwrap()
            .expect("an unpublished scratch is still a miss");
        assert_ne!(scratch, next, "same-process scratches must be unique");
        std::fs::remove_dir_all(final_dir.parent().unwrap()).ok();
    }

    #[test]
    fn failed_extract_cleans_its_private_scratch_directory() {
        let path = temp_dir("scratch-cleanup");
        {
            let scratch = ScratchDir(path.clone());
            std::fs::create_dir_all(scratch.path()).unwrap();
            std::fs::write(scratch.path().join("partial"), "partial").unwrap();
        }
        assert!(!path.exists());
    }

    #[test]
    fn concurrent_identical_publishers_converge_on_one_valid_directory() {
        let root = temp_dir("concurrent-publish");
        let final_dir = root.join("final");
        let left = scratch_for_extract(&final_dir, Some("result"))
            .unwrap()
            .unwrap();
        let right = scratch_for_extract(&final_dir, Some("result"))
            .unwrap()
            .unwrap();
        assert_ne!(left, right);
        write_valid_extract(&left, "result");
        write_valid_extract(&right, "result");
        prepare_extract_dir(&left, Some("result")).unwrap();
        prepare_extract_dir(&right, Some("result")).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let publish = |scratch: PathBuf, barrier: std::sync::Arc<std::sync::Barrier>| {
            let final_dir = final_dir.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish_extract_dir(&scratch, &final_dir, Some("result"))
            })
        };
        let a = publish(left, barrier.clone());
        let b = publish(right, barrier);
        assert!(a.join().unwrap().is_ok());
        assert!(b.join().unwrap().is_ok());
        assert!(validate_extract_dir(&final_dir, Some("result")).is_ok());
        std::fs::remove_dir_all(root).ok();
    }
}
