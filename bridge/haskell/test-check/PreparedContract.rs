use std::path::{Path, PathBuf};
use tidepool_bridge::FromHaskell;
use tidepool_codegen::host_fns::RuntimeError as HaskellError;
use tidepool_codegen::prepared_program::ExecutionError;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectDispatch};
use tidepool_effect::{EffectError, Response};
use tidepool_runtime::session::prepared::PreparedRuntimeError;
use tidepool_runtime::HaskellValue;

#[path = "../../facade/src/generated/recipe_check.rs"]
mod recipe_check;
use recipe_check::RecipeCheckReq;

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

fn main() {
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let root = PathBuf::from(std::env::args().nth(1).expect("repository root"));
    let scratch = PathBuf::from(std::env::args().nth(2).expect("scratch output"));
    std::fs::create_dir_all(&scratch).unwrap();
    let effects =
        tidepool_mcp::ensure_effects_module(&[tidepool_mcp::recipe_check_decl()]).unwrap();
    let mut includes = effects.include_paths().to_vec();
    includes.extend([
        root.join("bridge/haskell/lib"),
        root.join("bridge/haskell/actors"),
    ]);
    let refs: Vec<&Path> = includes.iter().map(PathBuf::as_path).collect();
    let preamble = "{-# LANGUAGE DataKinds, OverloadedStrings #-}\nmodule RecipeContract where\nimport Control.Monad.Freer\nimport qualified Tidepool.Check as Check\n";
    let expression = "do { actor <- Check.root; helper <- Check.readFile actor \"helper\"; if helper == \"false\" then Check.assertThat \"discarded false\" False else if helper == \"await\" then Check.awaitCell actor \"host assertion\" \"pure True\" else Check.assertCell actor \"host assertion\" \"True\"; pure (42 :: Int) }";
    let source = tidepool_runtime::session::assemble_expression_module(
        preamble,
        "result",
        "'[Check.RecipeCheck]",
        expression,
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    std::fs::write(scratch.join("contract.hs"), &source).unwrap();
    let compiled = tidepool_runtime::compile_haskell(
        &source,
        tidepool_runtime::session::PREPARED_SCAFFOLD_TARGET,
        &refs,
    )
    .unwrap();
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
        let result = tidepool_runtime::run_prepared_program(
            compiled.prepared.clone().into_prepared(),
            &compiled.table,
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
