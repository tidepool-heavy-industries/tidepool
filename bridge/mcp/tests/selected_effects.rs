use std::path::Path;

#[test]
fn concurrent_selected_profiles_enforce_membership_and_preserve_shared_shim() {
    let installed = tidepool_mcp::ensure_effects_module(&[
        tidepool_mcp::console_decl(),
        tidepool_mcp::context_read_write_decl(),
    ])
    .expect("installed effects");
    let original = std::fs::read(installed.shim.join("Tidepool/Effects.hs")).unwrap();
    let permitted = tidepool_mcp::ensure_selected_effects_shim("'[Exomonad.ContextReadWrite]")
        .expect("permitted profile");
    let forbidden = tidepool_mcp::ensure_selected_effects_shim("'[Exomonad.Console]")
        .expect("profile without context editing");
    assert_ne!(permitted, forbidden);
    assert_ne!(permitted, installed.shim);
    let stdlib = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../haskell/lib")
        .canonicalize()
        .unwrap();
    let source = include_str!("fixtures/selected-effects.hs");
    let compile = |selected: &Path| {
        tidepool_runtime::compile_haskell(
            source,
            "result",
            &[selected, &installed.core, &installed.shim, &stdlib],
        )
    };
    std::thread::scope(|scope| {
        let allowed = scope.spawn(|| compile(&permitted));
        let denied = scope.spawn(|| compile(&forbidden));
        allowed
            .join()
            .unwrap()
            .expect("selected ContextReadWrite is available");
        let error = denied
            .join()
            .unwrap()
            .expect_err("profile must reject ContextReadWrite through qualified Effects.M");
        assert!(
            matches!(error, tidepool_runtime::CompileError::Diagnostics(_)),
            "expected a compiler membership rejection, got {error:?}"
        );
    });
    compile(&permitted).expect("permitted recipe survives rejected neighbor");
    assert!(matches!(
        compile(&forbidden).expect_err("a permitted cache entry cannot grant ContextReadWrite"),
        tidepool_runtime::CompileError::Diagnostics(_),
    ));
    assert_eq!(
        std::fs::read(installed.shim.join("Tidepool/Effects.hs")).unwrap(),
        original,
    );
    assert_eq!(
        tidepool_mcp::ensure_selected_effects_shim("'[Exomonad.ContextReadWrite]").unwrap(),
        permitted,
    );
}
