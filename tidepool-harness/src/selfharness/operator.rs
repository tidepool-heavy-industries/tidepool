//! The operator-input seam (`09-askuser-form-gui.md`), consumed as-is by the
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
//! # Recursive forms — [`FormShape`] / [`FormAnswer`]
//!
//! [`FormSpec`]/[`Field`]/[`Submission`] above are the FLAT v1 wire: one
//! scalar per top-level key, no nesting. They stay live and unchanged — the
//! `askUser` effect still emits and decodes them today.
//!
//! [`FormShape`] and [`FormAnswer`] are the RECURSIVE wire, mirroring
//! Haskell's `Tidepool.Form.Shape` (`FormShape`/`FieldShape`/`VariantShape`/
//! `FormAnswer`) constructor for constructor. They exist ALONGSIDE the flat
//! types, not in place of them: the generic-derived `askUser @T` path (PRD
//! `plans/self-iterating-harness/14-generic-derived-askuser-prd.md`) has not
//! been wired into the effect yet — that is a later step in this wave. This
//! module is the Rust half of that contract, landed early so the later
//! Haskell encoder and the web renderer have something concrete to target.
//!
//! ## The JSON encoding is OWNED here, not by aeson
//!
//! The Haskell side does not (yet) have an encoder for this wire — a later
//! step writes one against exactly what follows. It is an internal
//! transport representation, not an aeson `ToJSON`/`FromJSON` contract:
//! nothing on either side derives or routes through generic JSON codecs for
//! these types. Every shape below is what `#[derive(Serialize,
//! Deserialize)]` with `#[serde(rename_all = "snake_case")]` (the default
//! EXTERNALLY TAGGED representation) actually produces — the derive is the
//! source of truth; the JSON here documents it, not the other way around.
//!
//! A unit-payload enum variant (`FormShape::String`, `FormAnswer::Unit`, …)
//! serializes as a bare JSON string equal to its snake_case variant name. A
//! newtype variant (`FormShape::Optional(Box<FormShape>)`) serializes as a
//! single-key object `{"optional": <inner>}`. A struct variant
//! (`FormShape::Product { .. }`) serializes as `{"product": {<fields>}}`.
//!
//! [`FormAnswer::Product`] deliberately wraps `Vec<(FieldKey, FormAnswer)>`,
//! NOT a JSON object: a JSON object silently collapses a duplicate key on
//! parse, which would make `FormError::DuplicateField`-style detection
//! (Haskell side, once the decoder exists) impossible to implement against
//! this wire. An array of `[key, value]` pairs preserves duplicates and
//! order for the decoder to reject or accept as it sees fit. Concretely, a
//! `Vec<(String, X)>` field serializes as a JSON array of 2-element arrays
//! (Rust tuples are sequences under serde), e.g. `[["host", …], ["port",
//! …]]`.
//!
//! ### The four encoding rules (frozen answer algebra, mirrored from
//! Haskell's `FormAnswer` haddock and
//! `plans/self-iterating-harness/16-generic-spike-receipts.md`)
//!
//! These are usage discipline for whoever CONSTRUCTS a [`FormAnswer`]
//! against a given [`FormShape`] (the future collector in `tidepool-web`,
//! and the future Haskell encoder) — the Rust enum does not and cannot
//! enforce them by itself, exactly as the Haskell ADT doesn't either:
//!
//! 1. A single-constructor datatype answers with a bare [`FormAnswer::Product`]
//!    — never wrap it in [`FormAnswer::Sum`].
//! 2. A multi-constructor datatype ALWAYS answers [`FormAnswer::Sum`]
//!    `{constructor, payload}`, including when the chosen branch is nullary.
//! 3. A nullary constructor's payload is [`FormAnswer::Unit`], never an
//!    empty [`FormAnswer::Product`] — so a nullary branch and a zero-field
//!    record stay distinguishable on the wire (`"unit"` vs `{"product":
//!    []}`).
//! 4. `Maybe`/optional fields answer with [`FormAnswer::Optional`], never a
//!    constructor pick.
//!
//! ## Worked examples
//!
//! One per shape, using the spike's own `DeployRequest` fixture
//! (`16-generic-spike-receipts.md`) so the JSON below is traceable back to
//! that verbatim `FormShape` dump.
//!
//! **Leaf** — `service :: Text`, answer `"api"`:
//! ```text
//! shape:  "string"
//! answer: {"string":"api"}
//! ```
//!
//! **Optional, present** — `releaseNote :: Maybe Text`, answer `Just
//! "hotfix"`:
//! ```text
//! shape:  {"optional":"string"}
//! answer: {"optional":{"string":"hotfix"}}
//! ```
//!
//! **Optional, absent** — same field, answer `Nothing`:
//! ```text
//! shape:  {"optional":"string"}
//! answer: {"optional":null}
//! ```
//!
//! **Record product** (single-constructor `Ssh { host :: Text, port :: Int
//! }`, so the answer is a BARE product — rule 1):
//! ```text
//! shape:  {"product":{"type_key":"Ssh","constructor":"Ssh","fields":[
//!           {"key":"host","shape":"string"},
//!           {"key":"port","shape":"int"}]}}
//! answer: {"product":[["host",{"string":"example.com"}],["port",{"int":22}]]}
//! ```
//!
//! **Nullary sum branch** — `environment :: Environment` (`Development |
//! Staging | Production`), answer picks `Staging` (rule 2: still a `Sum`
//! wrapper even though the payload is nullary; rule 3: payload is `"unit"`):
//! ```text
//! shape:  {"sum":{"type_key":"Environment","variants":[
//!           {"constructor":"Development","shape":"unit"},
//!           {"constructor":"Staging","shape":"unit"},
//!           {"constructor":"Production","shape":"unit"}]}}
//! answer: {"sum":{"constructor":"Staging","payload":"unit"}}
//! ```
//!
//! **Payload-bearing sum branch** — `destination :: Destination`
//! (`LocalHost | Ssh { host, port } | Container { image }`), answer picks
//! `Ssh`:
//! ```text
//! answer: {"sum":{"constructor":"Ssh","payload":
//!           {"product":[["host",{"string":"example.com"}],["port",{"int":22}]]}}}
//! ```
//!
//! **Nested product-of-sum** — the whole `DeployRequest`, combining all of
//! the above (see [`tests::deploy_request_nested_product_of_sum_round_trips`]
//! for this exact value asserted against `serde_json`):
//! ```text
//! {"product":[
//!   ["service",{"string":"api"}],
//!   ["environment",{"sum":{"constructor":"Staging","payload":"unit"}}],
//!   ["destination",{"sum":{"constructor":"Ssh","payload":
//!     {"product":[["host",{"string":"example.com"}],["port",{"int":22}]]}}}],
//!   ["replicas",{"int":3}],
//!   ["runMigrations",{"bool":false}],
//!   ["releaseNote",{"optional":{"string":"hotfix"}}]
//! ]}
//! ```

use serde::{Deserialize, Serialize};

/// A typed form the agent spawned via `askUser :: Form a -> M a`. Rendered by
/// the web GUI; its [`Submission`] decodes back to the agent's `a`. v1 field
/// kinds only: enum (1-of-N) / int / text / bool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormSpec {
    pub fields: Vec<Field>,
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

/// A flat operator submission: one scalar JSON value per field `key` (enum →
/// the chosen `tag` string, int → number, text → string, bool → bool). ONE
/// canonical shape — no `{values,prose}` coercion, no keyed/unkeyed duality.
pub type Submission = serde_json::Map<String, serde_json::Value>;

/// The seam the driver blocks on for operator input. Sync-blocking by design
/// (see module docs). A web GUI implements this by parking a channel
/// resolved from an HTTP handler; [`StdinGate`] keeps headless runs working.
pub trait OperatorGate: Send + Sync {
    /// Present `spec` to the operator and BLOCK until they submit. The returned
    /// [`Submission`] is decoded against the form by the caller; a decode
    /// failure re-presents the form (the retry is Haskell-side in `askUser`).
    fn present_form(&self, spec: &FormSpec) -> Submission;

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
    fn present_form(&self, _spec: &FormSpec) -> Submission {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return Submission::new();
        }
        serde_json::from_str(line.trim()).unwrap_or_default()
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

/// What the operator submitted, structurally — mirrors Haskell's
/// `Tidepool.Form.Shape.FormAnswer` constructor for constructor. See the
/// module docs for the four encoding rules a caller constructing one of
/// these against a [`FormShape`] must follow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FormAnswer {
    String(String),
    Int(i64),
    Number(f64),
    Bool(bool),
    Unit,
    Optional(Option<Box<FormAnswer>>),
    /// Field/value pairs, in the shape's declaration order. A `Vec` of pairs
    /// rather than a JSON object — see the module docs on why (duplicate-key
    /// preservation for `FormError::DuplicateField`-style detection).
    Product(Vec<(FieldKey, FormAnswer)>),
    Sum {
        constructor: ConstructorKey,
        payload: Box<FormAnswer>,
    },
}

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

    // ---- FormShape / FormAnswer JSON encoding ------------------------------

    #[test]
    fn leaf_shape_and_answer_encode_as_documented() {
        assert_eq!(serde_json::to_value(FormShape::String).unwrap(), json!("string"));
        assert_eq!(
            serde_json::to_value(FormAnswer::String("api".to_string())).unwrap(),
            json!({"string": "api"})
        );
    }

    #[test]
    fn optional_present_and_absent_encode_as_documented() {
        let present = FormAnswer::Optional(Some(Box::new(FormAnswer::String("hotfix".to_string()))));
        assert_eq!(
            serde_json::to_value(&present).unwrap(),
            json!({"optional": {"string": "hotfix"}})
        );

        let absent = FormAnswer::Optional(None);
        assert_eq!(serde_json::to_value(&absent).unwrap(), json!({"optional": null}));
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

    fn ssh_product_answer() -> FormAnswer {
        FormAnswer::Product(vec![
            ("host".to_string(), FormAnswer::String("example.com".to_string())),
            ("port".to_string(), FormAnswer::Int(22)),
        ])
    }

    /// Record product: single-constructor datatype, so the answer is a BARE
    /// `Product` — rule 1, no redundant `Sum` wrapper.
    #[test]
    fn record_product_shape_and_answer_encode_as_documented() {
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
        assert_eq!(
            serde_json::to_value(ssh_product_answer()).unwrap(),
            json!({"product": [["host", {"string": "example.com"}], ["port", {"int": 22}]]})
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

    /// Nullary sum branch: rule 2 (`Sum` wrapper even though nullary) + rule
    /// 3 (payload is `Unit`, not an empty `Product`).
    #[test]
    fn nullary_sum_branch_encodes_as_documented() {
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
        let answer = FormAnswer::Sum {
            constructor: "Staging".to_string(),
            payload: Box::new(FormAnswer::Unit),
        };
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            json!({"sum": {"constructor": "Staging", "payload": "unit"}})
        );
    }

    /// A nullary branch and a hypothetical zero-field record must stay
    /// distinguishable on the wire (rule 3).
    #[test]
    fn nullary_payload_distinguishable_from_empty_product() {
        let nullary = serde_json::to_value(FormAnswer::Unit).unwrap();
        let empty_product = serde_json::to_value(FormAnswer::Product(vec![])).unwrap();
        assert_ne!(nullary, empty_product);
        assert_eq!(nullary, json!("unit"));
        assert_eq!(empty_product, json!({"product": []}));
    }

    /// Payload-bearing sum branch: `destination :: Destination` picks `Ssh`.
    #[test]
    fn payload_bearing_sum_branch_encodes_as_documented() {
        let answer = FormAnswer::Sum {
            constructor: "Ssh".to_string(),
            payload: Box::new(ssh_product_answer()),
        };
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            json!({"sum": {"constructor": "Ssh", "payload":
                {"product": [["host", {"string": "example.com"}], ["port", {"int": 22}]]}
            }})
        );
    }

    fn deploy_request_answer() -> FormAnswer {
        FormAnswer::Product(vec![
            ("service".to_string(), FormAnswer::String("api".to_string())),
            (
                "environment".to_string(),
                FormAnswer::Sum {
                    constructor: "Staging".to_string(),
                    payload: Box::new(FormAnswer::Unit),
                },
            ),
            (
                "destination".to_string(),
                FormAnswer::Sum {
                    constructor: "Ssh".to_string(),
                    payload: Box::new(ssh_product_answer()),
                },
            ),
            ("replicas".to_string(), FormAnswer::Int(3)),
            ("runMigrations".to_string(), FormAnswer::Bool(false)),
            (
                "releaseNote".to_string(),
                FormAnswer::Optional(Some(Box::new(FormAnswer::String("hotfix".to_string())))),
            ),
        ])
    }

    /// The wire round trip DONE criterion: serialize, deserialize, compare —
    /// over the spike's own nested product-of-sum `DeployRequest` fixture,
    /// with every selector/constructor key surviving verbatim.
    #[test]
    fn deploy_request_nested_product_of_sum_round_trips() {
        let answer = deploy_request_answer();
        let wire = serde_json::to_string(&answer).unwrap();
        let back: FormAnswer = serde_json::from_str(&wire).unwrap();
        assert_eq!(answer, back);

        let value = serde_json::to_value(&answer).unwrap();
        assert_eq!(
            value,
            json!({"product": [
                ["service", {"string": "api"}],
                ["environment", {"sum": {"constructor": "Staging", "payload": "unit"}}],
                ["destination", {"sum": {"constructor": "Ssh", "payload":
                    {"product": [["host", {"string": "example.com"}], ["port", {"int": 22}]]}
                }}],
                ["replicas", {"int": 3}],
                ["runMigrations", {"bool": false}],
                ["releaseNote", {"optional": {"string": "hotfix"}}]
            ]})
        );

        // Exact keys survive: constructor tags and field keys are found
        // verbatim in the serialized wire, never humanized.
        let FormAnswer::Product(fields) = &back else {
            panic!("expected a Product answer");
        };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["service", "environment", "destination", "replicas", "runMigrations", "releaseNote"]
        );
        let Some((_, FormAnswer::Sum { constructor, .. })) =
            fields.iter().find(|(k, _)| k == "destination")
        else {
            panic!("expected destination to be a Sum answer");
        };
        assert_eq!(constructor, "Ssh");
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
        };
        let _recursive = FormShape::Bool;
    }
}
