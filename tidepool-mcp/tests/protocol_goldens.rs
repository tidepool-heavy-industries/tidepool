//! Class A golden baseline: pins, byte-for-byte, the
//! effect-contract artifacts that cross the Rust/Haskell boundary. These
//! goldens are the non-regression evidence that an effect-at-a-time schema
//! migration is safe — when lane N later flips effect N, re-asserting these
//! goldens proves every OTHER effect's artifacts did not move. Captured NOW,
//! from unmodified trunk, before anything is flipped.
//!
//! The bar is exact byte equality, not `contains`: the generated
//! `Tidepool.Effects` module source is content-addressed as a compile-cache
//! key (`ensure_effects_module` in `tidepool-mcp/src/lib.rs`) — a single
//! changed byte invalidates every cached compile for every user.
//!
//! Three layers, mirroring `tidepool-handlers/tests/bridged_records.rs` (the
//! pattern this file copies):
//!   1. whole-file byte compare against a committed golden (the three
//!      `*_golden_matches_committed_file` tests below),
//!   2. an INDEPENDENT hardcoded pin on a handful of items
//!      (`hardcoded_pins_survive_a_blind_regen`), so a blindly-run regen
//!      cannot launder a change into the goldens,
//!   3. the `TIDEPOOL_REGEN_PROTOCOL_GOLDENS` env-var regen escape hatch.

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
            "Subagent",
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
            "ActorKernel",
            "ActorLocal",
            "ActorMcp",
            "AgentSession",
        ]
    );
}

// ---------------------------------------------------------------------------
// The dumper — lossless, unambiguous text rendering of one EffectDecl
// ---------------------------------------------------------------------------
//
// Every field is framed as a header line naming its exact byte length,
// followed by exactly that many bytes of raw content, followed by a footer
// line. This is netstring-style length-prefixed framing: decoding a field
// never depends on scanning its content for a delimiter (which a multi-line
// `type_defs`/`helpers` entry — itself containing blank lines, "--"
// sequences, anything — could otherwise spoof), only on the declared byte
// count recorded right before it. Fields are emitted in a FIXED order under
// FIXED labels, so the whole rendering is an injective function of the
// tuple of field values: two EffectDecls differing in any single field
// (including WHICH multi-line entry differs inside `type_defs`/`helpers`,
// or how many blank lines one contains) can never render to the same text.

fn write_blob(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("-- {label} ({} bytes) --\n", value.len()));
    out.push_str(value);
    if !value.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("-- end {label} --\n\n"));
}

fn write_list(out: &mut String, label: &str, items: &[&str]) {
    out.push_str(&format!("-- {label} ({} items) --\n", items.len()));
    for (i, item) in items.iter().enumerate() {
        write_blob(out, &format!("{label}[{i}]"), item);
    }
    out.push_str(&format!("-- end {label} --\n\n"));
}

/// Dump every field of one `EffectDecl` to text, one labelled section per
/// field, in declaration order.
fn render_effect_decl(decl: &EffectDecl) -> String {
    let mut out = String::new();
    write_blob(&mut out, "type_name", decl.type_name);
    write_blob(&mut out, "description", decl.description);
    match decl.prompt_card {
        Some(pc) => {
            out.push_str("-- prompt_card: Some --\n");
            write_blob(&mut out, "prompt_card.0", pc);
        }
        None => out.push_str("-- prompt_card: None --\n\n"),
    }
    write_list(&mut out, "constructors", decl.constructors);
    write_list(&mut out, "type_defs", decl.type_defs);
    write_list(&mut out, "extra_imports", decl.extra_imports);
    write_list(&mut out, "helpers", decl.helpers);
    write_list(&mut out, "type_params", decl.type_params);
    write_list(&mut out, "default_row_args", decl.default_row_args);
    out.push_str(&format!(
        "-- helpers_row_polymorphic --\n{}\n-- end helpers_row_polymorphic --\n\n",
        decl.helpers_row_polymorphic
    ));
    out
}

/// Render every pinned decl, concatenated in list order with a clear
/// per-effect header.
fn render_pinned_decls(decls: &[EffectDecl]) -> String {
    let mut out = String::new();
    for decl in decls {
        out.push_str(&format!(
            "=============================== EFFECT: {} ===============================\n\n",
            decl.type_name
        ));
        out.push_str(&render_effect_decl(decl));
    }
    out
}

// ---------------------------------------------------------------------------
// Golden plumbing — mirrors bridged_records.rs
// ---------------------------------------------------------------------------

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/protocol")
        .join(name)
}

fn assert_matches_golden(name: &str, generated: &str) {
    let path = golden_path(name);
    let regen = std::env::var_os("TIDEPOOL_REGEN_PROTOCOL_GOLDENS").is_some();
    let current = std::fs::read_to_string(&path).ok();
    if regen || current.is_none() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, generated).unwrap();
        if current.as_deref() != Some(generated) && !regen {
            panic!("wrote missing/updated {} — re-run the test", path.display());
        }
        return;
    }
    assert_eq!(
        current.unwrap(),
        generated,
        "committed {} is stale vs the generated artifact — regenerate with \
         TIDEPOOL_REGEN_PROTOCOL_GOLDENS=1 cargo test -p tidepool-mcp --test protocol_goldens",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// Golden 1 — every pinned EffectDecl, dumped field-by-field
// ---------------------------------------------------------------------------

#[test]
fn effect_decls_golden_matches_committed_file() {
    let generated = render_pinned_decls(&pinned_decls());
    assert_matches_golden("effect_decls.txt", &generated);
}

// ---------------------------------------------------------------------------
// Golden 2 — the generated Tidepool.Effects.Core + Tidepool.Effects (shim)
// modules for the standard row. These are the compile-cache-key artifacts:
// captured verbatim, no normalization, no trailing-whitespace trimming.
// ---------------------------------------------------------------------------

#[test]
fn effects_core_module_universal_golden_matches_committed_file() {
    let generated = tidepool_mcp::effects_core_module_source();
    assert_matches_golden("effects_core_module.standard.hs", &generated);
}

#[test]
fn effects_shim_module_standard_golden_matches_committed_file() {
    let generated = tidepool_mcp::effects_shim_module_source(
        &tidepool_mcp::standard_decls(),
        &tidepool_mcp::RowArgs::default(),
    );
    assert_matches_golden("effects_shim_module.standard.hs", &generated);
}

// ---------------------------------------------------------------------------
// Golden 3 — the derived tool-description effects index over the standard row.
// ---------------------------------------------------------------------------

#[test]
fn tool_description_effects_index_golden_matches_committed_file() {
    let generated = tidepool_mcp::describe_effects_index(&tidepool_mcp::standard_decls());
    assert_matches_golden("tool_description.effects_index.txt", &generated);
}

// ---------------------------------------------------------------------------
// Layer 2 — an INDEPENDENT hardcoded pin, modelled on
// `tidepool-mcp/src/preamble.rs`'s `import_gating_pin` module doc: these are
// literal strings transcribed BY HAND from the hand-written definitions, NOT
// read from any golden file and NOT produced by `render_effect_decl` above.
// Their whole job is to stay true even when someone regenerates
// `effect_decls.txt` blindly (`TIDEPOOL_REGEN_PROTOCOL_GOLDENS=1` over a
// change that also altered the underlying definition): a dumper bug or a
// silent definition drift can launder itself into a regenerated golden, but it
// cannot launder itself into a string written independently, by hand, from the
// source of truth.
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
            "data ExecError = ExecSpawn Text | ExecBadDir Text | ExecTimeout Text deriving (Show, Eq)\ninstance ToJSON ExecError where\n  toJSON e = case e of\n    ExecSpawn detail -> object [\"tag\" .= (\"ExecSpawn\" :: Text), \"detail\" .= detail]\n    ExecBadDir detail -> object [\"tag\" .= (\"ExecBadDir\" :: Text), \"detail\" .= detail]\n    ExecTimeout detail -> object [\"tag\" .= (\"ExecTimeout\" :: Text), \"detail\" .= detail]\n",
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
    assert_eq!(
        console.constructors.to_vec(),
        vec!["Print :: Text -> Console ()"],
        "Console's single constructor signature drifted from effect_defs.rs's console_effect_def!"
    );
}

// ---------------------------------------------------------------------------
// Golden 4 — import-gating pin: the generated eval-module text (both the
// stmt/eval plane's `build_preamble` and the decl plane's
// `session_decl_module_env`) is a content-addressed compile-cache key
// (`ensure_effects_module_at`) — a single changed byte invalidates every
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
// Exec+Git+Entropy companion imports), the answerer row `[AskUser,
// Finalize]` (exercises the AskUser companion import alone), the outer row
// `[RunLLMTurn, AskUser]` (AskUser again, different row shape), and the
// empty row (no companion imports, no `paginateResult` alias).
// ---------------------------------------------------------------------------

fn env_text(env: &tidepool_runtime::session::ModuleEnv) -> String {
    format!("{}\n{}", env.pragmas, env.imports.join("\n"))
}

/// Renders `build_preamble` and `session_decl_module_env`'s output for one
/// effect row into a single labelled blob (netstring-style framing, same
/// idiom as [`render_effect_decl`] above) and diffs it against one golden.
fn check_import_gating(golden_name: &str, effects: &[EffectDecl]) {
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
    assert_matches_golden(&format!("import_gating.{golden_name}.txt"), &out);
}

#[test]
fn import_gating_standard_row_golden_matches_committed_file() {
    check_import_gating("standard_row", &tidepool_mcp::standard_decls());
}

#[test]
fn import_gating_answerer_row_golden_matches_committed_file() {
    let effects = vec![tidepool_mcp::askuser_decl(), tidepool_mcp::finalize_decl()];
    check_import_gating("answerer_row", &effects);
}

#[test]
fn import_gating_outer_row_golden_matches_committed_file() {
    let effects = vec![
        tidepool_mcp::runllmturn_decl(),
        tidepool_mcp::askuser_decl(),
    ];
    check_import_gating("outer_row", &effects);
}

#[test]
fn import_gating_no_effects_row_golden_matches_committed_file() {
    check_import_gating("empty_row", &[]);
}

/// [`tidepool_mcp::PaginateMode::Passthrough`] (what `tidepool-repl` uses)
/// swaps only the `paginateResult` alias BODY relative to
/// [`tidepool_mcp::PaginateMode::Truncate`] — imports stay identical, and
/// with no effects neither mode emits an alias at all. Not golden-backed: it
/// asserts a structural relationship between two LIVE outputs, not a
/// hand-kept literal, so there is nothing to migrate.
#[test]
fn passthrough_mode_swaps_only_the_paginate_alias_body() {
    let decls = tidepool_mcp::standard_decls();
    let truncate = tidepool_mcp::build_preamble_non_interactive(&decls, false);
    let passthrough = tidepool_mcp::build_preamble_non_interactive_mode(
        &decls,
        false,
        tidepool_mcp::PaginateMode::Passthrough,
    );
    assert_eq!(
        passthrough,
        truncate.replacen(
            "paginateResult = paginateTrunc\n",
            "paginateResult _ v = pure v\n",
            1,
        ),
        "PaginateMode::Passthrough must differ from Truncate only in the \
         paginateResult alias body"
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
