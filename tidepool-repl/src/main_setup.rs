//! Binary-local startup helpers for `tidepool-repl`'s `main()`.
//!
//! Not part of the `tidepool_repl` library crate — this module is declared
//! in `main.rs` and is private to the binary target.

use std::path::PathBuf;

use tidepool_handlers::{
    base_decls_with_ask, build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL,
};
use tidepool_repl::ReplServerConfig;

/// Resolve the bundled Haskell prelude/stdlib dir (`Tidepool.*` modules).
/// Honors `TIDEPOOL_PRELUDE_DIR`; falls back to the in-repo `haskell/lib`.
pub(crate) fn resolve_prelude_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TIDEPOOL_PRELUDE_DIR") {
        return PathBuf::from(dir);
    }
    let repo_lib = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("haskell").join("lib"))
        .unwrap_or_default();
    if repo_lib.is_dir() {
        return repo_lib;
    }
    repo_lib
}

/// Bundle of values `main()` needs after startup config assembly: the
/// `ReplServerConfig` itself, plus the values the per-session builder
/// closure needs.
pub struct ReplStartup {
    pub cfg: ReplServerConfig,
    pub cwd: PathBuf,
    pub llm_model: String,
    pub tidepool_dir: PathBuf,
}

/// Assemble the `ReplServerConfig` and related startup values.
///
/// Full effect suite, shared with the eval server via `tidepool-handlers`.
/// HandlerConfig resolution mirrors `tidepool/src/main.rs` (cwd sandbox for
/// Fs/Exec/Lsp, the KV backing file, the LLM model). NOTE: unlike the eval
/// binary we don't layer `config.toml` for the model (that `Config` lives in
/// the `tidepool` binary, not a shared lib) — env + default only; factoring
/// it out is a follow-up. `build_base_stack` must run in a tokio context (Llm
/// captures `Handle::current()`), which `#[tokio::main]` provides.
pub fn build(
    cwd: PathBuf,
    project_root: Option<PathBuf>,
) -> Result<ReplStartup, Box<dyn std::error::Error>> {
    let tidepool_dir = match &project_root {
        Some(root) => root.join(".tidepool"),
        None => tidepool_runtime::paths::cache_dir(),
    };
    let llm_model =
        std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());

    // Build a representative stack to derive effect declarations and ask_tag.
    // The kv_path here doesn't matter for decls (they depend only on handler types).
    let sample_cfg = HandlerConfig {
        cwd: cwd.clone(),
        kv_path: tidepool_dir.join("kv.json"),
        llm_model: llm_model.clone(),
    };
    let stack = build_base_stack(&sample_cfg);
    // Decls derive from the stack (in HList/tag order) + Ask appended.
    let (decls, ask_tag) = base_decls_with_ask(&stack);
    drop(stack); // the per-session builder owns each session's stack

    // The generated Tidepool.Effects module must be on the include path.
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)?;
    let prelude_dir = resolve_prelude_dir();
    let mut base_include = vec![effects_dir, prelude_dir];

    // Verb libraries (parity with the eval server): project `.tidepool/lib`
    // first, then user-global, AFTER the stdlib so `Tidepool.*` still resolves
    // from the bundle and a project `Library` shadows the global one. With these
    // on the include path, the preamble auto-imports `Library` (see
    // `has_user_library`) so `.tidepool/lib` verbs (vocab/gitS/census/…) are in
    // scope — previously the REPL listed them in `:vocab` but couldn't call them.
    let lib_dirs = tidepool_mcp::server_common::resolve_lib_dirs(project_root.as_deref());
    base_include.extend(lib_dirs.iter().cloned());

    // Mirrors `has_user_library` (server.rs) — computed here too since
    // `base_include` is about to move into `cfg` and `session_decl_module_env`
    // needs the flag before that.
    let user_library = tidepool_mcp::server_common::has_library_facade(&base_include);

    // Fault-isolate the verb-library layer (issue #322): if a `.tidepool/lib`
    // module is broken, `import Library` fails for EVERY session turn, including
    // the `writeFile` that would repair it. Probe the facade at startup and, on
    // breakage, PREPEND a sanitized `Library.hs` (re-exporting only the modules
    // that compile) so it shadows the broken one — healthy verbs stay in scope
    // and a session can `writeChecked` a repair. This is the redeploy-recovery
    // path for an effect-cut regression (a restart re-runs it); the eval server
    // additionally re-probes per eval for mid-session breakage.
    if user_library {
        let layer = tidepool_mcp::isolate_lib_layer(&lib_dirs, &base_include);
        if let Some(note) = &layer.brick_note {
            eprintln!("[tidepool-repl] {note}");
        }
        if !layer.prepend_include.is_empty() {
            let mut prefixed = layer.prepend_include;
            prefixed.extend(base_include);
            base_include = prefixed;
        }
    }

    // Per-session include trees live under a process-scoped temp dir.
    let session_root_base =
        std::env::temp_dir().join(format!("tidepool-repl-{}", std::process::id()));

    // Full effect stack + with-packages GHC: give Lane-A decls the SAME
    // pragmas/imports an `eval` expression sees, so declaration-item helpers
    // can use `M`, the effect verbs, the Prelude shadows, and `L.`/`Set.`/… —
    // not just the lens-free T+Map of `standalone_default`.
    let module_env = tidepool_mcp::session_decl_module_env(&decls, user_library);

    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include,
        module_env,
        session_root_base,
        nursery_size: None,
        // Parked `ask` suspensions never expire: a long-parked knot holding one
        // worker thread + JIT machine is an acceptable cost, and a silently
        // reaped continuation kills the ask/resume pattern for slow callers.
        continuation_ttl: None,
        // Wedged sessions (timed-out turns) ARE dead weight — sweep at 30 min.
        wedged_ttl: Some(std::time::Duration::from_secs(30 * 60)),
        // Default 600 s turn budget (see `TURN_TIMEOUT_SECS`).
        turn_timeout: None,
    };

    Ok(ReplStartup {
        cfg,
        cwd,
        llm_model,
        tidepool_dir,
    })
}
