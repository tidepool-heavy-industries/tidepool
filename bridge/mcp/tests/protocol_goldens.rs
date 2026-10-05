//! Focused compatibility pins for the generated effect protocol.
//!
//! Generated-file currency and real Haskell compilation own full-surface
//! fidelity. This suite pins only the details whose accidental movement has
//! independent meaning: effect order, selected hand-authored contracts,
//! import gating, the standard row, and the concise tool index. It deliberately
//! does not snapshot the entire generated Core module or every schema field;
//! those migration-era mirrors made ordinary API evolution require rewriting
//! thousands of lines without adding a distinct invariant.

use tidepool_mcp::EffectDecl;

// ---------------------------------------------------------------------------
// The pinned decl list
// ---------------------------------------------------------------------------

/// Every authored declaration, including opt-in effects. Do not sort this
/// list: emission order is part of the contract.
fn pinned_decls() -> Vec<EffectDecl> {
    tidepool_mcp::all_decls()
}

#[test]
fn all_declaration_names_and_order_are_explicit() {
    let names: Vec<_> = pinned_decls().iter().map(|d| d.type_name).collect();
    assert_eq!(
        names,
        vec![
            "Console",
            "KV",
            "FsRead",
            "FsWrite",
            "Http",
            "Git",
            "Time",
            "Entropy",
            "Meta",
            "Ask",
            "Llm",
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
        ]
    );
}

// ---------------------------------------------------------------------------
// Immutable compile-time goldens
// ---------------------------------------------------------------------------

fn assert_matches_golden(name: &str, generated: &str, committed: &str) {
    assert_eq!(
        committed, generated,
        "committed {name} is stale vs the generated artifact; review the contract \
         change and update its owning golden source"
    );
}

/// Length-prefix a generated artifact so adjacent sections remain
/// unambiguous even when their contents contain arbitrary newlines.
fn write_blob(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("-- {label} ({} bytes) --\n", value.len()));
    out.push_str(value);
    if !value.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("-- end {label} --\n\n"));
}

// ---------------------------------------------------------------------------
// The derived tool-description effects index over the standard row.
// ---------------------------------------------------------------------------

#[test]
fn tool_description_effects_index_golden_matches_committed_file() {
    let generated = tidepool_mcp::describe_effects_index(&tidepool_mcp::standard_decls());
    assert_matches_golden(
        "tool_description.effects_index.txt",
        &generated,
        include_str!("goldens/protocol/tool_description.effects_index.txt"),
    );
}

// ---------------------------------------------------------------------------
// Layer 2 — an INDEPENDENT hardcoded pin, modelled on
// `bridge/mcp/src/preamble.rs`'s `import_gating_pin` module doc: these are
// literal strings transcribed BY HAND from the hand-written definitions, NOT
// read from any golden file and NOT produced by a schema renderer.
// Their whole job is to stay true even when someone regenerates
// a generated artifact blindly: a generator bug or silent definition drift
// cannot launder itself into a string written independently from the source.
//
// The Exec pins have since outgrown that job. They were transcribed from
// `effect_defs.rs`'s `exec_effect_def!` while it still existed; that macro is
// now DELETED and `exec_decl()` is generated from the `tidepool-protocol`
// schema. So these particular literals are a hand-written
// record of the pre-migration contract that the generated output still
// satisfies — the most direct byte-compatibility evidence in the tree. Do not
// "update" them to match a future generator change: a diff here means the
// contract moved, which is a decision, not a refresh.
// ---------------------------------------------------------------------------

#[test]
fn hardcoded_pins_survive_a_blind_regen() {
    let exec = tidepool_mcp::exec_decl();
    assert_eq!(
        exec.constructors.to_vec(),
        vec![
            "Run :: Text -> Exec (Either ExecError Proc)",
            "RunIn :: Text -> Text -> Exec (Either ExecError Proc)",
            "RunArgv :: [Text] -> Exec (Either ExecError Proc)",
        ],
        "Exec's three constructor signatures moved since the pre-migration macro"
    );
    assert_eq!(
        exec.type_defs.to_vec(),
        vec![
            "data ExecError = ExecSpawn Text | ExecBadDir Text | ExecTimeout Text | ExecOutput Text | ExecWait Text deriving (Show, Eq)\ninstance ToJSON ExecError where\n  toJSON e = case e of\n    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n    ExecTimeout detail -> object [\"tag\" .= (\"ExecTimeout\" :: Text), \"detail\" .= detail]\n    ExecOutput detail -> object [\"tag\" .= (\"ExecOutput\" :: Text), \"detail\" .= detail]\n    ExecWait detail -> object [\"tag\" .= (\"ExecWait\" :: Text), \"detail\" .= detail]\n",
        ],
        "Exec's ExecError type_defs entry moved since the pre-migration macro"
    );
    assert_eq!(
        exec.extra_imports.to_vec(),
        vec![
            "import qualified Tidepool.Shell as Shell",
            "import Tidepool.Shell (sh)",
            "import qualified Tidepool.Cargo as Cargo",
        ],
        "Exec's three extra_imports lines drifted from effect_defs.rs's extra_imports_for!(Exec)"
    );

    let console = tidepool_mcp::console_decl();
    assert!(
        console
            .constructors
            .contains(&"Print :: Text -> Console ()"),
        "Console's text-printing contract changed"
    );
}

// ---------------------------------------------------------------------------
// Import-gating pin: the generated eval-module text (both the
// statement/evaluation module's `build_preamble` and the declaration module's
// `session_decl_module_env`) is a content-addressed compile-cache key
// (`ensure_effects_module`) — a single changed byte invalidates every
// cached compile for every user. Byte-exact golden files over the FULL
// generated text (pragmas + imports + paginate alias, not a hand-reassembled
// approximation) so a refactor of the import-gating machinery cannot
// accidentally keep the test green while silently changing the emitted
// bytes — this used to be four hand-copied literal import blocks
// (`preamble.rs`'s `import_gating_pin` module), which is exactly the kind of
// pin that goes stale silently when an earlier effect's `extra_imports`
// changes (e.g. `Entropy`'s `Tidepool.Random` landing in the standard row).
//
// Covers the four combinations the import-gating refactor (companion imports
// living on `EffectDecl::extra_imports`, see `effect_decls.rs`) must
// reproduce byte-for-byte: the standard row (all base effects, exercises the
// Exec+Git+Entropy companion imports), the actor-local row `[AskUser,
// ActorLocal]` (exercises the AskUser companion import alone), the agent-session row
// `[AgentSession, AskUser]` (AskUser again, different row shape), and the
// empty row (no companion imports, no `paginateResult` alias).
// ---------------------------------------------------------------------------

fn env_text(env: &tidepool_runtime::session::ModuleEnv) -> String {
    format!("{}\n{}", env.pragmas, env.imports.join("\n"))
}

/// Renders `build_preamble` and `session_decl_module_env`'s output for one
/// effect row into a single length-prefixed blob and diffs it against one
/// golden.
fn check_import_gating(golden_name: &str, effects: &[EffectDecl], committed: &str) {
    let mut out = String::new();
    write_blob(
        &mut out,
        "build_preamble",
        &tidepool_mcp::build_preamble(effects, false),
    );
    write_blob(
        &mut out,
        "session_decl_module_env",
        &env_text(&tidepool_mcp::session_decl_module_env(effects, false)),
    );
    assert_matches_golden(&format!("import_gating.{golden_name}.txt"), &out, committed);
}

#[test]
fn import_gating_standard_row_golden_matches_committed_file() {
    check_import_gating(
        "standard_row",
        &tidepool_mcp::standard_decls(),
        include_str!("goldens/protocol/import_gating.standard_row.txt"),
    );
}

#[test]
fn import_gating_actor_local_row_golden_matches_committed_file() {
    let effects = vec![
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::actor_local_decl(),
    ];
    check_import_gating(
        "actor_local_row",
        &effects,
        include_str!("goldens/protocol/import_gating.actor_local_row.txt"),
    );
}

#[test]
fn import_gating_agent_session_row_golden_matches_committed_file() {
    let effects = vec![
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::askuser_decl(),
    ];
    check_import_gating(
        "agent_session_row",
        &effects,
        include_str!("goldens/protocol/import_gating.agent_session_row.txt"),
    );
}

#[test]
fn import_gating_no_effects_row_golden_matches_committed_file() {
    check_import_gating(
        "empty_row",
        &[],
        include_str!("goldens/protocol/import_gating.empty_row.txt"),
    );
}

/// [`tidepool_mcp::PaginateMode::Passthrough`] (what `tidepool-repl` uses)
/// keeps pagination pure relative to
/// [`tidepool_mcp::PaginateMode::Truncate`] — imports stay identical, and
/// with no effects neither mode emits an alias at all. Not golden-backed: it
/// asserts a structural relationship between two LIVE outputs, not a
/// hand-kept literal, so there is nothing to migrate.
#[test]
fn passthrough_mode_removes_pagination_membership_constraints() {
    let decls = tidepool_mcp::standard_decls();
    let truncate = tidepool_mcp::build_preamble_non_interactive(&decls, false);
    let passthrough = tidepool_mcp::build_preamble_non_interactive_mode(
        &decls,
        false,
        tidepool_mcp::PaginateMode::Passthrough,
    );
    assert_eq!(
        passthrough,
        truncate
            .replacen("Members '[Console, KV] effs => ", "", 1,)
            .replacen(
                "paginateResult = paginateTrunc\n",
                "paginateResult _ v = pure v\n",
                1,
            ),
        "PaginateMode::Passthrough changes the alias body and removes its \
         unused membership constraints"
    );

    let truncate_empty = tidepool_mcp::build_preamble_non_interactive(&[], false);
    assert!(!truncate_empty.contains("paginateResult"));
    let passthrough_empty = tidepool_mcp::build_preamble_non_interactive_mode(
        &[],
        false,
        tidepool_mcp::PaginateMode::Passthrough,
    );
    assert_eq!(passthrough_empty, truncate_empty);
}
