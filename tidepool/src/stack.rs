use std::net::SocketAddr;
use std::path::PathBuf;

use tidepool_handlers::HandlerConfig;
use tidepool_mcp::TidepoolMcpServer;

/// First non-comment line of a generated helper's Haskell source — its
/// signature. Every generated helper starts with a `-- |` doc comment, so
/// `.lines().next()` would pick up the comment instead; skip lines starting
/// with `--`.
fn first_sig_line(helper: &str) -> Option<&str> {
    helper.lines().find(|l| !l.trim_start().starts_with("--"))
}

/// The debug decl list: base effects (Console..Time, `base_effects!` order)
/// with Meta appended, then the interposed effects (Ask, RunLLMTurn)
/// appended last. HAZARD: this must independently reproduce the SAME order
/// `build_debug_stack`'s handler HList actually wires (Ask/RunLLMTurn
/// are appended separately by `TidepoolMcpServer::new`, via
/// `EffectRoster::from_handlers`) — a past drift between this list and the
/// real dispatch order silently broke Meta's reported effect/helper list.
/// `debug_stack_handler_order_matches_debug_decls` below pins the two
/// against each other. No `Fork` here (vestigial-subsystems review §4): the
/// `--debug` stack runs through the SAME one-shot session engine as
/// `run_base`, which never services `ForkWith`/`ForkAllWith` — see
/// `tidepool_mcp::standard_decls`'s doc.
///
/// Splits off everything from `Ask` onward (found by name, not a fixed
/// count) rather than popping exactly one: `standard_decls()` appends
/// `RunLLMTurn` after `Ask`, and both must stay interposed, after Meta, for
/// the JIT's suspend-tag threshold to hold.
fn debug_decls() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = tidepool_mcp::standard_decls();
    #[allow(clippy::expect_used, reason = "standard_decls always contains Ask")]
    let ask_idx = decls
        .iter()
        .position(|d| d.type_name == "Ask")
        .expect("standard_decls always contains Ask");
    let interposed = decls.split_off(ask_idx);
    decls.push(tidepool_mcp::meta_decl());
    decls.extend(interposed);
    decls
}

/// Meta's `effect_names`/`helper_sigs` payload, derived from [`debug_decls`].
/// Helper signatures are extracted by the first NON-comment line, since
/// every generated helper starts with a `-- |` doc comment (see
/// [`first_sig_line`]).
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
        "Console",
        "KV",
        "Fs",
        "Http",
        "Exec",
        "Llm",
        "Git",
        "Time",
        "Entropy",
        "Meta",
        "Ask",
        "RunLLMTurn",
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
        assert!(
            helper_sigs.iter().any(|s| s.starts_with("gitStatus ::")),
            "gitStatus signature missing from helper_sigs: {helper_sigs:?}"
        );
        assert!(
            !helper_sigs.iter().any(|s| s.trim_start().starts_with("--")),
            "helper_sigs must never contain a doc-comment fragment: {helper_sigs:?}"
        );
    }

    /// Pins `build_debug_stack`'s real handler order against [`debug_decls`]
    /// — see the hazard note there.
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
            EXPECTED_ORDER.len() - 2,
            "Ask must land after Meta, followed only by RunLLMTurn"
        );
    }
}
