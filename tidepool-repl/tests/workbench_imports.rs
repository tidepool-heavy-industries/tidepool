//! Persistent import discovery and rejection recovery through the real REPL.

use crate::common;

use common::*;

#[tokio::test]
async fn show_imports_tracks_the_environment_used_by_type_and_declarations() {
    require_extract();
    let repl = Repl::new();

    let initial = repl.cmd(":show imports").await;
    assert!(initial
        .expect_ok("initial :show imports")
        .contains("\"imports\":[]"));

    repl.cmd("import qualified Data.Set as Set")
        .await
        .expect_ok("persistent import");
    let shown = repl.cmd(":show imports").await;
    assert!(
        shown
            .expect_ok(":show imports after import")
            .contains("import qualified Data.Set as Set"),
        "{shown:?}"
    );
    repl.cmd(":type Set.empty")
        .await
        .expect_ok(":type sees persistent import");
    repl.cmd("emptySet = Set.empty :: Set.Set Int")
        .await
        .expect_ok("declaration sees persistent import");
    repl.cmd(":type emptySet")
        .await
        .expect_ok(":type sees imported declaration");

    let unsupported = repl.cmd(":show modules").await;
    assert!(
        unsupported
            .expect_err("unsupported :show argument")
            .contains(":show does not support `modules` (supported: :show imports)"),
        "{unsupported:?}"
    );
    repl.cmd(":bindings")
        .await
        .expect_ok("session recovers after rejected :show");
}
