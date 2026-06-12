//! Validator quasi-quoter rejections: `[sg|]` and `[uri|]` move a class of
//! silent runtime traps (ast-grep's `$$NAME` no-match, scheme-less URIs) to
//! COMPILE-time splice errors. A rejected body fails the GHC splice, so
//! `compile_and_run` returns `Err` whose text carries the message. Reject cases
//! cannot be CBOR fixtures (they never compile), so they are asserted here; the
//! accept cases live in the Suite fixtures.
//!
//! Run with the worktree extract binary, e.g.:
//!   TIDEPOOL_EXTRACT=<worktree>/haskell/dist-newstyle/.../tidepool-extract-bin \
//!   TIDEPOOL_GHC_LIBDIR=<with-packages>/lib/ghc-9.12.2/lib \
//!   cargo test -p tidepool-runtime --test validator_reject
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// Never actually invoked — `pure [..|]` dispatches no effect, and the reject
/// cases fail to compile first.
struct NullDispatcher;
impl DispatchEffect<()> for NullDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond(serde_json::json!(0))
    }
}

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
        "Tidepool.QQ (fmt, j, patch, sg, uri)",
        "",
        None,
        None,
    );
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib").leak() as &Path;
    let lib = root.join(".tidepool/lib").leak() as &Path;
    let include = [hs, lib, effects_dir];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{e}")),
    }
}

#[test]
fn validator_rejects() {
    // Generous stack: importing Tidepool.QQ pulls the whole quoter graph
    // (incl. the ghc-package-backed HsMeta), whose meta can exhaust 2MB.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    // --- sg: the documented $$NAME silent-no-match trap ---
    let err =
        try_compile("[sg|fn $$NAME|]").expect_err("[sg|$$NAME|] must be rejected at compile time");
    assert!(
        err.contains("$$$NAME") && err.contains("$NAME"),
        "sg '$$' rejection should suggest both $$$NAME (multi) and $NAME (single); got:\n{err}"
    );
    // sg: lowercase metavariable name.
    let err = try_compile("[sg|fn $name|]").expect_err("[sg|$name|] (lowercase) must be rejected");
    assert!(
        err.to_lowercase().contains("uppercase"),
        "sg lowercase rejection should mention UPPERCASE; got:\n{err}"
    );
    // sg: unbalanced bracket.
    try_compile("[sg|foo($BAR|]").expect_err("[sg|] with an unbalanced bracket must be rejected");
    // sg: a valid pattern compiles and runs.
    try_compile("[sg|fn $NAME($$$ARGS)|]").expect("a valid ast-grep pattern should compile");

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
