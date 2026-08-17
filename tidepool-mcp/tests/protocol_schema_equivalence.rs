//! PRD 22's acceptance bar, mechanically: the `EffectDecl` a migrated effect
//! produces must be identical, field for field, to what the schema in
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
    assert_eq!(
        type_params.to_vec(),
        eff.type_params.to_vec(),
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
        vec!["Exec", "Journal"],
        "the migrated set changed — add the new effect's equivalence assertion above"
    );
}
