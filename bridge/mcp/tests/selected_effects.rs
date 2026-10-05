use std::path::PathBuf;

#[test]
fn inferred_cells_compile_without_an_implicit_row_alias() {
    let declarations = [tidepool_mcp::console_decl()];
    let installed = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let stdlib = PathBuf::from(
        std::env::var_os("TIDEPOOL_PRELUDE_DIR")
            .expect("TIDEPOOL_PRELUDE_DIR must name the declared Haskell library resource"),
    );
    let preamble = tidepool_mcp::build_notebook_preamble(&declarations, false);
    let compile = |expression| {
        let source = tidepool_runtime::session::assemble_opaque_expression_module(
            &preamble,
            "result",
            "'[Console]",
            expression,
            tidepool_runtime::session::ExpressionLift::Effectful,
        );
        tidepool_runtime::compile_haskell(
            &source,
            "result",
            &[&installed.core, &installed.orchestration, &stdlib],
        )
    };
    compile("pure (41 :: Int)").expect("ordinary cells infer their selected executable row");
    let error = compile("pure (41 :: M)").expect_err("M requires an authored declaration");
    let tidepool_runtime::CompileError::Diagnostics(diagnostics) = error else {
        panic!("expected a compiler name rejection, got {error:?}");
    };
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("Not in scope") && diagnostic.message.contains('M')
    }));
}

#[test]
fn concurrent_selected_profiles_enforce_membership_with_stable_vocabulary() {
    let installed = tidepool_mcp::ensure_effects_module(&[
        tidepool_mcp::console_decl(),
        tidepool_mcp::context_read_write_decl(),
    ])
    .expect("installed effects");
    let original = std::fs::read(installed.core.join("Tidepool/Effects.hs")).unwrap();
    let stdlib = PathBuf::from(
        std::env::var_os("TIDEPOOL_PRELUDE_DIR")
            .expect("TIDEPOOL_PRELUDE_DIR must name the declared Haskell library resource"),
    );
    let preamble = include_str!("fixtures/selected-effects.hs");
    let compile = |row, expression| {
        let source = tidepool_runtime::session::assemble_opaque_expression_module(
            preamble,
            "result",
            row,
            expression,
            tidepool_runtime::session::ExpressionLift::Effectful,
        );
        tidepool_runtime::compile_haskell(
            &source,
            "result",
            &[&installed.core, &installed.orchestration, &stdlib],
        )
    };
    std::thread::scope(|scope| {
        let allowed = scope.spawn(|| compile("'[ContextReadWrite]", "readContext"));
        let denied = scope.spawn(|| compile("'[Console]", "readContext"));
        allowed
            .join()
            .unwrap()
            .expect("selected ContextReadWrite is available");
        let error = denied
            .join()
            .unwrap()
            .expect_err("Console-only row must not satisfy Member ContextReadWrite");
        assert!(
            matches!(error, tidepool_runtime::CompileError::Diagnostics(_)),
            "expected a compiler membership rejection, got {error:?}"
        );
    });
    compile("'[ContextReadWrite]", "explicitAlias")
        .expect("an explicitly authored alias is an ordinary declaration");
    assert!(matches!(
        compile("'[Console]", "readContext")
            .expect_err("a permitted cache entry cannot grant ContextReadWrite"),
        tidepool_runtime::CompileError::Diagnostics(_),
    ));
    assert_eq!(
        std::fs::read(installed.core.join("Tidepool/Effects.hs")).unwrap(),
        original
    );
    assert_eq!(
        tidepool_mcp::ensure_effects_module(&[tidepool_mcp::context_read_write_decl()])
            .unwrap()
            .core,
        installed.core
    );
}
