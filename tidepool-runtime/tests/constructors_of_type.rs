//! `DataConTable::constructors_of_type` resolves a rendered parent-type name
//! (the extract's 8th DataCon metadata element) to its full constructor set,
//! in declaration order.

use tidepool_runtime::CompileResult;
use tidepool_testing::eval_harness::EvalHarness;

/// A nullary sum's declared constructors must all be resolvable by the
/// rendered type name, in `dataConTag` (declaration) order — regardless of
/// which single constructor the compiled binding actually uses.
#[test]
fn constructors_of_type_resolves_nullary_sum_in_declaration_order() {
    let src = r#"module Verdict where
data Verdict = GO | PARTIAL | NOGO deriving (Show)
target :: Verdict
target = GO
"#;

    let CompileResult { table, .. } = EvalHarness::new()
        .with_stdlib()
        .compile(src, "target")
        .expect("compilation failed");

    let ids = table.constructors_of_type("Verdict");
    let names: Vec<&str> = ids
        .iter()
        .map(|&id| table.name_of(id).expect("id from constructors_of_type must resolve"))
        .collect();

    assert_eq!(
        names,
        vec!["GO", "PARTIAL", "NOGO"],
        "constructors_of_type(\"Verdict\") must return all three constructors \
         in declaration order"
    );
}
