//! The operator-input seam, consumed as-is by the
//! `AskUser` effect decode, the driver's form servicer, and the web GUI —
//! never redefined at those call sites. Single source for the form-spec /
//! submission wire types and the [`OperatorGate`] the driver blocks on.
//!
//! The gate is **sync-blocking by frozen contract**, mirroring the existing
//! between-loops stdin gate
//! ([`super::driver::SelfHarnessDriver::between_loops_gate`]): `present_form`
//! blocks the calling thread until the operator submits; `await_continue`
//! blocks until the operator advances the loop. A web implementation parks a
//! channel; the headless [`StdinGate`] reads a line. The driver's turn loop
//! is `async fn` and `.await`s the `Harness` directly — the ONE place it
//! still reaches for `tokio::task::block_in_place` is around a call into
//! this genuinely sync-blocking gate, so that block yields the tokio worker
//! to other tasks instead of stalling it.
//!
//! # Recursive forms — [`FormShape`]
//!
//! [`FormSpec`]/[`Field`]/[`Submission`] above are the FLAT v1 wire: one
//! scalar per top-level key, no nesting. They stay live only until the
//! legacy path is deleted.
//!
//! [`FormShape`] is the RECURSIVE shape wire, mirroring Haskell's
//! `Tidepool.Form.Shape` (`FormShape`/`FieldShape`/`VariantShape`)
//! constructor for constructor. `askUser @T` emits a bare [`FormShape`]
//! through the `AskUserWith` suspension; [`crate::engine::classify_hole`]
//! lifts it into [`FormSpec::shape`].
//!
//! ## The shape JSON encoding is OWNED here, not by aeson
//!
//! The Haskell encoder (`Tidepool.Form.Wire`) targets exactly what follows,
//! and `tidepool-runtime/tests/generic_form_wire.rs` asserts it against
//! these worked examples themselves. Every shape below is what
//! `#[derive(Serialize, Deserialize)]` with
//! `#[serde(rename_all = "snake_case")]` (the default EXTERNALLY TAGGED
//! representation) actually produces — the derive is the source of truth.
//!
//! A unit-payload variant (`FormShape::String`, …) serializes as a bare JSON
//! string of its snake_case name. A newtype variant
//! (`FormShape::Optional(Box<FormShape>)`) serializes as
//! `{"optional": <inner>}`. A struct variant (`FormShape::Product { .. }`)
//! serializes as `{"product": {<fields>}}`.
//!
//! ## The ANSWER is ordinary JSON — there is no answer wire type
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
//! * unit form (`askUser @()`) → `[]` (the vendored aeson-1.5 `()` decode);
//! * leaves → the corresponding JSON scalar.
//!
//! The collector that builds this JSON from the rendered controls is
//! `tidepool-web`'s submission path, guided by the same [`FormShape`].
use serde::{Deserialize, Serialize};

/// A typed form an agent spawned. Rendered by the web GUI; its
/// [`Submission`] decodes back to the agent's `a`.
///
/// TWO forms arrive here, and which one it is depends on which field is
/// populated:
///
/// - `fields` — the FLAT v1 wire (enum (1-of-N) / int / text / bool, one
///   scalar per top-level key), built by the de-advertised applicative
///   builder (`Tidepool.Form.Legacy`). No Haskell caller emits it any more;
///   it stays live until the successor lane confirms nothing else consumes
///   it.
/// - `shape` — the RECURSIVE [`FormShape`] derived from the answer TYPE by
///   `askUser @T` (`Tidepool.Form`). This is what the live surface emits.
///   The Haskell side sends the shape BARE — exactly the JSON documented in
///   this module — and [`crate::engine::classify_hole`] lifts it into this
///   struct, so nothing on the wire carries an invented envelope.
///
/// A gate renders `shape` when it is present and `fields` otherwise. The
/// [`Submission`] it returns is a flat map either way: for a `shape` form
/// that map IS the serialized [`FormAnswer`] (every non-unit `FormAnswer`
/// variant is a one-key JSON object — see the encoding docs above), which is
/// what `askUser @T` reads back.
///
/// `fields` stays REQUIRED on the wire: that is what keeps a bare
/// [`FormShape`] JSON from deserializing into an empty `FormSpec` (serde
/// ignores unknown fields), so the two wires stay tellable apart by decode
/// alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormSpec {
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<FormShape>,
}

/// One field in a [`FormSpec`]. `key` is the stable submission key; `label` is
/// the operator-facing prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
}

/// The v1 typed primitives. `Enum` is a 1-of-N choice over labelled tags (the
/// `tag` is what the submission carries; the `label` is what the operator sees).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    Enum { options: Vec<EnumOption> },
    Int,
    Text,
    Bool,
}

/// One choice in an [`FieldKind::Enum`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnumOption {
    pub label: String,
    pub tag: String,
}

/// A flat CLIENT submission: one scalar JSON value per field `key` (enum →
/// the chosen `tag` string, int → number, text → string, bool → bool). ONE
/// canonical shape — no `{values,prose}` coercion, no keyed/unkeyed duality.
/// This is the shape a browser/API client POSTs; it is NOT the gate's return
/// type — see [`OperatorGate::present_form`].
pub type Submission = serde_json::Map<String, serde_json::Value>;

/// The seam the driver blocks on for operator input. Sync-blocking by design
/// (see module docs). A web GUI implements this by parking a channel
/// resolved from an HTTP handler; [`StdinGate`] keeps headless runs working.
pub trait OperatorGate: Send + Sync {
    /// Present `spec` to the operator and BLOCK until they submit. Returns the
    /// ANSWER VALUE ready for the Haskell decode: for a legacy flat form, the
    /// [`Submission`] object; for a shape-carrying form (`askUser @T`), the
    /// reassembled structural `FormAnswer` — which for a unit-shaped form is
    /// the bare JSON string `"unit"`, NOT an object. The transport is a full
    /// `Value` precisely so that answer survives; forcing an object here is
    /// what made `askUser @()` re-prompt forever. A decode failure Haskell-side
    /// re-presents the form (the retry lives in `askUser`).
    fn present_form(&self, spec: &FormSpec) -> serde_json::Value;

    /// BLOCK until the operator advances to the next loop iteration (the
    /// human button-click gate that replaces the stdin between-loops gate).
    fn await_continue(&self);
}

/// Headless default: `await_continue` reads a line from stdin (the current
/// between-loops behavior); `present_form` reads one JSON line as the flat
/// submission, so non-web/CLI drives and tests still work. Used as the
/// default when no web gate is configured.
#[derive(Debug, Default)]
pub struct StdinGate;

impl OperatorGate for StdinGate {
    fn present_form(&self, _spec: &FormSpec) -> serde_json::Value {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return serde_json::Value::Object(Submission::new());
        }
        // Any JSON value passes through — a bare `"unit"` line answers a
        // unit-shaped form. An unparseable line degrades to `{}`, which the
        // Haskell decode rejects and re-presents (never a panic mid-drive).
        serde_json::from_str(line.trim())
            .unwrap_or_else(|_| serde_json::Value::Object(Submission::new()))
    }

    fn await_continue(&self) {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
}

// -------------------------------------------------------------------------
// Recursive form shape/answer — mirrors Tidepool.Form.Shape (see module docs
// for the JSON contract this section defines).
// -------------------------------------------------------------------------

/// A datatype's own name — the form's title, and the key a sum is
/// identified by. Mirrors `Tidepool.Form.Shape.TypeKey`.
pub type TypeKey = String;

/// A constructor's name — the stable key for one branch of a sum, and the
/// identity of a product node. Mirrors `Tidepool.Form.Shape.ConstructorKey`.
pub type ConstructorKey = String;

/// A field's key within one product node: an exact record selector name, or
/// a one-based positional index rendered as text, scoped to its own product
/// node. Mirrors `Tidepool.Form.Shape.FieldKey`.
pub type FieldKey = String;

/// The structural description of a recursive form, derived from a type's
/// generic representation without ever seeing a value of that type. Mirrors
/// Haskell's `Tidepool.Form.Shape.FormShape` constructor for constructor.
/// See the module docs for the frozen JSON encoding.
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
    /// No payload — a nullary constructor's branch. Contributes no control.
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
/// nullary constructor's shape is [`FormShape::Unit`].
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

/// Join a parent dotted-path bind key with a child field/constructor key —
/// the ONE path-construction convention the recursive renderer
/// (`tidepool-web`'s `render::generic_shape`) and the nested-submission
/// collector (`tidepool-web`'s `server::collect_form_answer`) must share, so
/// a bind path the renderer emits is always exactly the path the collector
/// looks up. An empty parent (the form's root) yields the bare `key`.
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
/// [`FormShape`]/[`FormAnswer`] and every wire path always carry the exact
/// key verbatim; nothing here ever touches a submitted or serialized key.
/// Not acronym-aware: a run of capitals (`HTTPServer`) splits per letter —
/// out of scope, since the PRD's own examples are plain camelCase/PascalCase.
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
    fn humanize_key_positional_key_passthrough() {
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
                    shape: FormShape::Unit,
                },
                VariantShape {
                    constructor: "Staging".to_string(),
                    shape: FormShape::Unit,
                },
                VariantShape {
                    constructor: "Production".to_string(),
                    shape: FormShape::Unit,
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
                    {"constructor": "Development", "shape": "unit"},
                    {"constructor": "Staging", "shape": "unit"},
                    {"constructor": "Production", "shape": "unit"}
                ]
            }})
        );
    }

    /// Flat [`FormSpec`]/[`Submission`] path stays green alongside the new
    /// recursive types — the existing wire round trip test lives in
    /// `tidepool-web`'s `server.rs`; this just confirms both type families
    /// coexist in one module without collision.
    #[test]
    fn flat_and_recursive_types_coexist() {
        let _flat = FormSpec {
            fields: vec![Field {
                key: "mood".to_string(),
                label: "Mood".to_string(),
                kind: FieldKind::Bool,
            }],
            shape: None,
        };
        let _recursive = FormShape::Bool;
    }

    /// The two wires are tellable apart BY DECODE, which is what
    /// [`crate::engine::classify_hole`] relies on: a bare `FormShape` (what
    /// `askUser @T` emits) must NOT deserialize into an empty flat
    /// `FormSpec`. `fields` being required is the whole mechanism — serde
    /// ignores unknown fields, so an optional `fields` would swallow it.
    #[test]
    fn a_bare_shape_is_not_a_flat_form_spec() {
        let shape = serde_json::to_value(ssh_product_shape()).unwrap();
        assert!(
            serde_json::from_value::<FormSpec>(shape).is_err(),
            "a bare FormShape must not decode as a flat FormSpec"
        );
    }

    /// A shape-carrying spec round-trips, and a flat spec still serializes
    /// WITHOUT a `shape` key (nothing on the existing wire changes shape).
    #[test]
    fn shape_carrying_spec_round_trips_and_flat_spec_is_unchanged() {
        let shaped = FormSpec {
            fields: vec![],
            shape: Some(ssh_product_shape()),
        };
        let wire = serde_json::to_string(&shaped).unwrap();
        assert_eq!(serde_json::from_str::<FormSpec>(&wire).unwrap(), shaped);

        let flat = FormSpec {
            fields: vec![Field {
                key: "mood".to_string(),
                label: "Mood".to_string(),
                kind: FieldKind::Bool,
            }],
            shape: None,
        };
        // No `shape` key at all — `skip_serializing_if` keeps the existing
        // flat wire byte-identical to what it was before `shape` existed.
        // (`kind` nests because [`FieldKind`] is INTERNALLY tagged on `kind`;
        // that is the pre-existing flat encoding, not something added here.)
        assert_eq!(
            serde_json::to_value(&flat).unwrap(),
            json!({"fields": [{"key": "mood", "label": "Mood", "kind": {"kind": "bool"}}]})
        );
    }
}
