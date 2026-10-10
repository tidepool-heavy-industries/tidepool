//! Executes the authored outer recipe and records its cells as diagnostic data.
//! These cells and host assertions do not prove command execution or cleanup.
use super::{authored_workspace_root, resource_module};
use crate::generated::recipe_check::RecipeCheckReq;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tidepool_bridge::FromHaskell;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectDispatch};
use tidepool_effect::{EffectError, Response};
use tidepool_runtime::HaskellValue;

type ActorKey = (String, i64, i64);
struct Capture {
    workspace: PathBuf,
    cells: Vec<(ActorKey, String)>,
    assertions: Vec<(String, bool)>,
}
impl DispatchEffect for Capture {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_>,
    ) -> Result<Option<Response>, EffectError> {
        let response = match RecipeCheckReq::from_value(request, cx.table())? {
            RecipeCheckReq::RecipeRoot => {
                cx.respond(("source-capture".to_owned(), 1_i64, 1_i64))?
            }
            RecipeCheckReq::RecipeRead(_, requested) => {
                let relative = requested.strip_prefix(".exomonad/").ok_or_else(|| {
                    EffectError::Handler(format!("unexpected capture path {requested}"))
                })?;
                let path = self.workspace.join(relative);
                let text = std::fs::read_to_string(&path).map_err(|error| {
                    EffectError::Handler(format!("read {}: {error}", path.display()))
                })?;
                cx.respond(text)?
            }
            RecipeCheckReq::RecipeTurn(actor, source) => {
                self.cells.push((actor, source));
                // An input to the outer recipe's receipt parser, with no
                // publication certificate or acceptance into an actor runtime.
                cx.respond("{\"status\":\"committed\",\"items\":[]}".to_owned())?
            }
            RecipeCheckReq::RecipeAssert(label, observed) => {
                self.assertions.push((label, observed));
                cx.respond(())?
            }
            _ => panic!("background completion capture requested an unexpected effect"),
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
fn background_command_recipe_capture_records_seven_cells_without_validating_assertions() {
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    tidepool_testing::eval_harness::require_extract();
    let required = |name| {
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| {
            panic!("{name} must name a declared native compiler source resource")
        }))
    };
    let workspace = required("TIDEPOOL_RECIPE_WORKSPACE");
    let config: crate::exomonad::ExomonadConfig =
        toml::from_str(&std::fs::read_to_string(workspace.join("config.toml")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let resources = scratch.path().join("resources");
    let generated = resources.join("Exomonad/Workspace.hs");
    std::fs::create_dir_all(generated.parent().unwrap()).unwrap();
    std::fs::write(
        &generated,
        resource_module(
            "source-capture-diagnostic",
            &authored_workspace_root(&config.haskell.source_roots),
            &config.haskell.modules,
            &BTreeMap::new(),
        ),
    )
    .unwrap();
    let includes = [
        required("TIDEPOOL_EFFECTS_SOURCE_ROOT"),
        required("TIDEPOOL_PRELUDE_DIR"),
        required("TIDEPOOL_HASKELL_ACTORS_DIR"),
        workspace.clone(),
        resources,
    ];
    let refs: Vec<&Path> = includes.iter().map(PathBuf::as_path).collect();
    let source = tidepool_runtime::session::assemble_expression_module(
        "{-# LANGUAGE DataKinds, OverloadedStrings #-}\nmodule CaptureRecipe where\nimport Control.Monad.Freer\nimport Tidepool.Check (RecipeCheck)\nimport qualified Project.BackgroundCommandExampleChecks\n",
        "result",
        "'[RecipeCheck]",
        "Project.BackgroundCommandExampleChecks.completion",
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    let mut capture = Capture {
        workspace,
        cells: Vec::new(),
        assertions: Vec::new(),
    };
    let result = tidepool_testing::with_settlement(|settlement| {
        tidepool_runtime::compile_and_run(&source, "result", &refs, &mut capture, &(), settlement)
    });
    assert!(result.is_ok(), "actual outer recipe capture: {result:?}");
    assert_eq!(capture.cells.len(), 7, "outer recipe cell count");
    assert!(capture.cells.iter().all(|(actor, source)| actor
        == &("source-capture".to_owned(), 1, 1)
        && !source.trim().is_empty()));
    assert_eq!(
        capture
            .assertions
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>(),
        [
            "late command completion keeps a compact typed projection",
            "full command evidence remains available after the projection",
            "a failed command retains its outcome and both streams",
            "failed execution evidence stays accessible without rerunning",
        ]
    );
    for (ordinal, (_, source)) in capture.cells.iter().enumerate() {
        println!("captured cell {ordinal}: {source}");
    }
    println!("source-capture diagnostic: 7 cells; 4 host assertions recorded, not validated");
}
