//! Exercises the compiled helper receipt parser and host assertion boundary.
//! Receipt strings are parser inputs; actor publication is not exercised here.
use std::path::PathBuf;
use tidepool_bridge::FromHaskell;
use tidepool_codegen::host_fns::RuntimeError as HaskellError;
use tidepool_codegen::prepared_program::ExecutionError;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectDispatch};
use tidepool_effect::{EffectError, Response};
use tidepool_runtime::session::prepared::PreparedRuntimeError;
use tidepool_runtime::HaskellValue;

use crate::generated::recipe_check::RecipeCheckReq;

struct Host {
    receipt: String,
    helper: String,
    successes: Vec<String>,
    cells: Vec<String>,
    attempts: Vec<Attempt>,
}

#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    Root,
    SelectorRead,
    Turn,
    Assert(bool),
}

impl DispatchEffect for Host {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_>,
    ) -> Result<Option<Response>, EffectError> {
        let response = match RecipeCheckReq::from_value(request, cx.table())? {
            RecipeCheckReq::RecipeRoot => {
                self.attempts.push(Attempt::Root);
                cx.respond(("fixture".to_owned(), 1_i64, 1_i64))?
            }
            RecipeCheckReq::RecipeTurn(actor, source) => {
                assert_eq!(actor, ("fixture".to_owned(), 1, 1));
                self.attempts.push(Attempt::Turn);
                self.cells.push(source);
                cx.respond(self.receipt.clone())?
            }
            RecipeCheckReq::RecipeRead(_, path) if path == "helper" => {
                self.attempts.push(Attempt::SelectorRead);
                cx.respond(self.helper.clone())?
            }
            RecipeCheckReq::RecipeAssert(label, observed) => {
                self.attempts.push(Attempt::Assert(observed));
                if !observed {
                    return Err(EffectError::Handler(label));
                }
                self.successes.push(label);
                cx.respond(())?
            }
            _ => panic!("unexpected recipe effect"),
        };
        Ok(Some(response))
    }

    fn prepare_dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_>,
    ) -> Result<EffectDispatch, EffectError> {
        self.dispatch(request, cx)
            .map(|response| response.map_or(EffectDispatch::Unhandled, EffectDispatch::Immediate))
    }
}

#[test]
fn prepared_recipe_receipts_and_assertions_execute_all_eight_cases() {
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    tidepool_testing::eval_harness::require_extract();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path();
    let required = |name| {
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| {
            panic!("{name} must name a declared native compiler source resource")
        }))
    };
    let includes = [
        required("TIDEPOOL_EFFECTS_SOURCE_ROOT"),
        required("TIDEPOOL_PRELUDE_DIR"),
        required("TIDEPOOL_HASKELL_ACTORS_DIR"),
    ];
    let preamble = "{-# LANGUAGE DataKinds, OverloadedStrings #-}\nmodule RecipeContract where\nimport Control.Monad.Freer\nimport qualified Tidepool.Check as Check\n";
    let expression = tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/recipe_checks/prepared_contract_expression.hs",
    );
    let source = tidepool_runtime::session::assemble_expression_module(
        preamble,
        "result",
        "'[Check.RecipeCheck]",
        &expression,
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    std::fs::write(scratch.join("contract.hs"), &source).unwrap();
    let target = tidepool_runtime::session::PREPARED_SCAFFOLD_TARGET;
    let compiled =
        tidepool_runtime::compile_targets(&source, &[target], &includes, |_, _, _| {}).unwrap();
    let cases = [
        (
            "committed",
            "cell",
            "{\"status\":\"committed\",\"items\":[{\"output\":\"False\"}]}",
            true,
        ),
        (
            "replied",
            "cell",
            "{\"status\":\"replied\",\"items\":[{\"output\":\"False\"}]}",
            true,
        ),
        (
            "await-committed",
            "await",
            "{\"status\":\"committed\"}",
            true,
        ),
        ("await-replied", "await", "{\"status\":\"replied\"}", true),
        (
            "failed",
            "cell",
            "{\"status\":\"failed\",\"items\":[{\"output\":\"True\"}]}",
            false,
        ),
        ("rejected", "await", "{\"status\":\"rejected\"}", false),
        ("malformed", "cell", "invalid-json", false),
        ("discarded-false", "false", "", false),
    ];
    for (label, helper, receipt, succeeds) in cases {
        let mut host = Host {
            receipt: receipt.to_owned(),
            helper: helper.to_owned(),
            successes: vec![],
            cells: vec![],
            attempts: vec![],
        };
        let result = tidepool_runtime::run_compiled_target(
            &compiled,
            target,
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            &mut host,
            &(),
            |_| {},
        );
        assert_eq!(result.is_ok(), succeeds, "{label}: {result:?}");
        let expected_attempts = if helper == "false" {
            vec![Attempt::Root, Attempt::SelectorRead]
        } else {
            vec![
                Attempt::Root,
                Attempt::SelectorRead,
                Attempt::Turn,
                Attempt::Assert(succeeds),
            ]
        };
        assert_eq!(
            host.attempts, expected_attempts,
            "{label}: exact effect attempts"
        );
        if !succeeds {
            if helper == "false" {
                assert!(
                    matches!(
                        &result,
                        Err(tidepool_runtime::RuntimeError::Prepared(
                            PreparedRuntimeError::Run(ExecutionError::Runtime(failure))
                        )) if matches!(&failure.cause,
                            HaskellError::UserError | HaskellError::UserErrorMsg(_)
                            | HaskellError::RaisedException | HaskellError::RaisedExceptionMessage(_)
                        )
                    ),
                    "{label}: expected a typed Haskell error after selector read: {result:?}"
                );
                assert!(
                    host.cells.is_empty(),
                    "{label}: unexpectedly submitted an actor cell"
                );
            } else {
                assert!(
                    matches!(
                        &result,
                        Err(tidepool_runtime::RuntimeError::Prepared(
                            PreparedRuntimeError::Handler { .. }
                        ))
                    ),
                    "{label}: expected failed host assertion after exact turn: {result:?}"
                );
                assert_eq!(
                    host.cells.len(),
                    1,
                    "{label}: expected exactly one actor cell"
                );
            }
        }
        assert_eq!(
            host.successes.len(),
            usize::from(succeeds),
            "{label}: success count"
        );
        if succeeds {
            assert_eq!(host.successes, ["host assertion"]);
        }
        for (index, cell) in host.cells.iter().enumerate() {
            std::fs::write(scratch.join(format!("{label}-cell-{index}.hs")), cell).unwrap();
        }
        std::fs::write(
            scratch.join(format!("{label}-attempts.txt")),
            format!("attempts={:?}\nresult={result:?}\n", host.attempts),
        )
        .unwrap();
        println!(
            "passed: {label}; accepted={succeeds}; host successes={}",
            host.successes.len()
        );
    }
    println!(
        "executed: 8 prepared-runtime helper contract cases from one compiled immutable fixture"
    );
}
