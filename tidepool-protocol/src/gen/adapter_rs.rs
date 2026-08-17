//! Generator: the MECHANICAL domain↔wire conversions, emitted into
//! `tidepool-handlers`.
//!
//! Only the mechanical half. The split is a schema field
//! ([`crate::types::AdapterKind`]), so the generated file itself records which
//! conversions carry a decision and why they stay hand-written — today that
//! distinction exists only in a reader's head.
//!
//! **Why here and not in `tidepool-bridge-effects`.** The conversion needs both
//! the wire type and the DOMAIN type, and `tidepool-bridge-effects` is a LOW
//! crate that must not gain a dependency on `tidepool-worktree` — that is the
//! property letting test mocks in low crates keep importing the wire types. The
//! schema carries a domain Rust PATH and the path resolves at the consuming
//! crate, exactly as `RustBinding::Path` already does.
//!
//! **Why free functions and not `From`/`TryFrom` impls.** Both types are foreign
//! to `tidepool-handlers`, so a trait impl there is an orphan-rule violation.
//! Free functions are also what the hand-written block uses, so the flip is a
//! deletion rather than a call-site rewrite.

use super::{header, module_name as effect_module_name, GeneratedFile};
use crate::schema::Effect;

/// The module name this effect's adapters live under.
#[must_use]
pub fn module_name(e: &Effect) -> String {
    format!("{}_adapters", effect_module_name(e))
}

/// Where this effect's adapters live, relative to the workspace root.
#[must_use]
pub fn path(e: &Effect) -> String {
    format!("tidepool-handlers/src/generated/{}.rs", module_name(e))
}

/// Does this effect declare any domain mapping at all?
#[must_use]
pub fn has_adapters(e: &Effect) -> bool {
    e.type_defs.iter().any(|t| t.domain.is_some())
}

/// The whole generated adapter module for one effect.
#[must_use]
pub fn file(e: &Effect) -> GeneratedFile {
    GeneratedFile {
        path: path(e),
        contents: body(e),
    }
}

fn body(e: &Effect) -> String {
    let mut out = header("//! ", &format!("`{}` domain↔wire adapters", e.name));
    out.push('\n');
    out
}
