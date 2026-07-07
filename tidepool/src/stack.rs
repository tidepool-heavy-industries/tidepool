use std::net::SocketAddr;
use std::path::PathBuf;

use tidepool_handlers::HandlerConfig;
use tidepool_mcp::TidepoolMcpServer;

/// First non-comment line of a generated helper's Haskell source — its
/// signature. Every generated helper now starts with a `-- |` doc comment
/// (`helper_text!` in `tidepool-mcp/src/effect_defs.rs`), so `.lines().next()`
/// picks up a comment fragment instead of the signature; skip lines starting
/// with `--`.
fn first_sig_line(helper: &str) -> Option<&str> {
    helper.lines().find(|l| !l.trim_start().starts_with("--"))
}

/// The debug decl list: base effects (Console..Time, `base_effects!` order)
/// with Meta appended, then Ask appended last. This is the SAME order
/// `build_debug_stack`'s handler HList wires (Ask is interposed separately by
/// `TidepoolMcpServer::new`, which pushes `ask_decl()` onto whatever
/// `H::collect_decls()` reports) — so a `TidepoolMcpServer` built on
/// `build_debug_stack`'s handlers reproduces this exact list independently,
/// and `effect_names`/`helper_sigs` (derived from it below) can never drift
/// from the real dispatch order again (that drift was F1).
fn debug_decls() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = tidepool_mcp::standard_decls();
    let ask = decls.pop().expect("standard_decls always ends with Ask");
    decls.push(tidepool_mcp::meta_decl());
    decls.push(ask);
    decls
}

/// Meta's `effect_names`/`helper_sigs` payload, derived from [`debug_decls`]
/// (never a hand-maintained parallel list). Helper signatures are extracted
/// by the first NON-comment line, since every generated helper now starts
/// with a `-- |` doc comment (see [`first_sig_line`]).
fn debug_effect_names_and_helper_sigs() -> (Vec<String>, Vec<String>) {
    let decls = debug_decls();
    let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();
    let mut helper_sigs: Vec<String> = Vec::new();
    helper_sigs.push("putStrLn :: Text -> M ()".into());
    helper_sigs.push("showI :: Int -> Text".into());
    for decl in &decls {
        for h in decl.helpers {
            if let Some(sig) = first_sig_line(h) {
                helper_sigs.push(sig.to_string());
            }
        }
    }
    (effect_names, helper_sigs)
}

pub(crate) async fn run_debug(
    handler_cfg: HandlerConfig,
    prelude_dir: PathBuf,
    help_tool: bool,
    http_addr: Option<SocketAddr>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (effect_names, helper_sigs) = debug_effect_names_and_helper_sigs();
    let handlers = tidepool_handlers::build_debug_stack(&handler_cfg, effect_names, helper_sigs);
    let server = TidepoolMcpServer::new(handlers)
        .with_prelude(prelude_dir)
        .with_help_tool(help_tool);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}

pub(crate) async fn run_base(
    handler_cfg: HandlerConfig,
    prelude_dir: PathBuf,
    help_tool: bool,
    http_addr: Option<SocketAddr>,
) -> Result<(), Box<dyn std::error::Error>> {
    let handlers = tidepool_handlers::build_base_stack(&handler_cfg);
    let server = TidepoolMcpServer::new(handlers)
        .with_prelude(prelude_dir)
        .with_help_tool(help_tool);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_ORDER: &[&str] = &[
        "Console", "KV", "Fs", "Http", "Exec", "Lsp", "Llm", "Git", "Time", "Meta", "Ask",
    ];

    #[test]
    fn debug_decls_is_base_effects_order_plus_meta_then_ask() {
        let names: Vec<&str> = debug_decls().iter().map(|d| d.type_name).collect();
        assert_eq!(
            names, EXPECTED_ORDER,
            "Git/Time must not be missing, Meta must not be misplaced"
        );
    }

    #[test]
    fn debug_effect_names_and_helper_sigs_are_real_signatures() {
        let (effect_names, helper_sigs) = debug_effect_names_and_helper_sigs();
        assert_eq!(effect_names, EXPECTED_ORDER);
        // metaHelp must extract real signatures — every generated helper now
        // starts with a `-- |` doc comment, so a naive `.lines().next()`
        // regression would surface as comment fragments here.
        assert!(
            helper_sigs.iter().any(|s| s.starts_with("gitStatus ::")),
            "gitStatus signature missing from helper_sigs: {helper_sigs:?}"
        );
        assert!(
            !helper_sigs.iter().any(|s| s.trim_start().starts_with("--")),
            "helper_sigs must never contain a doc-comment fragment: {helper_sigs:?}"
        );
    }

    /// The handler HList `build_debug_stack` actually wires must report the
    /// SAME order via `collect_decls()` as [`debug_decls`] — this is what
    /// `TidepoolMcpServer::new` uses to build the real, compiled
    /// `Tidepool.Effects` module, so a pass here means `gitStatus`/
    /// `getCurrentTime` are genuinely reachable in the running `--debug`
    /// server (and `metaEffects`'s report matches it), not just present in a
    /// decl list nobody wires up (the F1 drift).
    #[tokio::test]
    async fn debug_stack_handler_order_matches_debug_decls() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = HandlerConfig {
            cwd: dir.path().to_path_buf(),
            kv_path: dir.path().join("kv.json"),
            llm_model: "ollama:llama3.2".to_string(),
        };
        let (effect_names, helper_sigs) = debug_effect_names_and_helper_sigs();
        let handlers = tidepool_handlers::build_debug_stack(&cfg, effect_names, helper_sigs);
        let (collected, ask_tag) = tidepool_handlers::base_decls_with_ask(&handlers);
        let collected_names: Vec<&str> = collected.iter().map(|d| d.type_name).collect();
        assert_eq!(collected_names, EXPECTED_ORDER);
        assert_eq!(
            ask_tag as usize,
            EXPECTED_ORDER.len() - 1,
            "Ask must land after Meta, at the very end"
        );
    }
}
