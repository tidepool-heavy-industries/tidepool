//! Throwaway: write the exact eval-module source for the #313 repro so
//! tidepool-extract --dump-core can show where `undefined` enters.
#[test]
fn dump_eval_source_for_313() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do("x <- t5\npure x"),
        "Probe",
        "",
        None,
        None,
    );
    std::fs::write("/tmp/expr313.hs", src).unwrap();
}
