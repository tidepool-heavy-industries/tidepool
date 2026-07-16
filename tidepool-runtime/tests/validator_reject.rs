//! Validator quasi-quoter rejections: `[uri|]` moves a class of silent runtime
//! traps (scheme-less URIs) to COMPILE-time splice errors. A rejected body
//! fails the GHC splice, so
//! `compile_and_run` returns `Err` whose text carries the message. Reject cases
//! cannot be CBOR fixtures (they never compile), so they are asserted here; the
//! accept cases live in the Suite fixtures.
//!
//! Run with the worktree extract binary, e.g.:
//!   TIDEPOOL_EXTRACT=<worktree>/haskell/dist-newstyle/.../tidepool-extract-bin \
//!   TIDEPOOL_GHC_LIBDIR=<with-packages>/lib/ghc-9.12.2/lib \
//!   cargo test -p tidepool-runtime --test validator_reject
use tidepool_testing::NullDispatcher;
use tidepool_testing::eval_harness::EvalHarness;

fn try_compile(hole: &str) -> Result<(), String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code = format!("-- nonce {nonce}\npure {hole}");
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&code),
        "Tidepool.QQ (fmt, j, patch, uri)",
        "",
        None,
        None,
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    EvalHarness::new()
        .with_stdlib()
        .with_include(root.join(".tidepool/lib"))
        .with_effects_module()
        .run(&src, "result", NullDispatcher)
        .into_result()
        .map(|_| ())
        // `Display` on `RuntimeError`/`CompileError::Diagnostics` is a terse
        // structural summary now (structured spans, not rendered text) — the
        // classifier's message is the joined diagnostic text carrying the
        // splice-rejection wording this test asserts against.
        .map_err(|e| tidepool_runtime::classify(&e).message)
}

#[test]
fn validator_rejects() {
    // Importing Tidepool.QQ pulls the whole quoter graph (incl. the
    // ghc-package-backed HsMeta) — needs the harness's EVAL_STACK_SIZE thread,
    // not the default 2MB test thread.
    run();
}

fn run() {
    // --- uri: scheme + host + no whitespace ---
    let err = try_compile("[uri|example.com/x|]").expect_err("a scheme-less URI must be rejected");
    assert!(
        err.contains("http://") || err.contains("https://"),
        "uri rejection should name the required schemes; got:\n{err}"
    );
    try_compile("[uri|https://a b.com|]").expect_err("a URI with whitespace must be rejected");
    try_compile("[uri|https://|]").expect_err("a URI with an empty host must be rejected");
    try_compile("[uri|https://example.com/p|]").expect("a valid https URI should compile");
}
