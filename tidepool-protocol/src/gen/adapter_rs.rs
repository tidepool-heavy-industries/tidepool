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

use std::collections::{BTreeMap, BTreeSet};

use super::{header, module_name as effect_module_name, GeneratedFile};
use crate::schema::{AdapterKind, Effect, TypeShape};

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

/// Which direction of a domain↔wire conversion is being emitted.
#[derive(Clone, Copy)]
enum Direction {
    IntoWire,
    FromWire,
}

impl Direction {
    /// The function-name suffix for this direction.
    fn suffix(self) -> &'static str {
        match self {
            Direction::IntoWire => "_to_wire",
            Direction::FromWire => "_from_wire",
        }
    }

    /// The label recorded next to a `HandWritten` reason.
    fn label(self) -> &'static str {
        match self {
            Direction::IntoWire => "into_wire",
            Direction::FromWire => "from_wire",
        }
    }
}

/// snake_case of a Haskell type name (`WorktreeId` -> `worktree_id`) — the
/// same rule [`super::module_name`] applies to an effect name, generalized to
/// any `TypeDef::name`. This is both the function-name prefix and the
/// parameter name, so naming is mechanical rather than a per-type mnemonic.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Record that the short name `short` is imported from `owner`.
///
/// # Panics
/// Panics when `short` was already claimed by a DIFFERENT `owner` — two
/// schema types cannot share a short import name, and papering over the
/// collision would silently import the wrong one at a call site.
fn claim(
    claimed: &mut BTreeMap<&'static str, &'static str>,
    short: &'static str,
    owner: &'static str,
) {
    if let Some(prev) = claimed.insert(short, owner) {
        assert_eq!(
            prev, owner,
            "adapter_rs: generated import `{short}` would come from both `{prev}` and \
             `{owner}` — rename one of the colliding schema types"
        );
    }
}

/// Render `use path::{a, b, …};`, wrapped onto its own multi-line block only
/// when the single-line form would not fit rustfmt's default width.
fn render_use(path: &str, idents: &BTreeSet<&'static str>) -> String {
    let items: Vec<&str> = idents.iter().copied().collect();
    let joined = items.join(", ");
    let single = format!("use {path}::{{{joined}}};\n");
    if single.trim_end().chars().count() <= 100 {
        single
    } else {
        format!("use {path}::{{\n    {joined},\n}};\n")
    }
}

fn body(e: &Effect) -> String {
    let mut out = header("//! ", &format!("`{}` domain↔wire adapters", e.name));
    out.push('\n');

    let mut wire_imports: BTreeSet<&'static str> = BTreeSet::new();
    let mut domain_imports: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    let mut claimed: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    let mut sections: Vec<String> = Vec::new();

    for t in &e.type_defs {
        let Some(dm) = &t.domain else { continue };
        let wire_name = t.wire_name();
        let domain_ident = dm.domain_ident();
        #[allow(
            clippy::expect_used,
            reason = "DomainMap::domain_path is crate-qualified"
        )]
        let domain_module = dm
            .domain_path
            .rsplit_once("::")
            .expect("DomainMap::domain_path is crate-qualified")
            .0;

        for (direction, kind) in [
            (Direction::IntoWire, &dm.into_wire),
            (Direction::FromWire, &dm.from_wire),
        ] {
            let Some(kind) = kind else { continue };
            match kind {
                AdapterKind::HandWritten(reason) => {
                    sections.push(format!(
                        "// {}::{} — HAND-WRITTEN, not generated: {reason}",
                        t.name,
                        direction.label(),
                    ));
                }
                AdapterKind::IdentityRaw { as_str, from_raw } => {
                    let TypeShape::Identity { rust_field, .. } = &t.shape else {
                        panic!("{}: an IdentityRaw adapter needs an Identity shape", t.name);
                    };
                    claim(&mut claimed, wire_name, "tidepool_bridge_effects");
                    claim(&mut claimed, domain_ident, domain_module);
                    wire_imports.insert(wire_name);
                    domain_imports
                        .entry(domain_module)
                        .or_default()
                        .insert(domain_ident);

                    let fn_name = format!("{}{}", snake_case(t.name), direction.suffix());
                    let param = snake_case(t.name);
                    sections.push(match direction {
                        Direction::IntoWire => format!(
                            "pub(crate) fn {fn_name}({param}: &{domain_ident}) -> {wire_name} {{\n    \
                             {wire_name} {{\n        {rust_field}: {param}.{as_str}().to_string(),\n    \
                             }}\n}}"
                        ),
                        Direction::FromWire => format!(
                            "pub(crate) fn {fn_name}({param}: &{wire_name}) -> {domain_ident} {{\n    \
                             {domain_ident}::{from_raw}({param}.{rust_field}.clone())\n}}"
                        ),
                    });
                }
                AdapterKind::VariantMap(pairs) => {
                    claim(&mut claimed, wire_name, "tidepool_bridge_effects");
                    claim(&mut claimed, domain_ident, domain_module);
                    wire_imports.insert(wire_name);
                    domain_imports
                        .entry(domain_module)
                        .or_default()
                        .insert(domain_ident);

                    let fn_name = format!("{}{}", snake_case(t.name), direction.suffix());
                    let param = snake_case(t.name);
                    let mut arms = String::new();
                    for (dvar, wvar) in pairs.iter() {
                        let arm = match direction {
                            Direction::IntoWire => {
                                format!("        {domain_ident}::{dvar} => {wire_name}::{wvar},\n")
                            }
                            Direction::FromWire => {
                                format!("        {wire_name}::{wvar} => {domain_ident}::{dvar},\n")
                            }
                        };
                        arms.push_str(&arm);
                    }
                    let (param_ty, ret_ty) = match direction {
                        Direction::IntoWire => (domain_ident, wire_name),
                        Direction::FromWire => (wire_name, domain_ident),
                    };
                    sections.push(format!(
                        "pub(crate) fn {fn_name}({param}: {param_ty}) -> {ret_ty} {{\n    \
                         match {param} {{\n{arms}    }}\n}}"
                    ));
                }
            }
        }
    }

    let mut imports = String::new();
    if !wire_imports.is_empty() {
        imports.push_str(&render_use("tidepool_bridge_effects", &wire_imports));
    }
    for (module, idents) in &domain_imports {
        imports.push_str(&render_use(module, idents));
    }
    if !imports.is_empty() {
        sections.insert(0, imports.trim_end().to_string());
    }

    out.push_str(&sections.join("\n\n"));
    out.push('\n');
    out
}
