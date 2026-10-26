//! Authenticated literal Bash compilation through the actual held-world owner.
use std::sync::Arc;

use super::compiler_test_support::OwnedEnvironment;
use tidepool_repr::SessionId;
use tidepool_runtime::session::turn::{
    consume_cell_program_item, finish_cell_program_admitted, finish_world_cell_plan_admitted,
    prepare_cell_program_admitted, prepare_world_cell_plan_admitted, TemplateSelector, TurnResult,
};
use tidepool_runtime::session::{
    resident_cell_check_template, resident_workbench_templates, ModuleEnv, ResidentOutcome,
    ResidentSession, RuntimeLexicalScopeLease, SessionLib, SessionRunContext, SourceImports,
};
use tidepool_testing::effect_surface::TestEffectSurface;

type Session = ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>;

fn action<T>(
    session: &mut Session,
    source: &Arc<RuntimeLexicalScopeLease>,
    run: impl FnOnce(&mut Session, &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
) -> T {
    let settled = session
        .with_source_compiler_transaction(
            source.original_source().clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            run,
        )
        .unwrap();
    assert!(settled.close().resources_retired());
    settled.into_outcome().action
}

fn bash_controls(shadow_provider: bool) {
    tidepool_testing::eval_harness::require_extract();
    let cache = tempfile::tempdir().unwrap();
    let _cache = OwnedEnvironment::set("TIDEPOOL_COMPILE_CACHE_DIR", Some(cache.path()));
    let root = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
    std::fs::write(
        root.path().join("CheckedQuoterFacade.hs"),
        include_str!("fixtures/CheckedQuoterFacade.hs"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("AliasQuoter.hs"),
        include_str!("fixtures/AliasQuoter.hs"),
    )
    .unwrap();
    if shadow_provider {
        let directory = root.path().join("Tidepool/QQ");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("Bash.hs"),
            include_str!("fixtures/ShadowBash.hs"),
        )
        .unwrap();
    }
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let lib = SessionLib::open(
        SessionId(1089),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        1024 * 1024,
        Some(lib),
    );
    let source = session.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    session
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let mut includes = effects.include_paths().to_vec();
    includes.insert(0, root.path().to_path_buf());
    super::compiler_test_support::install_world(
        &mut session,
        &source,
        &includes,
        effects.preamble(),
        effects.row(),
    );
    let execution = Arc::new(
        action(&mut session, &source, |session, _| {
            session.begin_ephemeral_private_execution(public)
        })
        .unwrap(),
    );
    session
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let imports = SourceImports::from_specs([
        "qualified CheckedQuoterFacade as Q",
        "CheckedQuoterFacade (bash)",
        "qualified AliasQuoter as Other",
        "Tidepool.Effects.Core (CommandSpec(..))",
    ])
    .template_text();
    let mut prepare = |label: &str, text: &str, execute: bool| -> Result<(), String> {
        let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let recipe = Arc::new(tidepool_extract_cmd::WorldCellRecipe {
            source: text.into(),
            template,
            turn_templates: templates
                .iter()
                .map(|template| {
                    let kind = match template.kind {
                        TemplateSelector::Decl => "decl",
                        TemplateSelector::Bind => "bind",
                        TemplateSelector::BindDiscard => "binddiscard",
                        TemplateSelector::Expr => "expr",
                    };
                    (kind.into(), template.source.clone())
                })
                .collect(),
        });
        let (snapshot, operand) = action(&mut session, &source, |session, _| {
            session.prepare_private_execution(execution.clone(), None)
        })
        .map_err(|error| format!("{label} snapshot: {error:?}"))?
        .into_parts();
        assert!(operand.is_none());
        let settled = source
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, _| prepare_world_cell_plan_admitted(owner, snapshot.world(), recipe, None),
            )
            .unwrap();
        let plan = finish_world_cell_plan_admitted(settled)
            .map_err(|error| format!("{label} plan: {error:?}"))?;
        let admission = action(&mut session, &source, |session, _| {
            session.admit_world_planned_cell_with_snapshot(execution.clone(), snapshot, plan, None)
        })
        .map_err(|error| format!("{label} admission: {error:?}"))?;
        let settled = source
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, report| prepare_cell_program_admitted(owner, admission, report),
            )
            .unwrap();
        let (checked, program) = finish_cell_program_admitted(settled)
            .map_err(|error| format!("{label} preparation: {error:?}"))?;
        assert!(!program.items().is_empty());
        assert!(program.items().iter().all(|item| item.native().is_some()));
        if execute {
            action(&mut session, &source, |session, report| {
                let prefix = session.begin_cell_program(program).unwrap().unwrap();
                for index in 0..checked.items.len() {
                    let item = checked.checked_item(index).unwrap();
                    let kind = item.kind();
                    let reservation = session.admit_checked_item(prefix.clone(), item).unwrap();
                    if kind == tidepool_toolchain::checked_cell::CheckedItemKind::Declaration {
                        session.adopt_checked_declaration(reservation).unwrap();
                        continue;
                    }
                    let TurnResult::Bind {
                        bound, compiled, ..
                    } = consume_cell_program_item(reservation.clone()).unwrap()
                    else {
                        panic!("{label}: expected native binding");
                    };
                    let [binder] = bound.as_slice() else {
                        panic!("{label}: singleton capture");
                    };
                    let outcome = session
                        .run_bind_with_sites(
                            &binder.name,
                            compiled.code(),
                            binder,
                            reservation.generation(),
                            report,
                        )
                        .unwrap();
                    assert!(
                        matches!(outcome, ResidentOutcome::Completed { .. }),
                        "{label}: {outcome:?}"
                    );
                }
            });
        }
        Ok(())
    };
    if shadow_provider {
        let error = prepare(
            "same-name provider replacement",
            "result <- pure (Q.describe [Q.bash|literal|])",
            false,
        )
        .expect_err("a workspace replacement cannot borrow shipped expansion semantics");
        assert!(
            error.contains("literal Bash source authority refused"),
            "{error}"
        );
        return;
    }
    for phase in ["cold", "warm"] {
        prepare(
            phase,
            "result <- pure (Q.describe [Q.bash|literal|])",
            false,
        )
        .unwrap();
        prepare(
            "unqualified",
            "result <- pure (Q.describe [bash|literal|])",
            false,
        )
        .unwrap();
    }
    prepare(
        "two distinct quote sites",
        "pair <- pure (Q.describe [Q.bash|first|], Q.describe [Q.bash|second|])",
        false,
    )
    .unwrap();
    for (label, text) in [
        (
            "local let",
            "result <- pure (let bash = (1 :: Int) in [bash|literal|])",
        ),
        (
            "lambda",
            "result <- pure ((\\bash -> [bash|literal|]) (1 :: Int))",
        ),
        (
            "case pattern",
            "result <- pure (case (1 :: Int) of bash -> [bash|literal|])",
        ),
        (
            "top declaration",
            "bash = (1 :: Int)\nresult <- pure [bash|literal|]",
        ),
        (
            "other quoter",
            "result <- pure (Q.describe [Other.other|literal|])",
        ),
        (
            "same occurrence, different defining Name",
            "result <- pure (Q.describe [Other.bash|literal|])",
        ),
        (
            "pattern quote",
            "result <- pure (case () of [Q.bash|literal|] -> ())",
        ),
        (
            "type quote",
            "result <- pure (undefined :: [Q.bash|literal|])",
        ),
        (
            "declaration quote",
            "class BashDeclaration a where\n  [Q.bash|literal|]",
        ),
        (
            "untyped splice",
            "{-# LANGUAGE TemplateHaskell #-}\nresult <- pure $(pure [|1 :: Int|])",
        ),
        (
            "typed splice",
            "{-# LANGUAGE TemplateHaskell #-}\nresult <- pure $$(pure [||1 :: Int||])",
        ),
    ] {
        assert!(
            prepare(label, text, false).is_err(),
            "{label} unexpectedly admitted"
        );
    }
    prepare(
        "literal bytes and native execution",
        include_str!("fixtures/bash-literal-bytes.hs"),
        true,
    )
    .unwrap();
    prepare("notebook alias", "bash <- pure (17 :: Int)", true).unwrap();
    assert!(prepare(
        "notebook alias shadow",
        "result <- pure [bash|literal|]",
        false
    )
    .is_err());
    prepare(
        "notebook demand beside a literal quote",
        "outside <- pure (bash, Q.describe [Q.bash|literal|])",
        true,
    )
    .unwrap();
    prepare(
        "qualified quote survives unrelated alias",
        "result <- pure (Q.describe [Q.bash|literal|])",
        false,
    )
    .unwrap();
}

#[test]
#[serial_test::serial]
fn held_world_literal_bash_preserves_scope_and_bytes() {
    bash_controls(false);
}

#[test]
#[serial_test::serial]
fn held_world_literal_bash_refuses_same_named_workspace_provider() {
    bash_controls(true);
}
