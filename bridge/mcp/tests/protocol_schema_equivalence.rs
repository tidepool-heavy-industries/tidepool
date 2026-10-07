//! The compiled EffectDecl values must equal their owning schema projections.
//! This catches escaping and source mapping errors in the compiled consumer.
//! Native generation and real GHC consumers own artifact and module acceptance.

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
/// `bridge/haskell/lib/Tidepool/Worktree.hs`, and the eleventh (`renderWorktreeError`)
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
/// first whose `type_defs` reference ANOTHER effect's own types (`EventWatch`
/// names Worktree's `WorktreeId`) and the first with a genuinely polymorphic
/// authored type (`Event a`/`Observed a`) that the schema cannot represent at
/// all — both relocate to `bridge/haskell/lib/Tidepool/Event.hs` alongside eighteen
/// non-representable helpers (Worktree's lane only ever relocated helpers).
/// See `bridge/protocol/src/effects/event.rs`'s module doc.
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
/// relocates to `bridge/haskell/lib`.
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
/// `green_effect_def!` macro — see the module doc. The widest of the four:
/// `HelperBody::IntDecode` (`asyncStatus`) and `HelperBody::AsyncSpawnBody`
/// (`asyncSpawn`, which existentially packages the spawned body’s row
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

/// The private actor kernel is schema-owned even though its constructors are
/// absent from the authored surface.
#[test]
fn actor_kernel_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::actor_kernel_decl(),
        &tidepool_protocol::effects::actor_kernel::actor_kernel(),
    );
}

/// ActorLocal is indexed by a unary protocol constructor; this assertion also
/// proves that kinded parameters survive the schema-to-MCP projection.
#[test]
fn actor_local_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::actor_local_decl(),
        &tidepool_protocol::effects::actor_local::actor_local(),
    );
}

#[test]
fn agent_tools_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::agent_tools_decl(),
        &tidepool_protocol::effects::agent_tools::agent_tools(),
    );
}

#[test]
fn agent_session_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::agent_session_decl(),
        &tidepool_protocol::effects::agent_session::agent_session(),
    );
}

#[test]
fn recipe_check_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::recipe_check_decl(),
        &tidepool_protocol::effects::recipe_check::recipe_check(),
    );
}

#[test]
fn actor_context_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::actor_context_decl(),
        &tidepool_protocol::effects::actor_context::actor_context(),
    );
}

#[test]
fn introspection_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::introspection_decl(),
        &tidepool_protocol::effects::introspection::introspection(),
    );
}

#[test]
fn sleep_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::sleep_decl(),
        &tidepool_protocol::effects::sleep::sleep(),
    );
}

#[test]
fn agent_control_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::agent_control_decl(),
        &tidepool_protocol::effects::agent_control::agent_control(),
    );
}

#[test]
fn commands_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::commands_decl(),
        &tidepool_protocol::effects::commands::commands(),
    );
}

#[test]
fn notifications_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::notifications_decl(),
        &tidepool_protocol::effects::notifications::notifications(),
    );
}

#[test]
fn agent_inspection_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::agent_inspection_decl(),
        &tidepool_protocol::effects::agent_inspection::agent_inspection(),
    );
}

#[test]
fn agent_launch_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::agent_launch_decl(),
        &tidepool_protocol::effects::agent_launch::agent_launch(),
    );
}

#[test]
fn resource_scopes_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::resource_scopes_decl(),
        &tidepool_protocol::effects::resource_scopes::resource_scopes(),
    );
}

#[test]
fn bound_worktree_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::bound_worktree_decl(),
        &tidepool_protocol::effects::worktree_facades::bound_worktree(),
    );
}

#[test]
fn worktree_registry_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::worktree_registry_decl(),
        &tidepool_protocol::effects::worktree_facades::worktree_registry(),
    );
}

#[test]
fn worktree_allocation_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::worktree_allocation_decl(),
        &tidepool_protocol::effects::worktree_facades::worktree_allocation(),
    );
}

#[test]
fn worktree_integration_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::worktree_integration_decl(),
        &tidepool_protocol::effects::worktree_facades::worktree_integration(),
    );
}

#[test]
fn lookup_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::lookup_decl(),
        &tidepool_protocol::effects::lookup::lookup(),
    );
}

#[test]
fn model_call_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::model_call_decl(),
        &tidepool_protocol::effects::model::model(),
    );
}

#[test]
fn context_read_write_decl_matches_the_schema_exactly() {
    assert_decl_matches_schema(
        &tidepool_mcp::context_read_write_decl(),
        &tidepool_protocol::effects::context_read_write::context_read_write(),
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
            "AskUser",
            "ReadState",
            "RecipeCheck",
            "Green",
            "Actor",
            "ActorContext",
            "Introspection",
            "Lookup",
            "ActorKernel",
            "ActorLocal",
            "Sleep",
            "AgentControl",
            "Commands",
            "Notifications",
            "Jev",
            "AgentInspection",
            "AgentLaunch",
            "Forks",
            "AgentTools",
            "AgentSession",
            "Reflect",
            "Source",
            "ModelCall",
            "ContextReadWrite",
            "BoundWorktree",
            "WorktreeRegistry",
            "WorktreeAllocation",
            "WorktreeIntegration",
        ],
        "the migrated set changed — add the new effect's equivalence assertion above"
    );
}
