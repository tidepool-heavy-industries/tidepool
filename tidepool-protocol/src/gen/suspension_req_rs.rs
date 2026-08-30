//! Generator: decode-only suspension request enums, emitted into the crate
//! that owns each effect's orchestration (see [`path`]'s `crate_dir`).
//!
//! `tidepool-harness`'s turn engine never DISPATCHES these effects — a
//! suspending effect (`Fork`, `Finalize`, `AskUser`, `RunLLMTurn`, `ReadState`,
//! `Subagent`, `Green`, `Console`'s `Print`, plus the four already-
//! migrated outer effects reused from `tidepool-handlers`) is classified and
//! handed to the driver's OWN orchestration, never routed through an
//! `EffectHandler`. So this generator emits only the middle third of
//! [`super::handler_rs`]'s output — the `#[derive(FromCore)] pub enum <Req>`
//! request enum, one variant per GADT constructor named exactly as in
//! Haskell — and none of the error ADT / `DescribeEffect` / `EffectHandler`
//! dispatch glue, which presuppose a handler these effects don't have.
//!
//! `Ask` is emitted into `tidepool-runtime` because
//! `tidepool-runtime::session::engine::
//! extract_ask_request` (the decode both `tidepool-repl` and the harness's
//! own `Ask` roster member need) sits BELOW `tidepool-harness` in the crate
//! graph, so the harness cannot be the one place this type lives without
//! `tidepool-runtime` either depending upward (impossible) or hand-carrying a
//! second, kept-in-sync copy (forbidden by the root `CLAUDE.md`'s
//! "one mechanism, one home" rule). Generating it once, into the lower
//! crate, and having the harness's `RosterRequest::Ask` reuse THAT copy is
//! the same pattern already used for the four outer effects reused from
//! `tidepool-handlers` — a shared type consumed by more than one crate lives
//! in whichever crate is lowest in the graph among its consumers.
//!
//! The effects this generator serves are declared in
//! [`crate::effects::suspension_roster`] for the transitional harness effects;
//! callers may also project a separately owned effect such as `Actor` into
//! its runtime crate. This generator owns only decode/typing, never the
//! orchestration itself.

use super::{header, index_body, module_name, GeneratedFile};
use crate::schema::Effect;

/// Where this effect's decode-only request enum lives, relative to the
/// workspace root. `crate_dir` is the target crate's directory name
/// (for example `"tidepool-harness"`, `"tidepool-runtime"`, or
/// `"tidepool-actor"`).
#[must_use]
pub fn path(e: &Effect, crate_dir: &str) -> String {
    format!("{crate_dir}/src/generated/{}.rs", module_name(e))
}

/// The `mod`-index for the generated decode modules in `crate_dir` —
/// deliberately NOT flattened: a consumer names each request type through
/// its own module (`generated::fork::ForkReq`) rather than a single glob, so
/// two effects can never contribute an ambiguously-named `Req` type to one
/// scope (unlike the decl-side index, which flattens because every name
/// there is already unique by Haskell convention).
#[must_use]
pub fn module_index(effects: &[Effect], crate_dir: &str) -> GeneratedFile {
    let modules: Vec<String> = effects.iter().map(module_name).collect();
    GeneratedFile {
        path: format!("{crate_dir}/src/generated/mod.rs"),
        contents: index_body("Generated suspension decode request types", &modules, false),
    }
}

/// The whole generated decode module for one effect, in `crate_dir`.
#[must_use]
pub fn file(e: &Effect, crate_dir: &str) -> GeneratedFile {
    GeneratedFile {
        path: path(e, crate_dir),
        contents: body(e),
    }
}

fn body(e: &Effect) -> String {
    let mut out = header("//! ", &format!("`{}` suspension request type", e.name));
    out.push('\n');
    out.push_str("use tidepool_bridge_derive::FromCore;\n\n");

    out.push_str(&format!(
        "/// One variant per `{}` GADT constructor, named EXACTLY as in Haskell.\n\
         /// Decode-only: this effect suspends to the consuming crate's own\n\
         /// orchestration rather than an `EffectHandler`, so there is no dispatch\n\
         /// glue here — see this module's crate-level generator doc. A field's\n\
         /// only job is making the `FromCore` name+arity match correct; the\n\
         /// consumer's own roster composition decides which fields (if any)\n\
         /// it goes on to read, so an all-recognition, no-field-read effect is\n\
         /// expected here, not a bug.\n",
        e.name
    ));
    out.push_str("#[derive(FromCore)]\n");
    // Every variant is named EXACTLY as its Haskell constructor (this
    // module's whole point), so a shared verb-family prefix (`Async*`,
    // `Subagent*`) is the CORRECT spelling, not a naming smell — clippy's
    // enum_variant_names lint disagrees, so it is silenced deliberately here
    // rather than by renaming variants away from their wire truth.
    out.push_str("#[allow(dead_code, clippy::enum_variant_names)]\n");
    out.push_str(&format!("pub enum {} {{\n", e.req_enum));
    for v in &e.verbs {
        let tys: Vec<String> = v
            .args
            .iter()
            .map(|a| {
                a.rust
                    .rust_type(&a.ty, &format!("{}::{}::{}", e.name, v.ctor, a.name))
            })
            .collect();
        out.push_str(&super::render_variant(v.ctor, &tys));
    }
    out.push_str("}\n");
    out
}
