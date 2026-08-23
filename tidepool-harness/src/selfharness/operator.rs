//! Operator input shared by the `AskUser` effect decoder, self-harness driver,
//! and web UI.
//!
//! [`OperatorGate`] is synchronous: `present_form` blocks until submission —
//! including the between-loops gate, which is an ordinary driver-authored
//! form (`SelfHarnessDriver::between_loops_gate`), not a second mechanism.
//! The web implementation parks a channel; [`StdinGate`] reads a line. The
//! async driver isolates the blocking call with `tokio::task::block_in_place`.
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
//! `FieldShape` and the `Product`/`Sum` variants of `FormShape` may each
//! carry an optional `"doc"` string — a sentence or two of help text (on the
//! ROOT shape, the form's title/intro prose). Absent by default
//! (`#[serde(default, skip_serializing_if = "Option::is_none")]`), so a wire
//! without it decodes and re-encodes byte-identically to before.
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
use std::sync::Arc;

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

    /// Post display-only narration (`note`, riding `AskUser`'s `NoteWith`
    /// constructor) to the operator's accumulating feed. Does NOT block —
    /// the driver resumes the session immediately after calling this, with
    /// no submission to wait for. Default no-op so every existing
    /// [`OperatorGate`] impl (in particular [`StdinGate`], and any test
    /// gate) stays valid without change; a web/GUI gate overrides it to
    /// actually display the text.
    fn post_note(&self, _text: &str) {}

    /// Post the LAST answerer round's compiled Haskell source — the same
    /// text a `TurnStart{source}` log line carries — so the operator can see
    /// what actually ran. Called once per compiled round, not accumulated:
    /// implementations keep only the most recent source, not a history.
    /// Default no-op, same reasoning as [`Self::post_note`].
    fn post_turn_source(&self, _source: &str) {}

    /// Resolve (registering it if needed) a gate scoped to one labeled
    /// window — PRD 21 C5's per-node operator GUI: a `runLLMTurnBranchLabeled`
    /// child's asks/notes route here instead of the default gate when this
    /// returns `Some`. `label` is the caller-chosen Text carried on the wire
    /// (never parsed out of a prompt). Default `None`, so every existing
    /// [`OperatorGate`] impl (in particular [`StdinGate`], and any test gate)
    /// stays valid without change and continues presenting every ask on the
    /// one default gate.
    fn node_gate(&self, _label: &str) -> Option<Arc<dyn OperatorGate>> {
        None
    }

    /// Mark the labeled window `label` as finished — called once, at that
    /// node's terminate/fold point, regardless of how it finished (answered,
    /// exited, or closure-refused). Default no-op, same reasoning as
    /// [`Self::post_note`]; a web/GUI gate overrides it to grey the node's
    /// section while keeping its history readable.
    fn retire_node(&self, _label: &str) {}

    /// The labeled window's starting prompt — the authored brief the window
    /// was opened with, sent once at birth, right after the eager
    /// [`Self::node_gate`] registration. Default no-op, same reasoning as
    /// [`Self::post_note`]; a web/GUI gate stores it so the operator can see
    /// what a node was asked to do, not only what it says.
    fn node_seeded(&self, _label: &str, _seed: &str) {}

    /// The labeled window's finalized answer, rendered to JSON text — the
    /// same rendering `Event::Finalize` carries. Called on the success path
    /// only, after [`Self::retire_node`] (retirement marks the END; the
    /// value materializes a few steps later, when the window's own
    /// post-finalize prefix is frozen). Default no-op.
    fn node_finalized(&self, _label: &str, _value: &str) {}

    /// The labeled window ended WITHOUT an answer — `reason` is the
    /// `InvocationExit` rendering (round exhaustion, non-finalize ending,
    /// provider failure) or the closure-refusal message. Called after
    /// [`Self::retire_node`], mutually exclusive with
    /// [`Self::node_finalized`]. Default no-op.
    fn node_failed(&self, _label: &str, _reason: &str) {}

    /// Withdraw a still-outstanding [`Self::present_form`] presentation of
    /// `shape` that the caller no longer needs an answer to — a SECOND
    /// resolution plane settled the same decision first (e.g.
    /// `Harness::resolve_escalation`'s direct override racing an escalation
    /// gate ask), or the caller gave up waiting (a timeout). A gate whose
    /// matching ask is STILL pending should stop presenting it as
    /// actionable and release the blocked `present_form` call, if any —
    /// nothing reads that call's return value once it has been abandoned,
    /// so any resolution unblocks it. A no-op when nothing pending matches
    /// `shape` (already answered through the gate, or this gate never
    /// published it) is expected and safe: a caller cannot know which plane
    /// won without asking every gate. Default no-op — [`StdinGate`] and any
    /// gate with no timeline to clean up need no override.
    fn retract_form(&self, _shape: &FormShape) {}
}

/// Headless default: `present_form` reads one JSON value per line (this
/// covers the between-loops gate too — it is an ordinary form), so
/// non-web/CLI drives and tests still work. Used as the default when no web
/// gate is configured.
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
        /// On the ROOT shape, the form's title/intro prose. Optional, absent
        /// by default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
    /// A sum: alternatives in constructor-declaration order.
    Sum {
        type_key: TypeKey,
        variants: Vec<VariantShape>,
        /// On the ROOT shape, the form's title/intro prose. Optional, absent
        /// by default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc: Option<String>,
    },
}

/// One named input within a [`FormShape::Product`]. Mirrors `FieldShape`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldShape {
    pub key: FieldKey,
    pub shape: FormShape,
    /// A sentence or two of help text for this field. Optional, absent by
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
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

    // ---- OperatorGate registrar defaults ------------------------------------

    /// [`OperatorGate::node_gate`]/[`OperatorGate::retire_node`] are
    /// default-implemented — [`StdinGate`] (and any other existing gate)
    /// stays valid with zero edits, and the default routes every ask to the
    /// one default gate (`node_gate` returns `None`).
    #[test]
    fn stdin_gate_has_no_per_node_registrar_by_default() {
        assert!(StdinGate.node_gate("root/1-x").is_none());
        StdinGate.retire_node("root/1-x"); // must not panic
    }

    /// The node-lifecycle extensions ([`OperatorGate::node_seeded`],
    /// [`OperatorGate::node_finalized`], [`OperatorGate::node_failed`]) are
    /// default no-ops for the same reason: every pre-existing gate stays
    /// valid with zero edits.
    #[test]
    fn node_lifecycle_methods_default_to_no_ops() {
        StdinGate.node_seeded("root/1-x", "NODE root/1 — DISCOVER …");
        StdinGate.node_finalized("root/1-x", "{\"tag\":\"FinishLayer\"}");
        StdinGate.node_failed("root/1-x", "round exhaustion");
    }

    /// [`OperatorGate::retract_form`] is default-implemented as a no-op for
    /// the same reason — [`StdinGate`] never publishes a timeline entry, so
    /// it has nothing to withdraw.
    #[test]
    fn retract_form_defaults_to_a_no_op() {
        StdinGate.retract_form(&FormShape::String); // must not panic
    }

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
                    doc: None,
                },
                FieldShape {
                    key: "port".to_string(),
                    shape: FormShape::Int,
                    doc: None,
                },
            ],
            doc: None,
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
            doc: None,
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
            doc: None,
        }
    }

    /// A docless shape's encoding is byte-identical to what it was before
    /// `doc` existed — the pinned wire strings from
    /// `record_product_shape_encodes_as_documented` /
    /// `nullary_sum_shape_encodes_as_documented`, reasserted here as exact
    /// serialized strings (not just `Value` equality) so a stray empty
    /// `"doc":null` could never sneak past `skip_serializing_if`.
    #[test]
    fn docless_encoding_is_byte_identical_to_before() {
        assert_eq!(
            serde_json::to_string(&ssh_product_shape()).unwrap(),
            r#"{"product":{"type_key":"Ssh","constructor":"Ssh","fields":[{"key":"host","shape":"string"},{"key":"port","shape":"int"}]}}"#
        );
        assert_eq!(
            serde_json::to_string(&environment_sum_shape()).unwrap(),
            r#"{"sum":{"type_key":"Environment","variants":[{"constructor":"Development","shape":{"product":{"type_key":"Environment","constructor":"Development","fields":[]}}},{"constructor":"Staging","shape":{"product":{"type_key":"Environment","constructor":"Staging","fields":[]}}},{"constructor":"Production","shape":{"product":{"type_key":"Environment","constructor":"Production","fields":[]}}}]}}"#
        );
    }

    /// A doc-carrying shape encodes its `doc` string at the field, at the
    /// root product, and at the root sum — and round-trips.
    #[test]
    fn doc_carrying_shape_encodes_and_round_trips() {
        let shape = FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![FieldShape {
                key: "host".to_string(),
                shape: FormShape::String,
                doc: Some("The hostname to connect to.".to_string()),
            }],
            doc: Some("Configure the SSH connection.".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&shape).unwrap(),
            json!({"product": {
                "type_key": "Ssh",
                "constructor": "Ssh",
                "fields": [
                    {"key": "host", "shape": "string", "doc": "The hostname to connect to."}
                ],
                "doc": "Configure the SSH connection."
            }})
        );
        let wire = serde_json::to_string(&shape).unwrap();
        assert_eq!(serde_json::from_str::<FormShape>(&wire).unwrap(), shape);

        let sum = FormShape::Sum {
            type_key: "Environment".to_string(),
            variants: vec![VariantShape {
                constructor: "Development".to_string(),
                shape: empty_product("Environment", "Development"),
            }],
            doc: Some("Pick an environment.".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&sum).unwrap()["sum"]["doc"],
            json!("Pick an environment.")
        );
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
