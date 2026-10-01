//! Source capture only: outer recipes run through the actual prepared runtime.
//! Checked actor cells are retained, never executed or treated as behavior proof.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tidepool_bridge::FromHaskell;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectDispatch};
use tidepool_effect::{EffectError, Response};
use tidepool_runtime::HaskellValue;

#[path = "../../facade/src/generated/recipe_check.rs"]
mod recipe_check;
use recipe_check::RecipeCheckReq;

#[path = "../../facade/src/actor_host/effect_vocabulary.rs"]
mod effect_vocabulary;

type ActorKey = (String, i64, i64);

struct Capture {
    workspace: PathBuf,
    scratch: PathBuf,
    cells: usize,
    epoch: i64,
    next_actor: i64,
    files: HashMap<(ActorKey, String), String>,
    manifest: String,
    assertions: Vec<(String, bool)>,
}

impl Capture {
    fn key(&self, id: i64) -> ActorKey {
        ("source-capture".to_owned(), id, self.epoch)
    }

    fn read(&self, actor: ActorKey, requested: String) -> Result<String, EffectError> {
        if let Some(contents) = self.files.get(&(actor, requested.clone())) {
            return Ok(contents.clone());
        }
        let path = if let Some(relative) = requested.strip_prefix(".exomonad/workspace/") {
            self.workspace.join(relative)
        } else if let Some(relative) = requested.strip_prefix(".exomonad/") {
            self.workspace.join(relative)
        } else {
            PathBuf::from(requested)
        };
        std::fs::read_to_string(&path).map_err(|error| {
            EffectError::Handler(format!("source capture read {}: {error}", path.display()))
        })
    }
}

impl DispatchEffect for Capture {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_>,
    ) -> Result<Option<Response>, EffectError> {
        let response = match RecipeCheckReq::from_value(request, cx.table())? {
            RecipeCheckReq::RecipeRoot => cx.respond(self.key(1))?,
            RecipeCheckReq::RecipeTurn(actor, source) => {
                let name = format!("cell-{:04}.hs", self.cells);
                std::fs::write(self.scratch.join(&name), source)
                    .map_err(|error| EffectError::Handler(error.to_string()))?;
                self.manifest.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{name}\n",
                    self.cells, actor.0, actor.1, actor.2
                ));
                self.cells += 1;
                cx.respond("{\"status\":\"committed\",\"items\":[]}".to_owned())?
            }
            RecipeCheckReq::RecipeAssert(label, observed) => {
                self.assertions.push((label, observed));
                cx.respond(())?
            }
            RecipeCheckReq::RecipeRead(actor, path) => cx.respond(self.read(actor, path)?)?,
            RecipeCheckReq::RecipeWrite(actor, path, contents) => {
                self.files.insert((actor, path), contents);
                cx.respond(())?
            }
            RecipeCheckReq::RecipeActivation => {
                let id = self.next_actor;
                self.next_actor += 1;
                cx.respond((
                    self.key(id),
                    (
                        format!("capture-actor-{id}"),
                        "source capture only".to_owned(),
                        Some("gpt-6.1-sol".to_owned()),
                    ),
                ))?
            }
            RecipeCheckReq::RecipeGit(_, args) => {
                let result = if args.first().is_some_and(|arg| arg == "rev-parse") {
                    "1111111111111111111111111111111111111111"
                } else {
                    ""
                };
                cx.respond(result.to_owned())?
            }
            RecipeCheckReq::RecipePresent
            | RecipeCheckReq::RecipeNotPresented(_)
            | RecipeCheckReq::RecipeUnconfirmed(_) => cx.respond(String::new())?,
            RecipeCheckReq::RecipeRestart => {
                self.epoch += 1;
                cx.respond("source-capture-workspace".to_owned())?
            }
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
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let workspace = PathBuf::from(&args[2]);
    let scratch = PathBuf::from(&args[3]);
    let entry = &args[4];
    let support = PathBuf::from(&args[5]);
    std::fs::create_dir_all(&scratch).unwrap();
    let (module, _) = entry.rsplit_once('.').expect("qualified recipe entry");
    let mut declarations = effect_vocabulary::exomonad_effect_declarations();
    declarations.push(tidepool_mcp::recipe_check_decl());
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut includes = effects.include_paths().to_vec();
    includes.extend([
        root.join("bridge/haskell/lib"),
        root.join("bridge/haskell/actors"),
        workspace.clone(),
        support,
    ]);
    if let Some(extra) = std::env::var_os("TIDEPOOL_CHECK_INCLUDE") {
        includes.extend(std::env::split_paths(&extra));
    }
    let refs: Vec<&Path> = includes.iter().map(PathBuf::as_path).collect();
    let preamble = format!("{{-# LANGUAGE DataKinds, OverloadedStrings #-}}\nmodule CaptureRecipe where\nimport Control.Monad.Freer\nimport Tidepool.Check (RecipeCheck)\nimport qualified {module}\n");
    let source = tidepool_runtime::session::assemble_expression_module(
        &preamble,
        "result",
        "'[RecipeCheck]",
        entry,
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    std::fs::write(scratch.join("outer-recipe.hs"), &source).unwrap();
    let mut capture = Capture {
        workspace,
        scratch: scratch.clone(),
        cells: 0,
        epoch: 1,
        next_actor: 2,
        files: HashMap::new(),
        manifest: "ordinal\tnamespace\tactor\tincarnation\tfile\n".to_owned(),
        assertions: vec![],
    };
    let result = tidepool_runtime::compile_and_run(&source, "result", &refs, &mut capture, &());
    std::fs::write(scratch.join("manifest.tsv"), &capture.manifest).unwrap();
    std::fs::write(
        scratch.join("host-assertions.txt"),
        format!("{:?}", capture.assertions),
    )
    .unwrap();
    assert!(result.is_ok(), "source capture {entry}: {result:?}");
    assert!(capture.cells > 0, "source capture emitted no cells");
    println!(
        "captured {entry}: {} cells; {} host assertions recorded, not validated",
        capture.cells,
        capture.assertions.len()
    );
}
