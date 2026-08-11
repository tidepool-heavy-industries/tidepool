//! Operator input shared by the `AskUser` effect decoder, self-harness driver,
//! and web UI.
//!
//! [`OperatorGate`] is synchronous: `present_form` blocks until submission and
//! `await_continue` blocks between loop iterations. The web implementation
//! parks a channel; [`StdinGate`] reads a line. The async driver isolates the
//! blocking call with `tokio::task::block_in_place`.
//!
//! [`FormShape`] is the form wire, mirroring Haskell's
//! `Tidepool.Form.Shape` (`FormShape`/`FieldShape`/`VariantShape`)
//! constructor for constructor. `askUser @T` emits a bare [`FormShape`]
//! through the `AskUserWith` suspension; [`crate::engine::classify_hole`]
//! decodes it directly for the in-process gate — [`FormShape`] IS the one
//! operator-presentation algebra, carried bare end to end.
//!
//! ## Shape JSON
//!
//! The Haskell encoder (`Tidepool.Form.Wire`) targets exactly what follows,
//! and `tidepool-runtime/tests/generic_form_wire.rs` asserts it against
//! these worked examples. The Rust derive below defines the encoding.
//!
//! A unit-payload variant (`FormShape::String`, …) serializes as a bare JSON
//! string of its snake_case name. A newtype variant
//! (`FormShape::Optional(Box<FormShape>)`) serializes as
//! `{"optional": <inner>}`. A struct variant (`FormShape::Product { .. }`)
//! serializes as `{"product": {<fields>}}`.
//!
//! ## Answers are ordinary JSON
//!
//! What the operator submits travels as a plain `serde_json::Value` shaped
//! for the answer type's own generic `FromJSON` decode (Haskell side):
//!
//! * record → JSON object of its fields: `{"host": "example.com", "port": 22}`;
//! * all-nullary sum (enum) → the chosen constructor as a bare string:
//!   `"Staging"`;
//! * payload sum → tagged object, fields alongside the tag:
//!   `{"tag": "Ssh", "host": "example.com", "port": 22}` (a nullary branch
//!   of a mixed sum is `{"tag": "LocalHost"}`);
//! * `Maybe` field → the value, or `null`/omitted for `Nothing`;
//! * unit form (`askUser @()`) → `null`;
//! * leaves → the corresponding JSON scalar.
//!
//! The collector that builds this JSON from the rendered controls is
//! `tidepool-web`'s submission path, guided by the same [`FormShape`].
use serde::{Deserialize, Serialize};

/// The seam the driver blocks on for operator input. Sync-blocking by design
/// (see module docs). A web GUI implements this by parking a channel
/// resolved from an HTTP handler; [`StdinGate`] keeps headless runs working.
pub trait OperatorGate: Send + Sync {
    /// Present `shape` to the operator and BLOCK until they submit. Returns
    /// the answer value ready for the Haskell decode. The transport is a
    /// full `Value` because valid answers include scalars and `null`, not
    /// only objects. A decode failure Haskell-side re-presents the form.
    fn present_form(&self, shape: &FormShape) -> serde_json::Value;

    /// BLOCK until the operator advances to the next loop iteration (the
    /// human button-click gate that replaces the stdin between-loops gate).
    fn await_continue(&self);
}

/// Headless default: `await_continue` reads a line from stdin (the current
/// between-loops behavior); `present_form` reads one JSON value per line, so
/// non-web/CLI drives and tests still work. Used as the
/// default when no web gate is configured.
#[derive(Debug, Default)]
pub struct StdinGate;

impl OperatorGate for StdinGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return serde_json::json!({});
        }
        // An unparseable line degrades to `{}`, which the Haskell decoder
        // rejects and re-presents.
        serde_json::from_str(line.trim()).unwrap_or_else(|_| serde_json::json!({}))
    }

    fn await_continue(&self) {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
}

// -------------------------------------------------------------------------
// Form shape — mirrors Tidepool.Form.Shape.
// -------------------------------------------------------------------------

/// A datatype's own name — the form's title, and the key a sum is
/// identified by. Mirrors `Tidepool.Form.Shape.TypeKey`.
pub type TypeKey = String;

/// A constructor's name — the stable key for one branch of a sum, and the
/// identity of a product node. Mirrors `Tidepool.Form.Shape.ConstructorKey`.
pub type ConstructorKey = String;

/// A field's key within one product node: the exact record selector name.
/// Positional fields are rejected by the Haskell form derivation. Mirrors
/// `Tidepool.Form.Shape.FieldKey`.
pub type FieldKey = String;

/// The structural description of a recursive form, derived from a type's
/// generic representation without ever seeing a value of that type. Mirrors
/// Haskell's `Tidepool.Form.Shape.FormShape` constructor for constructor.
/// See the module docs for the JSON encoding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FormShape {
    /// A single-line string leaf.
    String,
    /// A bounded integral leaf.
    Int,
    /// A numeric leaf.
    Number,
    /// A boolean leaf.
    Bool,
    /// The JSON unit value. Contributes no control and submits `null`.
    Unit,
    /// An optional shape, from `Maybe a`.
    Optional(Box<FormShape>),
    /// A product: one constructor's fields, in declaration order.
    Product {
        type_key: TypeKey,
        constructor: ConstructorKey,
        fields: Vec<FieldShape>,
    },
    /// A sum: alternatives in constructor-declaration order.
    Sum {
        type_key: TypeKey,
        variants: Vec<VariantShape>,
    },
}

/// One named input within a [`FormShape::Product`]. Mirrors `FieldShape`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldShape {
    pub key: FieldKey,
    pub shape: FormShape,
}

/// One alternative within a [`FormShape::Sum`]. Mirrors `VariantShape`. A
/// nullary constructor is an empty [`FormShape::Product`]; `Unit` is reserved
/// for an actual unit value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VariantShape {
    pub constructor: ConstructorKey,
    pub shape: FormShape,
}

/// The bind path a recursively-rendered [`FormShape`] sits at when it IS the
/// whole form — the root `tidepool-web`'s renderer and its submission
/// collector must BOTH start from, since a bind path is only meaningful
/// relative to the root it was built from.
///
/// Non-empty on purpose. A root [`FormShape::Sum`] (what `choose` and
/// `askUser @<enum>` produce) binds its radio group at the root path itself,
/// and HTML does not group radios that share an EMPTY `name` — an empty root
/// would let an operator check two branches of the same choice.
pub const ROOT_BIND_PATH: &str = "answer";

/// Join a parent dotted path with a child field or constructor key. The web
/// renderer and submission collector both use this function. An empty parent
/// yields the bare child key.
#[must_use]
pub fn child_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

/// Humanize a [`FieldKey`]/[`ConstructorKey`] for DISPLAY only: split on
/// camelCase/PascalCase word boundaries, lowercase every word, capitalize
/// the first letter of the joined result. `NeedsReview` → "Needs review",
/// `releaseNote` → "Release note". This is a rendering-time-only transform —
/// [`FormShape`] and every wire path always carry the exact
/// key verbatim; nothing here touches a submitted or serialized key. Runs of
/// capitals currently split per letter (`HTTPServer` → `H t t p server`).
#[must_use]
pub fn humanize_key(key: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in key.chars() {
        if c.is_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    let lower = words
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- humanize_key -----------------------------------------------------

    #[test]
    fn humanize_key_splits_pascal_case() {
        assert_eq!(humanize_key("NeedsReview"), "Needs review");
    }

    #[test]
    fn humanize_key_splits_camel_case() {
        assert_eq!(humanize_key("releaseNote"), "Release note");
    }

    #[test]
    fn humanize_key_single_word_capitalizes() {
        assert_eq!(humanize_key("host"), "Host");
    }

    #[test]
    fn humanize_key_digit_key_passes_through() {
        assert_eq!(humanize_key("1"), "1");
    }

    // ---- child_path ---------------------------------------------------------

    #[test]
    fn child_path_from_root_is_bare_key() {
        assert_eq!(child_path("", "service"), "service");
    }

    #[test]
    fn child_path_nests_with_dot() {
        assert_eq!(child_path("destination", "host"), "destination.host");
    }

    // ---- FormShape JSON encoding -------------------------------------------

    #[test]
    fn leaf_shape_encodes_as_documented() {
        assert_eq!(
            serde_json::to_value(FormShape::String).unwrap(),
            json!("string")
        );
    }

    fn ssh_product_shape() -> FormShape {
        FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![
                FieldShape {
                    key: "host".to_string(),
                    shape: FormShape::String,
                },
                FieldShape {
                    key: "port".to_string(),
                    shape: FormShape::Int,
                },
            ],
        }
    }

    /// Record product: single-constructor datatype, so the answer is a BARE
    /// `Product` — rule 1, no redundant `Sum` wrapper.
    #[test]
    fn record_product_shape_encodes_as_documented() {
        assert_eq!(
            serde_json::to_value(ssh_product_shape()).unwrap(),
            json!({"product": {
                "type_key": "Ssh",
                "constructor": "Ssh",
                "fields": [
                    {"key": "host", "shape": "string"},
                    {"key": "port", "shape": "int"}
                ]
            }})
        );
    }

    fn environment_sum_shape() -> FormShape {
        FormShape::Sum {
            type_key: "Environment".to_string(),
            variants: vec![
                VariantShape {
                    constructor: "Development".to_string(),
                    shape: empty_product("Environment", "Development"),
                },
                VariantShape {
                    constructor: "Staging".to_string(),
                    shape: empty_product("Environment", "Staging"),
                },
                VariantShape {
                    constructor: "Production".to_string(),
                    shape: empty_product("Environment", "Production"),
                },
            ],
        }
    }

    /// Sum shape wire, pinned.
    #[test]
    fn nullary_sum_shape_encodes_as_documented() {
        assert_eq!(
            serde_json::to_value(environment_sum_shape()).unwrap(),
            json!({"sum": {
                "type_key": "Environment",
                "variants": [
                    {"constructor": "Development", "shape": {"product": {"type_key": "Environment", "constructor": "Development", "fields": []}}},
                    {"constructor": "Staging", "shape": {"product": {"type_key": "Environment", "constructor": "Staging", "fields": []}}},
                    {"constructor": "Production", "shape": {"product": {"type_key": "Environment", "constructor": "Production", "fields": []}}}
                ]
            }})
        );
    }

    fn empty_product(type_key: &str, constructor: &str) -> FormShape {
        FormShape::Product {
            type_key: type_key.to_string(),
            constructor: constructor.to_string(),
            fields: vec![],
        }
    }

    /// The bare shape round-trips through serde — the whole wire, end to
    /// end, with no wrapper struct.
    #[test]
    fn form_shape_round_trips() {
        let shape = ssh_product_shape();
        let wire = serde_json::to_string(&shape).unwrap();
        assert_eq!(serde_json::from_str::<FormShape>(&wire).unwrap(), shape);
    }
}
