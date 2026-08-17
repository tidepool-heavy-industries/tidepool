//! Generator: the Rust WIRE types, emitted into `tidepool-bridge-effects`.
//!
//! Replaces the hand-written `Wt*`/`Ev*`/`Ag*` blocks and — the point of the
//! whole lane — the comment above them:
//!
//! > Field ORDER in these structs is the wire contract and must match those
//! > `type_defs` decls positionally.
//!
//! The struct emitted here and the Haskell `data` declaration emitted by
//! `decl_rs` traverse the SAME `Vec<RecordField>`. The invariant is not better
//! guarded; it is untrue-by-construction.
//!
//! Two rules this emitter encodes, both promoted from convention in the
//! hand-written block:
//!
//! - `#[core(name = …)]` goes on a STRUCT whose Rust name differs from its
//!   Haskell name, and NEVER on an enum. An enum's data constructors ARE its
//!   variant names, so the bridge never looks the type name up.
//! - Every [`crate::types::TypeShape::Identity`] gets a FALLIBLE boundary
//!   constructor. PRD 22: decode once at the edge, typed everywhere after.

use super::{header, index_body, module_name as effect_module_name, GeneratedFile};
use crate::schema::Effect;

/// Where this effect's wire module lives, relative to the workspace root.
#[must_use]
pub fn path(e: &Effect) -> String {
    format!(
        "tidepool-bridge-effects/src/generated/{}.rs",
        effect_module_name(e)
    )
}

/// Does this effect have any wire types to emit?
#[must_use]
pub fn has_wire_types(e: &Effect) -> bool {
    !e.type_defs.is_empty()
}

/// The `mod`-index for the generated wire modules.
///
/// Flattened (`pub use <eff>::*`), because the hand-written wire types sit at
/// the crate root today and every consumer spells them
/// `tidepool_bridge_effects::WtWorktreeId`. A generated type that moved would be
/// a rename across every consuming crate for no proof value.
#[must_use]
pub fn module_index(effects: &[Effect]) -> GeneratedFile {
    let modules: Vec<String> = effects
        .iter()
        .filter(|e| has_wire_types(e))
        .map(effect_module_name)
        .collect();
    GeneratedFile {
        path: "tidepool-bridge-effects/src/generated/mod.rs".to_string(),
        contents: index_body("Generated effect wire types", &modules, true),
    }
}

/// The whole generated wire module for one effect.
#[must_use]
pub fn file(e: &Effect) -> GeneratedFile {
    GeneratedFile {
        path: path(e),
        contents: body(e),
    }
}

fn body(e: &Effect) -> String {
    let mut out = header("//! ", &format!("`{}` wire types", e.name));
    out.push('\n');
    out
}
