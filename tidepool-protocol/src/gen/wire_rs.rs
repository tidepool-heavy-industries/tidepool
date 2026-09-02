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
//!   constructor. The rule: decode once at the edge, typed everywhere after.

use super::{header, index_body, module_name as effect_module_name, GeneratedFile};
use crate::hs::HsType;
use crate::schema::{
    Effect, IdentityPayload, TypeDef, TypeShape, Validation, VariantFields, WireDerive,
};

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

    // A `foreign_types` entry names a wire type another effect's OWN generated
    // module declares (Event's `Watch` etc. name Worktree's `WtWorktreeId`).
    // Every generated wire module lives in `tidepool-bridge-effects`, and the
    // crate root flattens every effect's module (`pub use generated::*;`), so
    // the foreign name is reachable at `crate::<Name>` regardless of which
    // sibling module actually declares it. rustfmt sorts `use` items
    // lexically, and `crate` sorts before `tidepool_bridge_derive`.
    if !e.foreign_types.is_empty() {
        let mut names: Vec<&str> = e.foreign_types.iter().map(|(_, wire)| *wire).collect();
        names.sort_unstable();
        names.dedup();
        out.push_str(&format!("use crate::{{{}}};\n", names.join(", ")));
    }

    let idents = used_bridge_derives(e);
    out.push_str(&format!(
        "use tidepool_bridge_derive::{{{}}};\n",
        idents.join(", ")
    ));
    out.push('\n');

    if let Some(err) = wire_error_enum(e) {
        out.push_str(&err);
        out.push('\n');
    }

    for t in &e.type_defs {
        emit_type_decl(e, t, &mut out);
        out.push('\n');
    }

    for t in &e.type_defs {
        if matches!(t.shape, TypeShape::Identity { .. }) {
            emit_identity_impl(t, &mut out);
            out.push('\n');
        }
    }

    // The loops above always leave one trailing blank line; trim it back to
    // the single newline every other generated file ends on.
    if out.ends_with("\n\n") {
        out.pop();
    }

    out
}

/// The `tidepool_bridge_derive` derives actually used across `e`'s
/// `type_defs`, sorted. `Clone`/`Debug`/… are compiler-builtin derives already
/// in the prelude and need no `use`; only the two bridge macros do.
fn used_bridge_derives(e: &Effect) -> Vec<&'static str> {
    let mut idents = Vec::new();
    if e.type_defs
        .iter()
        .any(|t| t.derives.has(WireDerive::FromCore))
    {
        idents.push("FromCore");
    }
    if e.type_defs
        .iter()
        .any(|t| t.derives.has(WireDerive::ToCore))
    {
        idents.push("ToCore");
    }
    idents.sort_unstable();
    idents
}

/// The Rust type a `RecordField`/`SumVariant` field's `HsType` renders as on
/// the wire. Closed over exactly the shapes the wire-record language uses
/// today; anything else is a generation-time failure, same spirit as
/// [`Effect::wire_rust_of`]'s panic.
fn rust_type(e: &Effect, ty: &HsType) -> String {
    match ty {
        HsType::Text => "String".to_string(),
        HsType::Int => "i64".to_string(),
        HsType::Bool => "bool".to_string(),
        // The vendored aeson JSON value, ret-only wherever it appears in a wire
        // record today (`RepositoryEvent::ObservedMessage`'s bare payload) — the
        // same `serde_json::Value` spelling `AgCyclePayload`/`AgAgentStep` use
        // for the same reason (no `FromCore` for it, so it never decodes).
        HsType::Value => "serde_json::Value".to_string(),
        HsType::List(inner) => format!("Vec<{}>", rust_type(e, inner)),
        HsType::Maybe(inner) => format!("Option<{}>", rust_type(e, inner)),
        // `[(GitOid, GitOid)]` (`HeadChangeKind::Rewritten`) is the only tuple
        // seen in a wire record today; a Haskell list-of-tuple is a Rust
        // `Vec<(..)>`, same as every other `HsType::List` — the tuple itself
        // renders as an ordinary Rust tuple.
        HsType::Tuple(tys) => {
            let inner: Vec<String> = tys.iter().map(|t| rust_type(e, t)).collect();
            format!("({})", inner.join(", "))
        }
        HsType::Named(n) => e.wire_rust_of(n).to_string(),
        other => panic!(
            "{}: wire_rs cannot render {other:?} as a wire record field type — \
             only Text/Int/Bool/Value/[T]/Maybe T/(T, ..)/Named(n) are representable here",
            e.name
        ),
    }
}

/// Does this effect need a `WireError` type, and if so which policies does it
/// need variants for? `None` when every `Identity` is infallible — a future
/// effect with no fallible identity gets no dead error type.
fn wire_error_enum(e: &Effect) -> Option<String> {
    let has_nonempty = e.type_defs.iter().any(|t| {
        matches!(
            &t.shape,
            TypeShape::Identity {
                validation: Validation::NonEmpty,
                ..
            }
        )
    });
    let has_segment = e.type_defs.iter().any(|t| {
        matches!(
            &t.shape,
            TypeShape::Identity {
                validation: Validation::Segment { .. },
                ..
            }
        )
    });
    if !has_nonempty && !has_segment {
        return None;
    }

    let mut out = String::new();
    out.push_str("/// Why a wire identity's boundary constructor refused a raw value.\n");
    out.push_str("#[derive(Clone, Debug, PartialEq, Eq)]\n");
    out.push_str("pub enum WireError {\n");
    if has_nonempty {
        out.push_str("    /// The raw value was empty.\n");
        out.push_str("    Empty {\n");
        out.push_str("        /// Which wire type refused it.\n");
        out.push_str("        wire_type: &'static str,\n");
        out.push_str("    },\n");
    }
    if has_segment {
        out.push_str(
            "    /// The raw value was not a safe single path segment: empty, too long,\n",
        );
        out.push_str("    /// or carrying a byte outside ascii-alphanumeric plus the type's\n");
        out.push_str("    /// allowed extras.\n");
        out.push_str("    InvalidSegment {\n");
        out.push_str("        /// Which wire type refused it.\n");
        out.push_str("        wire_type: &'static str,\n");
        out.push_str("    },\n");
    }
    out.push_str("}\n");
    Some(out)
}

/// One `TypeDef`'s doc comment, derive line, optional `#[core(name = …)]`, and
/// its struct/enum body.
fn emit_type_decl(e: &Effect, t: &TypeDef, out: &mut String) {
    for line in t.doc {
        out.push_str("/// ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&t.derives.render());
    out.push('\n');
    if t.needs_core_name() {
        out.push_str(&format!("#[core(name = \"{}\")]\n", t.name));
    }

    match &t.shape {
        TypeShape::Record { fields } => {
            out.push_str(&format!("pub struct {} {{\n", t.wire_name()));
            for f in fields {
                for line in f.doc {
                    out.push_str(&format!("    /// {line}\n"));
                }
                out.push_str(&format!(
                    "    pub {}: {},\n",
                    f.rust_name,
                    rust_type(e, &f.ty)
                ));
            }
            out.push_str("}\n");
        }
        TypeShape::Sum { variants } => {
            out.push_str(&format!("pub enum {} {{\n", t.wire_name()));
            for v in variants {
                for line in v.doc {
                    out.push_str(&format!("    /// {line}\n"));
                }
                match &v.fields {
                    VariantFields::Positional(fields) if fields.is_empty() => {
                        out.push_str(&format!("    {},\n", v.ctor));
                    }
                    VariantFields::Positional(fields) => {
                        let types: Vec<String> =
                            fields.iter().map(|field| rust_type(e, field)).collect();
                        out.push_str(&format!("    {}({}),\n", v.ctor, types.join(", ")));
                    }
                    VariantFields::Named(fields) => {
                        out.push_str(&format!("    {} {{\n", v.ctor));
                        for field in fields {
                            for line in field.doc {
                                out.push_str(&format!("        /// {line}\n"));
                            }
                            out.push_str(&format!(
                                "        {}: {},\n",
                                field.rust_name,
                                rust_type(e, &field.ty)
                            ));
                        }
                        out.push_str("    },\n");
                    }
                }
            }
            out.push_str("}\n");
        }
        TypeShape::Identity {
            payload,
            rust_field,
            ..
        } => {
            out.push_str(&format!("pub struct {} {{\n", t.wire_name()));
            out.push_str(&format!("    pub {}: {},\n", rust_field, payload.rust()));
            out.push_str("}\n");
        }
    }
}

/// The fallible (or, for `Validation::None`, infallible) boundary constructor
/// plus payload accessor for one `TypeShape::Identity`.
fn emit_identity_impl(t: &TypeDef, out: &mut String) {
    let TypeShape::Identity {
        payload,
        rust_field,
        validation,
        ..
    } = &t.shape
    else {
        return;
    };
    let wire_name = t.wire_name();

    out.push_str(&format!("impl {wire_name} {{\n"));
    match payload {
        IdentityPayload::Text => {
            match validation {
                Validation::None => {
                    out.push_str(
                        "    /// An untrusted raw value becomes a wire id here — infallibly:\n",
                    );
                    out.push_str("    /// this identity carries no string policy.\n");
                    out.push_str("    #[must_use]\n");
                    out.push_str(&format!(
                        "    pub fn new({rust_field}: impl Into<String>) -> Self {{\n"
                    ));
                    out.push_str(&format!(
                        "        Self {{ {rust_field}: {rust_field}.into() }}\n"
                    ));
                    out.push_str("    }\n");
                }
                Validation::NonEmpty => {
                    out.push_str(
                        "    /// The trust boundary: an untrusted raw value becomes a wire id here or\n",
                    );
                    out.push_str("    /// not at all.\n");
                    out.push_str("    ///\n");
                    out.push_str("    /// # Errors\n");
                    out.push_str("    /// [`WireError::Empty`] when the raw value is empty.\n");
                    out.push_str(&format!(
                        "    pub fn new({rust_field}: impl Into<String>) -> Result<Self, WireError> {{\n"
                    ));
                    out.push_str(&format!(
                        "        let {rust_field} = {rust_field}.into();\n"
                    ));
                    out.push_str(&format!("        if {rust_field}.is_empty() {{\n"));
                    out.push_str("            return Err(WireError::Empty {\n");
                    out.push_str(&format!("                wire_type: \"{wire_name}\",\n"));
                    out.push_str("            });\n");
                    out.push_str("        }\n");
                    out.push_str(&format!("        Ok(Self {{ {rust_field} }})\n"));
                    out.push_str("    }\n");
                }
                Validation::Segment {
                    max_len,
                    extra_allowed,
                } => {
                    let byte_checks: Vec<String> = extra_allowed
                        .chars()
                        .map(|c| format!("b == b'{c}'"))
                        .collect();
                    out.push_str(
                        "    /// The trust boundary: an untrusted raw value becomes a wire id here or\n",
                    );
                    out.push_str("    /// not at all.\n");
                    out.push_str("    ///\n");
                    out.push_str("    /// # Errors\n");
                    out.push_str(&format!(
                        "    /// [`WireError::InvalidSegment`] unless the raw value is a safe single\n\
                         \x20   /// path component: non-empty, at most {max_len} bytes, every byte\n\
                         \x20   /// ascii-alphanumeric or one of `{extra_allowed}`.\n"
                    ));
                    out.push_str(&format!(
                        "    pub fn new({rust_field}: impl Into<String>) -> Result<Self, WireError> {{\n"
                    ));
                    out.push_str(&format!(
                        "        let {rust_field} = {rust_field}.into();\n"
                    ));
                    out.push_str(&format!("        if {rust_field}.is_empty()\n"));
                    out.push_str(&format!("            || {rust_field}.len() > {max_len}\n"));
                    out.push_str(&format!("            || !{rust_field}\n"));
                    out.push_str("                .bytes()\n");
                    out.push_str(&format!(
                        "                .all(|b| b.is_ascii_alphanumeric() || {})\n",
                        byte_checks.join(" || ")
                    ));
                    out.push_str("        {\n");
                    out.push_str("            return Err(WireError::InvalidSegment {\n");
                    out.push_str(&format!("                wire_type: \"{wire_name}\",\n"));
                    out.push_str("            });\n");
                    out.push_str("        }\n");
                    out.push_str(&format!("        Ok(Self {{ {rust_field} }})\n"));
                    out.push_str("    }\n");
                }
            }
            out.push('\n');
            out.push_str("    /// The validated payload.\n");
            out.push_str("    #[must_use]\n");
            out.push_str("    pub fn as_str(&self) -> &str {\n");
            out.push_str(&format!("        &self.{rust_field}\n"));
            out.push_str("    }\n");
        }
        IdentityPayload::Int => {
            assert!(
                matches!(validation, Validation::None),
                "{wire_name}: an Int identity cannot carry a string validation policy"
            );
            out.push_str(
                "    /// An untrusted raw value becomes a wire id here — infallibly: an\n",
            );
            out.push_str("    /// integer identity carries no policy.\n");
            out.push_str("    #[must_use]\n");
            out.push_str(&format!("    pub fn new({rust_field}: i64) -> Self {{\n"));
            out.push_str(&format!("        Self {{ {rust_field} }}\n"));
            out.push_str("    }\n");
            out.push('\n');
            out.push_str("    /// The payload.\n");
            out.push_str("    #[must_use]\n");
            out.push_str("    pub fn as_i64(&self) -> i64 {\n");
            out.push_str(&format!("        self.{rust_field}\n"));
            out.push_str("    }\n");
        }
    }
    out.push_str("}\n");
}
