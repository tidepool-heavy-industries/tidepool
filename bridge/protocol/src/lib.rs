//! The Tidepool effect protocol, as data.
//!
//! One schema describing effects, verbs, records, errors, field names and
//! types — plus the generators that project it into the crates that consume it.
//! NOT a network IDL, and no runtime component: nothing here is linked into the
//! server.
//!
//! # Why this crate exists
//!
//! The effect contract used to be single-sourced per SLICE rather than as a
//! protocol: one verb's truth was spread across up to four hand-maintained
//! registries — the macro DSL, the wire structs whose field ORDER was
//! maintained by comment, the extractor's per-verb tables, and the harness's
//! constructor-NAME classification lists. A verb added to one and missed in
//! another did not fail; it MISROUTED. This crate is where a verb's truth lives
//! instead.
//!
//! # The two rules that keep it honest
//!
//! **It is a leaf.** Zero dependencies, `std` only, never on another tidepool
//! crate. The crates it describes consume its OUTPUT, so a dependency in the
//! other direction would make the schema unusable from the low crates.
//!
//! **No raw escape hatches.** There is no "arbitrary Haskell source" slot and
//! no "arbitrary Rust body" slot, because that is precisely how a schema stops
//! being authoritative. Helper shapes are parameterized, reviewed patterns
//! ([`schema::HelperBody`]); types come from a closed language ([`hs::HsType`]).
//! Anything that cannot be expressed either becomes a deliberate schema feature
//! or stays hand-written OUTSIDE the contract — it is never smuggled in as a
//! string.
//!
//! # Migration status
//!
//! Effects move here one at a time, each proven byte-compatible before its
//! hand-written copy is deleted. [`effects::all`] is the migrated set;
//! everything else still lives in `bridge/mcp/src/effect_defs.rs`.

pub mod effects;
pub mod gen;
pub mod hs;
pub mod schema;
pub mod types;

pub use gen::{all_files, GeneratedFile};
pub use hs::HsType;
pub use schema::Effect;

/// Every generated file for every migrated effect, in a stable order.
///
/// # Panics
/// Panics when a migrated effect fails [`schema::Effect::validate`]. A
/// malformed schema must never produce output — the failure belongs at
/// generation time, which is the whole point of the annotations being required
/// fields.
#[must_use]
pub fn generated_files() -> Vec<GeneratedFile> {
    let effects = effects::all();
    for e in &effects {
        if let Err(problems) = e.validate() {
            panic!("schema is invalid:\n  {}", problems.join("\n  "));
        }
    }
    all_files(&effects)
}

/// The `Ask` member of the suspension-decode roster, generated into
/// `tidepool-runtime` — used by the runtime's decoder and
/// `tidepool-runtime::session::engine::extract_ask_request` (consumed by
/// the one-shot MCP eval server). See [`gen::suspension_req_rs`]'s doc.
///
/// # Panics
/// Panics when the `Ask` effect fails [`schema::Effect::validate`], same
/// discipline as [`generated_files`].
#[must_use]
pub fn runtime_generated_files() -> Vec<GeneratedFile> {
    let effects = vec![effects::ask::ask()];
    for e in &effects {
        if let Err(problems) = e.validate() {
            panic!("schema is invalid:\n  {}", problems.join("\n  "));
        }
    }
    let mut out: Vec<GeneratedFile> = effects
        .iter()
        .map(|e| gen::suspension_req_rs::file(e, "tidepool/runtime"))
        .collect();
    out.push(gen::suspension_req_rs::module_index(
        &effects,
        "tidepool/runtime",
    ));
    out
}

/// The actor-runtime request decoder, generated into `exomonad-actor` so the
/// actor kernel owns its orchestration boundary rather than teaching the
/// transitional harness classifier about actor lifecycle.
#[must_use]
pub fn actor_generated_files() -> Vec<GeneratedFile> {
    let effects: Vec<_> = vec![
        effects::actor::actor(),
        effects::actor_context::actor_context(),
        effects::introspection::introspection(),
        effects::lookup::lookup(),
        effects::actor_kernel::actor_kernel(),
        effects::actor_local::actor_local(),
        effects::sleep::sleep(),
        effects::agent_control::agent_control(),
        effects::commands::commands(),
        effects::console::console(),
        effects::notifications::notifications(),
        effects::jev::jev(),
        effects::agent_inspection::agent_inspection(),
        effects::agent_launch::agent_launch(),
        effects::forks::forks(),
        effects::agent_tools::agent_tools(),
        effects::agent_session::agent_session(),
        effects::reflect::reflect(),
    ]
    .into_iter()
    .filter(|effect| !effect.verbs.is_empty())
    .collect();
    for e in &effects {
        if let Err(problems) = e.validate() {
            panic!("schema is invalid:\n  {}", problems.join("\n  "));
        }
    }
    let mut out: Vec<GeneratedFile> = effects
        .iter()
        .map(|e| gen::suspension_req_rs::file(e, "exomonad/actor"))
        .collect();
    out.push(gen::suspension_req_rs::module_index(
        &effects,
        "exomonad/actor",
    ));
    out
}

/// Decode-only recipe operations are consumed by the Exomonad composition root.
#[must_use]
pub fn recipe_generated_files() -> Vec<GeneratedFile> {
    let effects = vec![effects::recipe_check::recipe_check()];
    let mut files = effects
        .iter()
        .map(|effect| gen::suspension_req_rs::file(effect, "bridge/facade"))
        .collect::<Vec<_>>();
    files.push(gen::suspension_req_rs::module_index(
        &effects,
        "bridge/facade",
    ));
    files
}
