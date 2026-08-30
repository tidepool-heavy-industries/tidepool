//! Real GHC -> Core -> JIT proof for the managed single-assignment cell used
//! by actor and green-thread exit handles.

use serde_json::json;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_testing::eval_harness::{require_extract, EvalHarness};

#[test]
fn copied_cells_share_a_closure_valued_result() {
    require_extract();
    let source = r#"
module Expr where

import Prelude
import Tidepool.Internal.ExitCell

result :: Bool
result =
  let cell = newExitCell ("pending" :: String)
      alias = cell
      other = newExitCell ("pending" :: String)
      pressure = and (replicate 4096 True)
  in case fillExitCell cell not of
       () ->
         case pressure of
           True ->
             case ( readExitCell pressure cell
                  , readExitCell pressure alias
                  , readExitCell pressure other
                  ) of
               (Just first, Just second, Nothing) -> first False && second False
               _ -> error "managed exit cells lost sharing or independence"
           False -> False
"#;

    let compiled = EvalHarness::new()
        .with_stdlib()
        .compile(source, "result")
        .expect("compile managed exit cell");
    let result = JitEffectMachine::compile(&compiled.expr, &compiled.table, 16 * 1024)
        .expect("compile managed exit cell JIT")
        .run_pure()
        .expect("run managed exit cell");
    assert_eq!(
        tidepool_runtime::value_to_json(&result, &compiled.table, 64),
        json!(true)
    );
}
