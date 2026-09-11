//! The type-declaration vocabulary: one ordered field list, three renderings.
//!
//! This is the answer to the invariant `tidepool-bridge-effects` used to assert
//! by comment:
//!
//! > Field ORDER in these structs is the wire contract and must match those
//! > `type_defs` decls positionally.
//!
//! Two hand-maintained lists, one invariant, zero enforcement. A field inserted
//! in the middle of one and appended to the other type-checks on both sides and
//! silently swaps two payloads on the wire.
//!
//! Here there is ONE [`Vec<RecordField>`]. The Haskell `data` declaration, the
//! Rust wire struct, and the `ToJSON` instance are three traversals of it. They
//! cannot disagree, because there is nothing for them to disagree about — the
//! comment does not get a better guard, it gets deleted.
//!
//! Everything here is CLOSED. There is no raw-Haskell slot and no raw-Rust
//! slot. A declaration that cannot be spelled with these shapes stays
//! hand-written OUTSIDE the contract; it is never smuggled in as a string.

use crate::hs::HsType;

// ---------------------------------------------------------------------------
// TypeDef
// ---------------------------------------------------------------------------

/// One supporting type declaration in an effect's contract.
///
/// A `TypeDef` is the single source for up to four artifacts: the Haskell `data`
/// declaration, its `ToJSON` instance, the Rust wire struct/enum in
/// `tidepool-bridge-effects`, and the mechanical domain↔wire adapters in
/// `tidepool-handlers`.
#[derive(Clone, Debug)]
pub struct TypeDef {
    /// The HASKELL type name — and, for a [`TypeShape::Record`] or
    /// [`TypeShape::Identity`], its data constructor too.
    pub name: &'static str,
    /// The Rust wire type name when it differs from [`Self::name`]
    /// (`WtWorktreeId` for `WorktreeId`), or `None` when they agree.
    ///
    /// Carried as DATA so retiring the `Wt`/`Ev`/`Ag` prefixes is a one-line
    /// schema edit rather than a rename across lane boundaries: when the last
    /// mirror family is generated, this field drops from every `TypeDef` in
    /// one sweep.
    pub wire_rust: Option<&'static str>,
    /// Defining Haskell module for nominal bridge lookup, when known.
    pub core_module: Option<&'static str>,
    /// What kind of declaration this is.
    pub shape: TypeShape,
    /// Which `ToJSON` instance to emit, if any.
    pub json: JsonInstance,
    /// The derive set on the Rust wire type.
    pub derives: WireDerives,
    /// The richer domain type this wire type mirrors, and which direction of
    /// the conversion is mechanical enough to generate.
    pub domain: Option<DomainMap>,
    /// Rust doc-comment lines for the wire type, WITHOUT their `/// ` prefix.
    pub doc: &'static [&'static str],
}

impl TypeDef {
    /// The Rust wire type's name.
    #[must_use]
    pub fn wire_name(&self) -> &'static str {
        self.wire_rust.unwrap_or(self.name)
    }

    /// Does the emitted Rust type need `#[core(name = …)]`?
    ///
    /// Only when the Rust name differs from the Haskell name AND the type is a
    /// struct. An ENUM's data constructors ARE its variant names, so the
    /// bridge never looks its type name up — an attribute there would be inert
    /// at best and misleading at worst. This is the rule the hand-written `Wt*`
    /// block follows today (every struct carries it, no enum does), promoted
    /// from convention to a function.
    #[must_use]
    pub fn needs_core_name(&self) -> bool {
        self.wire_rust.is_some_and(|w| w != self.name)
            && !matches!(self.shape, TypeShape::Sum { .. })
    }

    /// The Haskell `data … deriving (Show, Eq)` declaration.
    #[must_use]
    pub fn render_decl(&self) -> String {
        let name = self.name;
        let body = match &self.shape {
            TypeShape::Record { fields } => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{} :: {}", f.hs_name, f.ty.render()))
                    .collect();
                format!("{name} {{ {} }}", fs.join(", "))
            }
            TypeShape::Sum { variants } => {
                let vs: Vec<String> = variants
                    .iter()
                    .map(|v| match &v.fields {
                        VariantFields::Positional(fields) => {
                            let mut rendered = String::from(v.ctor);
                            for field in fields {
                                rendered.push(' ');
                                rendered.push_str(&field.render_app_arg());
                            }
                            rendered
                        }
                        VariantFields::Named(fields) => {
                            let rendered: Vec<String> = fields
                                .iter()
                                .map(|field| format!("{} :: {}", field.hs_name, field.ty.render()))
                                .collect();
                            format!("{} {{ {} }}", v.ctor, rendered.join(", "))
                        }
                    })
                    .collect();
                vs.join(" | ")
            }
            TypeShape::Identity { payload, .. } => {
                format!("{name} {}", payload.hs().render_app_arg())
            }
        };
        format!("data {name} = {body} deriving (Show, Eq)")
    }

    /// The `instance ToJSON …` declaration, or `None` for
    /// [`JsonInstance::None`].
    ///
    /// # Panics
    /// Panics when the instance does not match the shape (a `Transparent`
    /// instance on a record, an `Object` key naming a field the record does not
    /// have). [`Self::validate`] reports the same problems as a list first; the
    /// panic is the generation-time backstop.
    #[must_use]
    pub fn render_json(&self) -> Option<String> {
        let name = self.name;
        match &self.json {
            JsonInstance::None => None,
            JsonInstance::Transparent => {
                let TypeShape::Identity { hs_binder, .. } = &self.shape else {
                    panic!("{name}: JsonInstance::Transparent needs TypeShape::Identity");
                };
                Some(format!(
                    "instance ToJSON {name} where toJSON ({name} {hs_binder}) = toJSON {hs_binder}"
                ))
            }
            JsonInstance::ShownString { binder } => Some(format!(
                "instance ToJSON {name} where toJSON {binder} = toJSON (show {binder})"
            )),
            JsonInstance::Object { binder, keys } => {
                let TypeShape::Record { fields } = &self.shape else {
                    panic!("{name}: JsonInstance::Object needs TypeShape::Record");
                };
                let pairs: Vec<String> = keys
                    .iter()
                    .map(|(key, field)| {
                        assert!(
                            fields.iter().any(|f| f.hs_name == *field),
                            "{name}: ToJSON key {key:?} names field {field:?}, \
                             which the record does not declare"
                        );
                        format!("\"{key}\" .= {binder}.{field}")
                    })
                    .collect();
                Some(format!(
                    "instance ToJSON {name} where toJSON {binder} = object [{}]",
                    pairs.join(", ")
                ))
            }
        }
    }

    /// Every problem with this declaration, as a list so one run reports all.
    ///
    /// A violation is a GENERATION failure by design: a malformed declaration
    /// must never reach the emitters.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let name = self.name;
        let mut errs = self.derives.validate(name);

        match &self.shape {
            TypeShape::Record { fields } => {
                if fields.is_empty() {
                    errs.push(format!("{name}: a record with no fields is not a record"));
                }
                errs.extend(validate_record_fields(name, None, fields));
            }
            TypeShape::Sum { variants } => {
                if variants.is_empty() {
                    errs.push(format!("{name}: a sum with no variants is uninhabited"));
                }
                let mut seen: Vec<&str> = Vec::new();
                let mut hs_fields: Vec<(&str, &HsType)> = Vec::new();
                for v in variants {
                    if seen.contains(&v.ctor) {
                        errs.push(format!("{name}: two variants named `{}`", v.ctor));
                    }
                    seen.push(v.ctor);
                    if let VariantFields::Named(fields) = &v.fields {
                        if fields.is_empty() {
                            errs.push(format!(
                                "{name}.{}: a named constructor must have at least one field",
                                v.ctor
                            ));
                        }
                        errs.extend(validate_record_fields(name, Some(v.ctor), fields));
                        for field in fields {
                            if let Some((_, prior_ty)) = hs_fields
                                .iter()
                                .find(|(field_name, _)| *field_name == field.hs_name)
                            {
                                if *prior_ty != &field.ty {
                                    errs.push(format!(
                                        "{name}: record field `{}` has different types across variants",
                                        field.hs_name
                                    ));
                                }
                            } else {
                                hs_fields.push((field.hs_name, &field.ty));
                            }
                        }
                    }
                }
            }
            TypeShape::Identity {
                validation,
                payload,
                ..
            } => {
                if matches!(payload, IdentityPayload::Int) && *validation != Validation::None {
                    errs.push(format!(
                        "{name}: an Int identity carries no string policy \
                         (got {validation:?})"
                    ));
                }
            }
        }

        match (&self.json, &self.shape) {
            (JsonInstance::None, _)
            | (JsonInstance::Transparent, TypeShape::Identity { .. })
            | (JsonInstance::ShownString { .. }, TypeShape::Sum { .. })
            | (JsonInstance::Object { .. }, TypeShape::Record { .. }) => {}
            (JsonInstance::Transparent, _) => {
                errs.push(format!(
                    "{name}: Transparent ToJSON needs an Identity shape"
                ));
            }
            (JsonInstance::ShownString { .. }, _) => {
                errs.push(format!("{name}: ShownString ToJSON needs a Sum shape"));
            }
            (JsonInstance::Object { .. }, _) => {
                errs.push(format!("{name}: Object ToJSON needs a Record shape"));
            }
        }

        if let (JsonInstance::Object { keys, .. }, TypeShape::Record { fields }) =
            (&self.json, &self.shape)
        {
            for (key, field) in *keys {
                if !fields.iter().any(|f| f.hs_name == *field) {
                    errs.push(format!(
                        "{name}: ToJSON key `{key}` names field `{field}`, \
                         which the record does not declare"
                    ));
                }
            }
            if keys.len() != fields.len() {
                errs.push(format!(
                    "{name}: ToJSON lists {} of {} fields — a partial object is a \
                     silent omission, spell every field",
                    keys.len(),
                    fields.len()
                ));
            }
        }

        if let Some(d) = &self.domain {
            errs.extend(d.validate(name, &self.shape));
        }

        errs
    }
}

/// What kind of declaration a [`TypeDef`] is.
#[derive(Clone, Debug)]
pub enum TypeShape {
    /// `data X = X { f :: T, … }` — a record. The field `Vec` order IS the wire
    /// contract, on both sides at once.
    Record {
        /// Fields in wire order.
        fields: Vec<RecordField>,
    },
    /// `data X = A | B T | C { field :: U }` — a sum.
    Sum {
        /// Variants in declaration order.
        variants: Vec<SumVariant>,
    },
    /// `data X = X Text` — an identity type.
    ///
    /// `data`, not `newtype` and not a synonym: a synonym would let a `GitOid`
    /// be passed where a `WorktreeId` is wanted, and a `newtype` is erased in
    /// Core so the Rust `ToCore` side would build a one-field `Con` the Haskell
    /// side no longer has.
    ///
    /// This is the shape that mints a Rust newtype WITH a fallible boundary
    /// constructor. A bare `Int`/`Text` FIELD inside a
    /// record is deliberately NOT promoted: promoting it would change the
    /// Haskell declaration, and that declaration is byte-locked by the Class A
    /// goldens.
    Identity {
        /// `Text` or `Int`.
        payload: IdentityPayload,
        /// The pattern variable the Haskell side binds when unwrapping
        /// (`toJSON (WorktreeId t) = toJSON t`). Carried rather than
        /// normalized because the live instances use different binders and the
        /// goldens are byte-locked. A binder is data, not source.
        hs_binder: &'static str,
        /// The Rust struct's single field name (`raw`).
        rust_field: &'static str,
        /// What the boundary constructor enforces.
        validation: Validation,
    },
}

/// The payload of a [`TypeShape::Identity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPayload {
    /// `data X = X Text` — the id is a string.
    Text,
    /// `data X = X Int` — the id is an integer (Event's `EventId`, Subagent's
    /// `AgentId`; not exercised by the Worktree slice).
    Int,
}

impl IdentityPayload {
    /// The payload as a Haskell type.
    #[must_use]
    pub fn hs(self) -> HsType {
        match self {
            IdentityPayload::Text => HsType::Text,
            IdentityPayload::Int => HsType::Int,
        }
    }

    /// The payload as a Rust type.
    #[must_use]
    pub fn rust(self) -> &'static str {
        match self {
            IdentityPayload::Text => "String",
            IdentityPayload::Int => "i64",
        }
    }
}

/// One field of a [`TypeShape::Record`].
///
/// The Haskell name and the Rust name are BOTH here, in one struct, in one
/// ordered `Vec`. That is the deliverable: the two spellings of a field are the
/// same datum, so the positional invariant is untrue-by-construction rather
/// than asserted by comment.
#[derive(Clone, Debug)]
pub struct RecordField {
    /// The Haskell field name (`specDirtyPolicy`).
    pub hs_name: &'static str,
    /// The Rust wire field name (`spec_dirty_policy`).
    pub rust_name: &'static str,
    /// Its Haskell type. The Rust type is resolved from this against the
    /// effect's own `type_defs` — a `Named` type's Rust spelling is that
    /// declaration's [`TypeDef::wire_name`], never a second string.
    pub ty: HsType,
    /// Rust doc-comment lines for the field, WITHOUT their `/// ` prefix.
    pub doc: &'static [&'static str],
}

/// The payload shape of one sum constructor.
#[derive(Clone, Debug)]
pub enum VariantFields {
    /// `Ctor A B` / `Ctor(A, B)`.
    Positional(Vec<HsType>),
    /// `Ctor { field :: A }` / `Ctor { field: A }`.
    Named(Vec<RecordField>),
}

impl VariantFields {
    /// Whether this constructor has no payload.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Positional(fields) => fields.is_empty(),
            Self::Named(fields) => fields.is_empty(),
        }
    }

    /// Its payload types in Core constructor order.
    #[must_use]
    pub fn types(&self) -> Vec<&HsType> {
        match self {
            Self::Positional(fields) => fields.iter().collect(),
            Self::Named(fields) => fields.iter().map(|field| &field.ty).collect(),
        }
    }
}

/// One variant of a [`TypeShape::Sum`].
#[derive(Clone, Debug)]
pub struct SumVariant {
    /// The constructor name. Identical on both sides — a Haskell data
    /// constructor IS the Rust variant name, which is why an enum needs no
    /// `#[core(name)]`.
    pub ctor: &'static str,
    /// Constructor payload fields, in Core order.
    pub fields: VariantFields,
    /// Rust doc-comment lines for the variant, WITHOUT their `/// ` prefix.
    pub doc: &'static [&'static str],
}

fn validate_record_fields(
    type_name: &str,
    variant: Option<&str>,
    fields: &[RecordField],
) -> Vec<String> {
    let owner = variant.map_or_else(
        || type_name.to_string(),
        |variant| format!("{type_name}.{variant}"),
    );
    let mut errors = Vec::new();
    let mut hs_seen: Vec<&str> = Vec::new();
    let mut rust_seen: Vec<&str> = Vec::new();
    for field in fields {
        if hs_seen.contains(&field.hs_name) {
            errors.push(format!("{owner}: two fields named `{}`", field.hs_name));
        }
        if rust_seen.contains(&field.rust_name) {
            errors.push(format!(
                "{owner}: two Rust fields named `{}`",
                field.rust_name
            ));
        }
        hs_seen.push(field.hs_name);
        rust_seen.push(field.rust_name);
    }
    errors
}

// ---------------------------------------------------------------------------
// ToJSON
// ---------------------------------------------------------------------------

/// Which `ToJSON` instance a [`TypeDef`] emits.
///
/// These instances are NEEDED: an effect's `errors` block templates a `ToJSON`
/// for its error ADT, so every type reachable from an error field needs one, and
/// the vendored generic default only covers single-constructor records. They
/// were also the largest surviving raw-Haskell hatch in the registry. Four
/// shapes cover every one of them, so they are policy rather than source.
#[derive(Clone, Debug)]
pub enum JsonInstance {
    /// No instance emitted.
    None,
    /// `toJSON (X t) = toJSON t` — the id, not a wrapper object. A receipt
    /// reader wants `"wt-3f9"`, not `{"raw": "wt-3f9"}`.
    Transparent,
    /// `toJSON k = toJSON (show k)` — a nullary-variant sum as its constructor
    /// name.
    ShownString {
        /// The pattern variable bound in the instance body.
        binder: &'static str,
    },
    /// `toJSON r = object ["k" .= r.f, …]`.
    ///
    /// The key map is DATA, so a deliberate rename (`gitArgs` → `"args"`, so
    /// the JSON reads as a git receipt rather than as a struct dump) is visible
    /// in the schema instead of buried in a string. Every field must appear —
    /// see [`TypeDef::validate`]; a partial object is a silent omission.
    Object {
        /// The pattern variable bound in the instance body.
        binder: &'static str,
        /// `(json_key, haskell_field)` pairs, in emission order.
        keys: &'static [(&'static str, &'static str)],
    },
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// What an [`TypeShape::Identity`]'s fallible boundary constructor enforces.
///
/// The requirement: wire-side integers and identifiers generate as
/// newtypes with fallible boundary constructors — decode once at the edge, typed
/// everywhere after. This is the "decode once" policy, as data.
///
/// Tightening one is a schema edit with a test, not a code change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validation {
    /// Anything is accepted; the constructor is infallible.
    None,
    /// Non-empty. The weakest policy that is actually true today for
    /// `GitOid`/`GitRef`/`BranchName`.
    NonEmpty,
    /// A single path component: non-empty, at most `max_len` BYTES, and every
    /// byte ascii-alphanumeric or in `extra_allowed`.
    ///
    /// Byte-oriented rather than char-oriented deliberately: this mirrors
    /// `tidepool_worktree::WorktreeId::is_path_safe`, which is byte-oriented
    /// because the value is joined into a filesystem path as one component.
    /// A cross-check test guards the duplication this creates.
    Segment {
        /// Maximum length in bytes.
        max_len: usize,
        /// Bytes allowed beyond ascii-alphanumeric.
        extra_allowed: &'static str,
    },
}

impl Validation {
    /// Is the generated boundary constructor fallible?
    #[must_use]
    pub fn is_fallible(self) -> bool {
        !matches!(self, Validation::None)
    }
}

// ---------------------------------------------------------------------------
// Derives
// ---------------------------------------------------------------------------

/// One derive on a Rust wire type. A CLOSED set — not a string list, so a typo
/// is a compile error and the emission order is not a per-site decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDerive {
    /// Serialization at private native transport boundaries.
    Serialize,
    /// Deserialization at private native transport boundaries.
    Deserialize,
    /// `tidepool_bridge_derive::ToCore` — Rust value out to Core.
    ToCore,
    /// `tidepool_bridge_derive::FromCore` — Core value in to Rust.
    FromCore,
    /// `Clone`.
    Clone,
    /// `Copy`.
    Copy,
    /// `Debug`.
    Debug,
    /// `Default`.
    Default,
    /// `PartialEq`.
    PartialEq,
    /// `Eq`.
    Eq,
    /// `PartialOrd`.
    PartialOrd,
    /// `Ord`.
    Ord,
    /// `Hash`.
    Hash,
}

impl WireDerive {
    /// Its position in the canonical emission order.
    ///
    /// Bridge derives first (they are the wire contract), then the std traits in
    /// the order the hand-written families already spell them.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            WireDerive::ToCore => 0,
            WireDerive::FromCore => 1,
            WireDerive::Clone => 2,
            WireDerive::Copy => 3,
            WireDerive::Debug => 4,
            WireDerive::Default => 5,
            WireDerive::PartialEq => 6,
            WireDerive::Eq => 7,
            WireDerive::PartialOrd => 8,
            WireDerive::Ord => 9,
            WireDerive::Hash => 10,
            WireDerive::Serialize => 11,
            WireDerive::Deserialize => 12,
        }
    }

    /// The identifier as spelled in a `#[derive(…)]` list.
    #[must_use]
    pub fn ident(self) -> &'static str {
        match self {
            WireDerive::ToCore => "ToCore",
            WireDerive::FromCore => "FromCore",
            WireDerive::Clone => "Clone",
            WireDerive::Copy => "Copy",
            WireDerive::Debug => "Debug",
            WireDerive::Default => "Default",
            WireDerive::PartialEq => "PartialEq",
            WireDerive::Eq => "Eq",
            WireDerive::PartialOrd => "PartialOrd",
            WireDerive::Ord => "Ord",
            WireDerive::Hash => "Hash",
            WireDerive::Serialize => "serde::Serialize",
            WireDerive::Deserialize => "serde::Deserialize",
        }
    }
}

/// The derive set on one wire type.
///
/// Emission order is CANONICAL ([`WireDerive::rank`]), not the order written
/// here: the set is a set, and the rendered line must not depend on how someone
/// happened to list it. That canonical order reproduces every derive spelling
/// the hand-written `Wt*` block uses today — which is the check that it is the
/// right order and not merely a consistent one.
#[derive(Clone, Copy, Debug)]
pub struct WireDerives(pub &'static [WireDerive]);

impl WireDerives {
    /// Is `d` in the set?
    #[must_use]
    pub fn has(self, d: WireDerive) -> bool {
        self.0.contains(&d)
    }

    /// The rendered attribute: `#[derive(ToCore, FromCore, Clone, Debug, …)]`.
    #[must_use]
    pub fn render(self) -> String {
        let mut ds: Vec<WireDerive> = self.0.to_vec();
        ds.sort_by_key(|d| d.rank());
        let idents: Vec<&str> = ds.iter().map(|d| d.ident()).collect();
        format!("#[derive({})]", idents.join(", "))
    }

    /// Structural problems with the set.
    #[must_use]
    pub fn validate(self, whose: &str) -> Vec<String> {
        let mut errs = Vec::new();
        for (i, d) in self.0.iter().enumerate() {
            if self.0[..i].contains(d) {
                errs.push(format!("{whose}: derive `{}` listed twice", d.ident()));
            }
        }
        // Each of these is a compile error in the EMITTED file, which is a much
        // worse place to learn about it than here.
        for (need, implied_by) in [
            (WireDerive::Clone, WireDerive::Copy),
            (WireDerive::PartialEq, WireDerive::Eq),
            (WireDerive::PartialOrd, WireDerive::Ord),
            (WireDerive::Eq, WireDerive::Ord),
        ] {
            if self.has(implied_by) && !self.has(need) {
                errs.push(format!(
                    "{whose}: `{}` requires `{}`",
                    implied_by.ident(),
                    need.ident()
                ));
            }
        }
        errs
    }
}

// ---------------------------------------------------------------------------
// Domain mapping
// ---------------------------------------------------------------------------

/// The richer domain type a wire type mirrors, and how each direction of the
/// conversion is produced.
///
/// The generated/hand-written split is a schema FIELD, so it is visible rather
/// than inferred. Today that split exists only in a reader's head; recording it
/// is what lets a later lane see, without re-deriving it, which conversions it
/// may safely regenerate.
#[derive(Clone, Debug)]
pub struct DomainMap {
    /// The Rust path of the domain type, resolved at the CONSUMING crate
    /// (`tidepool_worktree::WorktreeId`). A path, never Haskell source — the
    /// same pressure valve `RustBinding::Path` already is, and the reason the
    /// schema crate can stay a leaf while the adapters generate into
    /// `tidepool-handlers`.
    pub domain_path: &'static str,
    /// domain → wire, or `None` when no such conversion exists today.
    pub into_wire: Option<AdapterKind>,
    /// wire → domain, or `None` when no such conversion exists today.
    pub from_wire: Option<AdapterKind>,
}

impl DomainMap {
    /// The domain type's last path segment, used as the imported short name.
    ///
    /// # Panics
    /// Panics on an empty `domain_path`.
    #[must_use]
    pub fn domain_ident(&self) -> &'static str {
        #[allow(clippy::expect_used, reason = "a domain path has at least one segment")]
        self.domain_path
            .rsplit("::")
            .next()
            .expect("a domain path has at least one segment")
    }

    /// Structural problems with this mapping, given the shape it describes.
    #[must_use]
    pub fn validate(&self, whose: &str, shape: &TypeShape) -> Vec<String> {
        let mut errs = Vec::new();
        if !self.domain_path.contains("::") {
            errs.push(format!(
                "{whose}: domain_path `{}` must be a crate-qualified Rust path",
                self.domain_path
            ));
        }
        for (dir, kind) in [
            ("into_wire", &self.into_wire),
            ("from_wire", &self.from_wire),
        ] {
            let Some(kind) = kind else { continue };
            match (kind, shape) {
                (AdapterKind::IdentityRaw { .. }, TypeShape::Identity { .. })
                | (AdapterKind::VariantMap(_), TypeShape::Sum { .. })
                | (AdapterKind::HandWritten(_), _) => {}
                (AdapterKind::IdentityRaw { .. }, _) => errs.push(format!(
                    "{whose}: {dir} IdentityRaw needs an Identity shape"
                )),
                (AdapterKind::VariantMap(_), _) => {
                    errs.push(format!("{whose}: {dir} VariantMap needs a Sum shape"));
                }
            }
            if let (AdapterKind::VariantMap(pairs), TypeShape::Sum { variants }) = (kind, shape) {
                // A VariantMap must be a true bijection against the declared wire
                // variants: every domain name appears once, every wire constructor
                // is targeted exactly once, and no wire constructor is left out.
                // Count-only and existence-only checks both pass a map that hits
                // one wire constructor twice while missing another entirely — the
                // generator then emits a duplicate `match` arm and no arm at all
                // for the missed variant, a non-exhaustive match reaching Rust.
                let mut domain_seen: Vec<&str> = Vec::new();
                for (domain, _) in *pairs {
                    if domain_seen.contains(domain) {
                        errs.push(format!(
                            "{whose}: {dir} VariantMap names domain variant `{domain}` twice"
                        ));
                    }
                    domain_seen.push(domain);
                }

                let mut wire_seen: Vec<&str> = Vec::new();
                for (_, wire) in *pairs {
                    if !variants.iter().any(|v| v.ctor == *wire) {
                        errs.push(format!(
                            "{whose}: {dir} VariantMap names wire variant `{wire}`, \
                             which this type does not declare"
                        ));
                    } else if wire_seen.contains(wire) {
                        errs.push(format!(
                            "{whose}: {dir} VariantMap maps wire variant `{wire}` twice — \
                             the generated `match` would have a duplicate arm"
                        ));
                    }
                    wire_seen.push(wire);
                }

                for v in variants {
                    if !wire_seen.contains(&v.ctor) {
                        errs.push(format!(
                            "{whose}: {dir} VariantMap does not cover wire variant `{}` — \
                             the generated `match` would be non-exhaustive",
                            v.ctor
                        ));
                    }
                }
            }
            if let AdapterKind::HandWritten(reason) = kind {
                if reason.trim().is_empty() {
                    errs.push(format!(
                        "{whose}: {dir} HandWritten must record WHY — an empty reason is \
                         the comment this schema exists to replace"
                    ));
                }
            }
        }
        errs
    }
}

/// How one direction of a domain↔wire conversion is produced.
#[derive(Clone, Debug)]
pub enum AdapterKind {
    /// Newtype raw ↔ newtype raw: `X { raw: v.as_str().to_string() }` one way,
    /// `X::from_raw(v.raw.clone())` the other. Names the domain accessor and
    /// constructor so a domain crate that spells them differently still fits.
    IdentityRaw {
        /// The domain accessor yielding the payload (`as_str`).
        as_str: &'static str,
        /// The domain constructor taking the payload (`from_raw`).
        from_raw: &'static str,
    },
    /// A total `match` between two variant vocabularies, as
    /// `(domain_variant, wire_variant)` pairs.
    ///
    /// This is the mechanical-but-error-prone case the generator earns its keep
    /// on: `InProgressKind` renames EVERY variant (`Merge` →
    /// `InProgressMerge`), and a hand-written ten-arm map with two adjacent
    /// renames is exactly where a transposition hides.
    VariantMap(&'static [(&'static str, &'static str)]),
    /// NOT generated, and why.
    ///
    /// Not decoration. The reason is emitted as a comment in the generated
    /// file, so the file itself says which conversions carry a decision.
    HandWritten(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_field(hs_name: &'static str, rust_name: &'static str, ty: HsType) -> RecordField {
        RecordField {
            hs_name,
            rust_name,
            ty,
            doc: &[],
        }
    }

    fn sum_shape(ctors: &[&'static str]) -> TypeShape {
        TypeShape::Sum {
            variants: ctors
                .iter()
                .map(|c| SumVariant {
                    ctor: c,
                    fields: VariantFields::Positional(vec![]),
                    doc: &[],
                })
                .collect(),
        }
    }

    fn variant_map(pairs: &'static [(&'static str, &'static str)]) -> DomainMap {
        DomainMap {
            domain_path: "some_crate::Domain",
            into_wire: None,
            from_wire: Some(AdapterKind::VariantMap(pairs)),
        }
    }

    #[test]
    fn variant_map_bijection_is_clean() {
        let shape = sum_shape(&["X", "Y"]);
        let map = variant_map(&[("A", "X"), ("B", "Y")]);
        assert!(map.validate("Test", &shape).is_empty());
    }

    #[test]
    fn variant_map_missing_target_is_reported() {
        let shape = sum_shape(&["X", "Y"]);
        let map = variant_map(&[("A", "X")]);
        let errs = map.validate("Test", &shape);
        assert!(
            errs.iter()
                .any(|e| e.contains("does not cover wire variant `Y`")),
            "{errs:?}"
        );
    }

    #[test]
    fn variant_map_duplicate_target_is_reported() {
        let shape = sum_shape(&["X", "Y"]);
        let map = variant_map(&[("A", "X"), ("B", "X")]);
        let errs = map.validate("Test", &shape);
        assert!(
            errs.iter()
                .any(|e| e.contains("maps wire variant `X` twice")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("does not cover wire variant `Y`")),
            "{errs:?}"
        );
    }

    #[test]
    fn variant_map_duplicate_domain_name_is_reported() {
        let shape = sum_shape(&["X", "Y"]);
        let map = variant_map(&[("A", "X"), ("A", "Y")]);
        let errs = map.validate("Test", &shape);
        assert!(
            errs.iter()
                .any(|e| e.contains("names domain variant `A` twice")),
            "{errs:?}"
        );
    }

    #[test]
    fn named_sum_fields_render_as_haskell_record_constructors() {
        let type_def = TypeDef {
            name: "Head",
            wire_rust: None,
            core_module: None,
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "Attached",
                        fields: VariantFields::Named(vec![
                            named_field("branch", "branch", HsType::Text),
                            named_field("oid", "oid", HsType::Text),
                        ]),
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "Detached",
                        fields: VariantFields::Named(vec![named_field("oid", "oid", HsType::Text)]),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WireDerives(&[]),
            domain: None,
            doc: &[],
        };

        assert_eq!(
            type_def.render_decl(),
            "data Head = Attached { branch :: Text, oid :: Text } | Detached { oid :: Text } deriving (Show, Eq)"
        );
        assert!(type_def.validate().is_empty());
    }

    #[test]
    fn named_sum_field_types_must_agree_across_constructors() {
        let shape = TypeShape::Sum {
            variants: vec![
                SumVariant {
                    ctor: "Textual",
                    fields: VariantFields::Named(vec![named_field(
                        "payload",
                        "payload",
                        HsType::Text,
                    )]),
                    doc: &[],
                },
                SumVariant {
                    ctor: "Numeric",
                    fields: VariantFields::Named(vec![named_field(
                        "payload",
                        "payload",
                        HsType::Int,
                    )]),
                    doc: &[],
                },
            ],
        };
        let type_def = TypeDef {
            name: "Payload",
            wire_rust: None,
            core_module: None,
            shape,
            json: JsonInstance::None,
            derives: WireDerives(&[]),
            domain: None,
            doc: &[],
        };

        assert!(type_def
            .validate()
            .iter()
            .any(|error| error.contains("different types across variants")));
    }

    #[test]
    fn empty_named_sum_constructor_is_rejected() {
        let type_def = TypeDef {
            name: "Empty",
            wire_rust: None,
            core_module: None,
            shape: TypeShape::Sum {
                variants: vec![SumVariant {
                    ctor: "Empty",
                    fields: VariantFields::Named(vec![]),
                    doc: &[],
                }],
            },
            json: JsonInstance::None,
            derives: WireDerives(&[]),
            domain: None,
            doc: &[],
        };

        assert!(type_def
            .validate()
            .iter()
            .any(|error| error.contains("must have at least one field")));
    }
}
