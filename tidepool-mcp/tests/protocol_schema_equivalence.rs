//! This migration's acceptance bar, mechanically: the `EffectDecl` a migrated
//! effect produces must be identical, field for field, to what the schema in
//! `tidepool-protocol` renders.
//!
//! This test is written to run BEFORE the flip, against the still-hand-written
//! `<eff>_effect_def!` macro — that is what makes it a proof rather than a
//! tautology. Green here means the schema reproduces the hand-maintained
//! artifact exactly, and only then is the hand copy deleted.
//!
//! It stays green after the flip, where it still earns its place: byte equality
//! of the generated FILE (`tidepool-protocol`'s `generated_files_are_current`)
//! and equality of the compiled VALUE are different failure modes. A bug in the
//! generator's Rust-string escaping could emit a file that differs from the
//! schema's intent while still round-tripping its own bytes; this catches that,
//! because it compares what the compiler actually built.
//!
//! Why the comparison is field-by-field rather than one `assert_eq!` on the
//! whole struct: `EffectDecl` is `&'static str` data, so a mismatch buried in a
//! 400-byte helper string is unreadable in a whole-struct dump. Each field
//! reports itself.

/// Every field of `EffectDecl` must be accounted for here. If a field is added
/// to `EffectDecl` and not to this list, the destructuring below stops
/// compiling — which is the point: a new contract field must not silently go
/// unproven.
fn assert_decl_matches_schema(decl: &tidepool_mcp::EffectDecl, eff: &tidepool_protocol::Effect) {
    // Exhaustive destructuring: adding a field to EffectDecl breaks this line.
    let tidepool_mcp::EffectDecl {
        type_name,
        description,
        prompt_card,
        constructors,
        type_defs,
        extra_imports,
        helpers,
        type_params,
        default_row_args,
        helpers_row_polymorphic,
    } = decl;

    assert_eq!(*type_name, eff.name, "type_name");
    assert_eq!(*description, eff.description_text(), "description");
    assert_eq!(
        prompt_card.map(str::to_string),
        eff.prompt_card_text(),
        "prompt_card"
    );
    assert_eq!(
        constructors.to_vec(),
        eff.constructor_signatures(),
        "constructors"
    );
    assert_eq!(type_defs.to_vec(), eff.type_def_texts(), "type_defs");
    assert_eq!(
        extra_imports.to_vec(),
        eff.extra_imports.to_vec(),
        "extra_imports"
    );
    assert_eq!(helpers.to_vec(), eff.helper_texts(), "helpers");
    let rendered_type_params: Vec<_> = eff.type_params.iter().map(|param| param.render()).collect();
    assert_eq!(
        type_params
            .iter()
            .map(|param| (*param).to_owned())
            .collect::<Vec<_>>(),
        rendered_type_params,
        "type_params"
    );
    assert_eq!(
        default_row_args.to_vec(),
        eff.default_row_args.to_vec(),
        "default_row_args"
    );
    assert_eq!(
        *helpers_row_polymorphic, eff.helpers_row_polymorphic,
        "helpers_row_polymorphic"
    );
}

#[test]
fn exec_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::exec_decl(),
        &tidepool_protocol::effects::exec::exec(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `journal_effect_def!` macro — see the module doc: green here is what makes
/// this a proof rather than a tautology, and it is the go-ahead the flip
/// waits on.
#[test]
fn journal_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::journal_decl(),
        &tidepool_protocol::effects::journal::journal(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `worktree_effect_def!` macro — see the module doc. Worktree is the first
/// migrated effect with a non-empty `type_defs` (thirteen declarations, seven
/// `ToJSON` instances, an eleven-variant error ADT) and the first with a
/// non-empty `extra_imports` that is schema data from the start, so this is the
/// widest the field-for-field comparison has ever been.
///
/// It is EXACT, with nothing excused. The two preceding commits are what made
/// it so: ten helpers the schema cannot represent moved to
/// `haskell/lib/Tidepool/Worktree.hs`, and the eleventh (`renderWorktreeError`)
/// moved once its caller did — so the four the schema DOES represent are the
/// whole list the macro emits.
#[test]
fn worktree_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::worktree_decl(),
        &tidepool_protocol::effects::worktree::worktree(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `event_effect_def!` macro — see the module doc. `RepoEvent` is the second
/// effect to retire a `Wt*`/`Ev*`-family hand-written wire block, and the
/// first whose `type_defs` reference ANOTHER effect's own types (`Watch`
/// names Worktree's `WorktreeId`) and the first with a genuinely polymorphic
/// authored type (`Event a`/`Observed a`) that the schema cannot represent at
/// all — both relocate to `haskell/lib/Tidepool/Event.hs` alongside eighteen
/// non-representable helpers (Worktree's lane only ever relocated helpers).
/// See `tidepool-protocol/src/effects/event.rs`'s module doc.
#[test]
fn event_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::event_decl(),
        &tidepool_protocol::effects::event::event(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `askuser_effect_def!` macro — see the module doc. #20 steps 2-3: the first
/// migrated effect with two constructors riding one GADT (`AskUserWith`/
/// `NoteWith`) and the first whose helpers are ALL representable as-is (both
/// `askUserRaw`/`noteRaw` are thin single-verb `send` wrappers) — nothing
/// relocates to `haskell/lib`.
#[test]
fn ask_user_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::askuser_decl(),
        &tidepool_protocol::effects::ask_user::ask_user(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `readstate_effect_def!` macro — see the module doc. #20 steps 2-3: the
/// smallest migrated effect (one nullary verb, one nullary helper).
#[test]
fn read_state_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::readstate_decl(),
        &tidepool_protocol::effects::read_state::read_state(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `runllmturn_effect_def!` macro — see the module doc. #20 steps 2-3
/// (helperbody-flip lane): the first migrated effect using
/// [`tidepool_protocol::schema::HelperBody::OpaqueForward`]/`OpaqueSited`,
/// the reviewed shapes that finally express the OPAQUE+`*Sited`+
/// `unsafeCoerce` delegation pattern, and the first whose `errors` ADT
/// (`InvocationExit`) has no `errors`-tagged verb of its own — it backs a
/// pure display helper (`renderInvocationExit`) and two OPAQUE forwards'
/// declared result types instead.
#[test]
fn run_llm_turn_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::runllmturn_decl(),
        &tidepool_protocol::effects::run_llm_turn::run_llm_turn(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `finalize_effect_def!` macro — see the module doc. The one effect whose
/// `Polymorphism::ArgBound` (`v`) now has a real consumer:
/// `finalize`/`finalizeSited`'s two-tyvar `forall v a effs.` signatures.
#[test]
fn finalize_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::finalize_decl(),
        &tidepool_protocol::effects::finalize::finalize(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `fork_effect_def!` macro — see the module doc. Both helpers are the
/// `OpaqueSited` shape's simplest real use: the site id rides as a bare
/// leading constructor argument, no payload object to build.
#[test]
fn fork_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::fork_decl(),
        &tidepool_protocol::effects::fork::fork(),
    );
}

/// Written to run BEFORE the flip, against the still-hand-written
/// `green_effect_def!` macro — see the module doc. The widest of the four:
/// `HelperBody::IntDecode` (`asyncStatus`) and `HelperBody::AsyncSpawnBody`
/// (`asyncSpawn`, the one helper in this whole schema fixed to concrete `M`
/// rather than `Eff effs`) both make their first and only real appearance
/// here.
#[test]
fn green_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::green_decl(),
        &tidepool_protocol::effects::green::green(),
    );
}

/// Actor is generated directly into its owning runtime crate rather than
/// migrating from a hand-written declaration. This still pins the compiled
/// `EffectDecl` value to the schema, independently of the generated-file
/// staleness guard.
#[test]
fn actor_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::actor_decl(),
        &tidepool_protocol::effects::actor::actor(),
    );
}

/// Deliberation is runtime suspension substrate generated directly from the
/// schema. Keep its compiled declaration pinned independently of file output.
#[test]
fn deliberate_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::deliberate_decl(),
        &tidepool_protocol::effects::deliberate::deliberate(),
    );
}

/// ActorLocal is indexed by a unary protocol constructor and an exit type;
/// this assertion therefore also proves that kinded parameters survive the
/// schema-to-MCP projection.
#[test]
fn actor_local_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::actor_local_decl(),
        &tidepool_protocol::effects::actor_local::actor_local(),
    );
}

/// Every effect the schema claims to own must actually be wired into
/// `tidepool-mcp` — a schema entry with no live decl would prove nothing while
/// looking like coverage.
#[test]
fn every_schema_effect_is_reachable() {
    let names: Vec<&str> = tidepool_protocol::effects::all()
        .iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "Exec",
            "Journal",
            "Worktree",
            "RepoEvent",
            "Deliberate",
            "AskUser",
            "ReadState",
            "RunLLMTurn",
            "Fork",
            "Finalize",
            "Green",
            "Actor",
            "ActorLocal",
        ],
        "the migrated set changed — add the new effect's equivalence assertion above"
    );
}
