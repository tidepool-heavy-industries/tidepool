//! The effect schema — plain data describing one effect completely.
//!
//! This is the schema's single source of truth. Everything an effect's
//! generated artifacts need is here as structured data: no raw Haskell, no raw
//! Rust bodies, no field-order-by-comment.
//!
//! Two annotation slots ([`Verb::handling`] and [`Verb::extract`]) exist for
//! LATER phases. They are required fields, so a verb cannot be added without
//! answering them: a verb missing a handling-class annotation fails
//! generation, not runtime — but no phase-1 generator reads them yet.

use crate::hs::{render_signature, HsType};
pub use crate::types::{
    AdapterKind, DomainMap, IdentityPayload, JsonInstance, RecordField, SumVariant, TypeDef,
    TypeShape, Validation, WireDerive, WireDerives,
};

// ---------------------------------------------------------------------------
// Effect
// ---------------------------------------------------------------------------

/// One effect, completely.
#[derive(Clone, Debug)]
pub struct Effect {
    /// Haskell GADT type name, and the Rust-side prefix: `"Exec"`.
    pub name: &'static str,
    /// The hand-written handler struct in `tidepool-handlers`: `"ExecHandler"`.
    pub handler: &'static str,
    /// The snake_case module `handler` lives in under
    /// `tidepool-handlers/src/handlers/` — usually [`Self::name`]'s own
    /// snake_case, but not always: `RepoEvent`'s hand-authored module is
    /// `handlers::event` (matching `Tidepool.Event`/`event_decl`, the family
    /// name used everywhere ELSE in the registry), not `handlers::repo_event`
    /// (the GADT's own name — `RepoEvent` rather than `Event` only because
    /// `Event a` is a separate authored description type). Carried as DATA
    /// rather than derived, the same reason [`crate::types::TypeDef::wire_rust`]
    /// is: a convention that breaks once is a convention the schema should not
    /// re-derive.
    pub handler_module: &'static str,
    /// The generated request enum: `"ExecReq"`.
    pub req_enum: &'static str,
    /// The generated `EffectDecl` builder: `"exec_decl"`.
    pub decl_fn: &'static str,
    /// The long-form description, concatenated with NO separator (matching the
    /// `concat!` the hand-written registry uses, so a description split across
    /// source lines keeps its exact bytes).
    pub description: &'static [&'static str],
    /// The compact per-turn card, or `None` to fall back to `description`.
    pub prompt_card: Option<&'static [&'static str]>,
    /// Type parameters the GADT head carries before its result parameter.
    pub type_params: &'static [&'static str],
    /// Row arguments a compile that supplies none falls back to.
    pub default_row_args: &'static [&'static str],
    /// Do this effect's helpers typecheck against any row carrying `Member`?
    pub helpers_row_polymorphic: bool,
    /// Companion `import` lines this effect's helpers need beyond the fixed
    /// eval surface. These reach the eval preamble and the persistent declaration
    /// environment, NOT the generated `Tidepool.Effects` module.
    pub extra_imports: &'static [&'static str],
    /// Supporting Haskell declarations emitted before the GADT. The error ADT
    /// is NOT listed here — it is derived from [`Effect::errors`].
    pub type_defs: Vec<TypeDef>,
    /// `(Haskell name, Rust wire name)` pairs for a NAMED type this effect's own
    /// `type_defs` reference but which is declared by ANOTHER, already-migrated
    /// effect (Event's `Watch` names Worktree's `WorktreeId`). All Haskell
    /// declarations still land in the ONE generated `Tidepool.Effects` module
    /// regardless of which effect owns them, so the Haskell side needs no
    /// change — this table exists only so [`Effect::wire_rust_of`] (and
    /// [`Effect::validate`]'s undeclared-reference check) can resolve a WIRE
    /// Rust spelling this effect does not itself own. Kept effect-local (no
    /// `Vec<Effect>` threaded through the generator) rather than a
    /// whole-registry lookup, because the pairing is small and the owning
    /// effect's wire name is already public, stable data.
    pub foreign_types: &'static [(&'static str, &'static str)],
    /// This effect's typed per-verb failure ADT (#335), if it has one.
    pub errors: Option<ErrorAdt>,
    /// The GADT constructors, one per verb.
    pub verbs: Vec<Verb>,
    /// The thin send-wrapper surface authored code actually calls.
    pub helpers: Vec<Helper>,
    /// How this effect's row admits a type bound at the invocation site
    /// (`fork @T`, `finalize @T x`, `runLLMTurn @T`) — #20 steps 2-3's design
    /// gate, decided explicit-per-effect data (not inferred from an
    /// unconstrained [`HsType::Var`] turning up in some verb's `ret`) for the
    /// same reason [`Verb::handling`]/[`Verb::extract`] are required fields
    /// rather than shape-sniffed: a verb's polymorphism kind is answered once,
    /// at the definition, not re-derived by a renderer guessing from shape.
    /// See [`Polymorphism`]'s own variants for the two real shapes this
    /// migration found.
    pub polymorphism: Polymorphism,
    /// Does this effect have a real `tidepool-handlers` `EffectHandler` that
    /// dispatches its verbs? `true` for an ordinary base effect (`Exec`,
    /// `Worktree`, …) — [`crate::gen::all_files`] emits `handler_rs`/`wire_rs`/
    /// `adapter_rs` output for it into `tidepool-handlers`/
    /// `tidepool-bridge-effects`. `false` for a SUSPENDING effect (`AskUser`,
    /// `ReadState`, `Fork`, `Finalize`, `RunLlmTurn`, `Green`, `Ask`) that
    /// never reaches an `EffectHandler` — the harness/driver services it
    /// directly (see [`crate::effects::suspension_roster`]) — for which
    /// generating handler/wire/adapter glue would be actively wrong: there is
    /// no handler struct for it to dispatch into. Such an effect can still
    /// contribute its DECL text (`decl_rs`) once its helpers are fully
    /// schema-representable, entirely independent of this flag.
    pub dispatched: bool,
}

/// How an effect's row admits a type bound at the invocation site — #20
/// steps 2-3's design gate (Option B: explicit per-effect data).
///
/// The two variants are the two shapes this migration actually found, not a
/// speculative menu: [`Effect::validate`] checks each against the effect's own
/// `type_params`/verb shapes, so a mismatch (e.g. an `ArgBound` tyvar absent
/// from `type_params`) fails at generation time, the same discipline
/// [`Verb::errors`] tagging already gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polymorphism {
    /// Every verb's result is monomorphic; nothing binds at an invocation
    /// site. The default for almost every effect, including ones whose
    /// SURFACE helpers (not yet schema-represented) are `@T`-polymorphic one
    /// layer down — `Fork`/`RunLlmTurn`/`Green`'s own GADT constructors all
    /// return a concrete type (`Value`, or a verb-local `Int`/`()`); their
    /// `@T` binding lives entirely in a `*Sited` substrate helper's signature,
    /// not in the row/GADT this variant describes.
    None,
    /// The invocation-bound type variable is a REAL FIELD of at least one
    /// constructor, and a real, applied parameter of the GADT head — `v` in
    /// `data Finalize v a where FinalizeWith :: Int -> v -> Finalize v a`.
    /// The ROW ENTRY itself is the constraint (`Member (Finalize T) effs`),
    /// exactly the shape `State s` uses — `finalize @T` type-checks because
    /// the row was built with `Finalize T` in it, not because of anything
    /// verb-local.
    ArgBound {
        /// The type parameter's name, as it appears in `type_params` and in
        /// the field(s)/`ret` that use it (`"v"`).
        tyvar: &'static str,
    },
    /// The invocation-bound type variable is a PHANTOM at the GADT level: it
    /// would appear in the GADT's own type parameter list and in a verb's
    /// `ret`, but never in a constructor FIELD — reserved for an effect whose
    /// constructor genuinely returns the bare tyvar (no wrapping `Value`/
    /// concrete type to marshal through). No effect in this schema uses this
    /// shape yet (`Fork`/`RunLlmTurn`'s constructors return concrete `Value`,
    /// not a bare `a` — their polymorphism lives in a deferred substrate
    /// helper, not the GADT), but the variant is named now so a later
    /// migration of those helpers has a home to bind to rather than inventing
    /// one under time pressure.
    ResultBound {
        /// The type parameter's name (`"a"`).
        tyvar: &'static str,
    },
}

impl Effect {
    /// The `description` field as one string.
    #[must_use]
    pub fn description_text(&self) -> String {
        self.description.concat()
    }

    /// The `prompt_card` field as one string, if present.
    #[must_use]
    pub fn prompt_card_text(&self) -> Option<String> {
        self.prompt_card.map(<[&str]>::concat)
    }

    /// Look up a verb by constructor name.
    #[must_use]
    pub fn verb(&self, ctor: &str) -> Option<&Verb> {
        self.verbs.iter().find(|v| v.ctor == ctor)
    }

    /// The GADT head with its type parameters applied: `Exec`, `Finalize v`.
    #[must_use]
    pub fn head(&self) -> String {
        let mut out = String::from(self.name);
        for p in self.type_params {
            out.push(' ');
            out.push_str(p);
        }
        out
    }

    /// Every rendered constructor signature, in verb order.
    #[must_use]
    pub fn constructor_signatures(&self) -> Vec<String> {
        self.verbs
            .iter()
            .map(|v| {
                let args: Vec<HsType> = v.args.iter().map(|a| a.ty.clone()).collect();
                format!(
                    "{} :: {}",
                    v.ctor,
                    render_signature(&args, &self.head(), &v.result_type())
                )
            })
            .collect()
    }

    /// Look up a supporting type declaration by its HASKELL name.
    #[must_use]
    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.type_defs.iter().find(|t| t.name == name)
    }

    /// The Rust wire spelling of a Haskell type appearing in this effect's own
    /// `type_defs`.
    ///
    /// This is the ONLY place a `Named` field type acquires a Rust name: the
    /// declaration that defines it is asked. There is no second string to keep
    /// in step, which is the same property [`RecordField`] gives a field name.
    ///
    /// # Panics
    /// Panics when `name` is neither declared by this effect nor listed in its
    /// [`Effect::foreign_types`]. That is a generation-time failure by design —
    /// an under-specified schema must not produce output.
    #[must_use]
    pub fn wire_rust_of(&self, name: &str) -> &'static str {
        if let Some(td) = self.type_def(name) {
            return td.wire_name();
        }
        if let Some((_, wire)) = self.foreign_types.iter().find(|(hs, _)| *hs == name) {
            return wire;
        }
        panic!(
            "{}: no type_defs entry (own or foreign_types) declares `{name}`, so it has \
             no wire Rust spelling. A type from another mechanism (a `CoreRecord` bridged \
             record) cannot appear in a generated wire struct.",
            self.name
        )
    }

    /// Walk a field chain through this effect's `type_defs` and return the type
    /// it lands on — the derivation behind [`HelperBody::Projection`].
    ///
    /// Same resolution `wire_rust_of` performs for a `Named` field's Rust
    /// spelling, one step further: the declaration that DECLARES
    /// a type is the only thing asked what its fields are typed. Nothing is
    /// restated, so nothing can drift.
    ///
    /// # Errors
    /// Returns a message naming the exact step that failed — a non-record in the
    /// middle of the chain, a type this effect does not declare, or a field the
    /// declaring `TypeDef` does not have.
    pub fn project(&self, start: &HsType, fields: &[&str]) -> Result<HsType, String> {
        let mut cur = start.clone();
        for (i, f) in fields.iter().enumerate() {
            let HsType::Named(owner) = cur else {
                return Err(format!(
                    "{}: field `{f}` (step {}) projects out of `{}`, which is not a named type",
                    self.name,
                    i + 1,
                    cur.render()
                ));
            };
            let Some(td) = self.type_def(owner) else {
                return Err(format!(
                    "{}: field `{f}` (step {}) projects out of `{owner}`, which this effect \
                     does not declare",
                    self.name,
                    i + 1
                ));
            };
            let TypeShape::Record { fields: rec } = &td.shape else {
                return Err(format!(
                    "{}: field `{f}` (step {}) projects out of `{owner}`, which is not a record",
                    self.name,
                    i + 1
                ));
            };
            let Some(rf) = rec.iter().find(|rf| rf.hs_name == *f) else {
                return Err(format!(
                    "{}: `{owner}` has no field `{f}` (step {})",
                    self.name,
                    i + 1
                ));
            };
            cur = rf.ty.clone();
        }
        Ok(cur)
    }

    /// Every rendered `type_defs` entry, in emission order: every shape
    /// declaration in schema order, then every `ToJSON` instance in schema
    /// order, then the derived error ADT.
    ///
    /// That order is not a preference — it is the order the hand-written
    /// projection already emits, and the Class A `effect_decls.txt` golden is
    /// what proves it. An effect with empty `type_defs` (Exec, Journal) renders
    /// exactly as before: the error ADT alone, or nothing.
    #[must_use]
    pub fn type_def_texts(&self) -> Vec<String> {
        let mut out: Vec<String> = self.type_defs.iter().map(TypeDef::render_decl).collect();
        out.extend(self.type_defs.iter().filter_map(TypeDef::render_json));
        if let Some(e) = &self.errors {
            out.push(e.render());
        }
        out
    }

    /// Every rendered helper, in helper order.
    #[must_use]
    pub fn helper_texts(&self) -> Vec<String> {
        self.helpers.iter().map(|h| h.render(self)).collect()
    }

    /// Check the schema is internally consistent. A violation is a GENERATION
    /// failure — the point is that a malformed effect can never reach runtime.
    ///
    /// # Errors
    /// Returns every problem found, so one run reports all of them.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();

        if self.default_row_args.len() != self.type_params.len() {
            errs.push(format!(
                "{}: default_row_args ({}) must saturate type_params ({})",
                self.name,
                self.default_row_args.len(),
                self.type_params.len()
            ));
        }

        match self.polymorphism {
            Polymorphism::None => {}
            Polymorphism::ArgBound { tyvar } => {
                if !self.type_params.contains(&tyvar) {
                    errs.push(format!(
                        "{}: ArgBound tyvar `{tyvar}` must appear in type_params (it is a real, \
                         applied GADT parameter)",
                        self.name
                    ));
                }
                if !self.verbs.iter().any(|v| {
                    v.args
                        .iter()
                        .any(|a| matches!(&a.ty, HsType::Var(t) if *t == tyvar))
                }) {
                    errs.push(format!(
                        "{}: ArgBound tyvar `{tyvar}` must appear as some verb's argument type \
                         (that is what makes it argument-bound, not result-bound)",
                        self.name
                    ));
                }
            }
            Polymorphism::ResultBound { tyvar } => {
                if self.type_params.contains(&tyvar) {
                    errs.push(format!(
                        "{}: ResultBound tyvar `{tyvar}` must NOT appear in type_params (it is a \
                         phantom, never an applied GADT parameter)",
                        self.name
                    ));
                }
                if !self
                    .verbs
                    .iter()
                    .any(|v| matches!(&v.ret, HsType::Var(t) if *t == tyvar))
                {
                    errs.push(format!(
                        "{}: ResultBound tyvar `{tyvar}` must appear as some verb's `ret`",
                        self.name
                    ));
                }
                if self.verbs.iter().any(|v| {
                    v.args
                        .iter()
                        .any(|a| matches!(&a.ty, HsType::Var(t) if *t == tyvar))
                }) {
                    errs.push(format!(
                        "{}: ResultBound tyvar `{tyvar}` must not appear as any verb's argument \
                         type (that would make it argument-bound, not result-bound)",
                        self.name
                    ));
                }
            }
        }

        // --- supporting type declarations ---------------------------------
        let mut td_seen: Vec<&str> = Vec::new();
        let mut wire_seen: Vec<&str> = Vec::new();
        for t in &self.type_defs {
            if td_seen.contains(&t.name) {
                errs.push(format!("{}: two type_defs named `{}`", self.name, t.name));
            }
            if wire_seen.contains(&t.wire_name()) {
                errs.push(format!(
                    "{}: two type_defs claim the wire Rust name `{}`",
                    self.name,
                    t.wire_name()
                ));
            }
            td_seen.push(t.name);
            wire_seen.push(t.wire_name());
            errs.extend(
                t.validate()
                    .into_iter()
                    .map(|e| format!("{}: {e}", self.name)),
            );
        }
        // Every `Named` type a declaration REFERENCES must itself be declared
        // here — otherwise the wire emitter has no Rust spelling for it and
        // would panic mid-generation. Catching it as a validation error names
        // every offender at once instead of the first.
        for t in &self.type_defs {
            let referenced: Vec<&HsType> = match &t.shape {
                TypeShape::Record { fields } => fields.iter().map(|f| &f.ty).collect(),
                TypeShape::Sum { variants } => variants.iter().flat_map(|v| &v.fields).collect(),
                TypeShape::Identity { .. } => Vec::new(),
            };
            for ty in referenced {
                for n in named_types(ty) {
                    if !td_seen.contains(&n) && !self.foreign_types.iter().any(|(hs, _)| *hs == n) {
                        errs.push(format!(
                            "{}: {} references `{n}`, which this effect does not declare \
                             and which is not listed in `foreign_types`",
                            self.name, t.name
                        ));
                    }
                }
            }
        }

        for (hs, wire) in self.foreign_types {
            if td_seen.contains(hs) {
                errs.push(format!(
                    "{}: `{hs}` is listed in foreign_types but is also declared in this \
                     effect's own type_defs — foreign_types is for names OTHER effects own",
                    self.name
                ));
            }
            if wire.is_empty() {
                errs.push(format!(
                    "{}: foreign_types entry for `{hs}` carries an empty wire name",
                    self.name
                ));
            }
        }

        for v in &self.verbs {
            if let Some(tag) = v.errors {
                match &self.errors {
                    Some(adt) if adt.name == tag => {}
                    Some(adt) => errs.push(format!(
                        "{}: verb {} is tagged `{}` but the effect declares `{}`",
                        self.name, v.ctor, tag, adt.name
                    )),
                    None => errs.push(format!(
                        "{}: verb {} is tagged `{}` but the effect declares no error ADT",
                        self.name, v.ctor, tag
                    )),
                }
            }
            let mut seen: Vec<&str> = Vec::new();
            for a in &v.args {
                if seen.contains(&a.name) {
                    errs.push(format!(
                        "{}: verb {} has two arguments named `{}`",
                        self.name, v.ctor, a.name
                    ));
                }
                seen.push(a.name);
            }
        }

        for h in &self.helpers {
            // The pure shape: no verb to check against, but its own two
            // invariants — a projection with no fields is not a projection, and
            // a helper that names a verb it does not use is a misleading schema.
            if let HelperBody::Projection { arg, fields, .. } = &h.body {
                if fields.is_empty() {
                    errs.push(format!(
                        "{}: helper {} projects no fields, so it is not a projection",
                        self.name, h.name
                    ));
                }
                // Both directions of the ctor pairing are checked — `None`
                // exactly for a projection here, `Some` exactly for every
                // verb-derived body below. An `Option` only one side checks is
                // how the sentinel comes back in through the side door.
                if h.ctor.is_some() {
                    errs.push(format!(
                        "{}: helper {} is a pure projection but names a verb; \
                         a projection wraps none",
                        self.name, h.name
                    ));
                }
                // `arg` is the one declared type, so it is the one that can
                // name something this effect does not own.
                match arg {
                    HsType::Named(n) if self.type_def(n).is_some() => {}
                    other => errs.push(format!(
                        "{}: helper {} projects from `{}`, which is not a type_defs entry \
                         of this effect",
                        self.name,
                        h.name,
                        other.render()
                    )),
                }
                // Walking the chain here is what turns a mistyped field into a
                // generation failure rather than a GHC error in emitted source.
                if !fields.is_empty() {
                    if let Err(e) = self.project(arg, fields) {
                        errs.push(format!("helper {}: {e}", h.name));
                    }
                }
                continue;
            }
            let Some(ctor) = h.ctor else {
                errs.push(format!(
                    "{}: helper {} names no verb, but only a pure projection may",
                    self.name, h.name
                ));
                continue;
            };
            let Some(v) = self.verb(ctor) else {
                errs.push(format!(
                    "{}: helper {} wraps `{ctor}`, which is not a verb of this effect",
                    self.name, h.name
                ));
                continue;
            };
            let arity = v.args.len();
            match &h.body {
                HelperBody::Nullary | HelperBody::NullaryLiftEither if arity != 0 => {
                    errs.push(format!(
                        "{}: helper {} is nullary but {} takes {arity} argument(s)",
                        self.name, h.name, v.ctor
                    ));
                }
                HelperBody::NullaryLiftEither if v.errors.is_none() => errs.push(format!(
                    "{}: helper {} lifts an Either out of {}, which is not errors-tagged",
                    self.name, h.name, v.ctor
                )),
                HelperBody::Pointfree if arity != 1 => errs.push(format!(
                    "{}: helper {} is point-free but {} takes {arity} argument(s)",
                    self.name, h.name, v.ctor
                )),
                HelperBody::Applied(params) if params.len() != arity => errs.push(format!(
                    "{}: helper {} binds {} parameter(s) but {} takes {arity}",
                    self.name,
                    h.name,
                    params.len(),
                    v.ctor
                )),
                _ => {}
            }
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

// ---------------------------------------------------------------------------
// Verb
// ---------------------------------------------------------------------------

/// One GADT constructor: the unit an authored program actually invokes.
#[derive(Clone, Debug)]
pub struct Verb {
    /// GADT constructor name; also, exactly, the Rust request-enum variant.
    pub ctor: &'static str,
    /// The hand-written inherent method on the handler struct the dispatch arm
    /// calls.
    pub method: &'static str,
    /// Arguments, in wire order.
    pub args: Vec<Arg>,
    /// The SUCCESS result, before any `Either` wrapping.
    pub ret: HsType,
    /// The effect's error ADT name when this verb's failure is typed (#335).
    /// `Some` makes the result `Either <Err> <ret>` and changes the handler
    /// method shape — see [`Verb::result_type`].
    pub errors: Option<&'static str>,
    /// How a suspension carrying this constructor must be routed. PHASE 3
    /// consumes this; phase 1 requires it and emits nothing for it.
    pub handling: HandlingClass,
    /// The extractor's per-verb type-shape policy, for the few verbs the
    /// extractor rewrites at the call site. `None` for every ordinary bridged
    /// verb. PHASE 3 consumes this; phase 1 requires it and emits nothing.
    pub extract: Option<ExtractPolicy>,
}

impl Verb {
    /// The verb's result INSIDE the effect, `Either`-wrapped when tagged.
    #[must_use]
    pub fn result_type(&self) -> HsType {
        match self.errors {
            Some(e) => HsType::either(HsType::Named(e), self.ret.clone()),
            None => self.ret.clone(),
        }
    }
}

/// One verb argument.
#[derive(Clone, Debug)]
pub struct Arg {
    /// The argument's name. Doubles as the dispatch-arm binding and, for an
    /// applied helper, a candidate parameter name.
    pub name: &'static str,
    /// Its Haskell type.
    pub ty: HsType,
    /// How it arrives on the Rust side.
    pub rust: RustBinding,
}

/// How a Haskell argument type is spelled in Rust.
///
/// Closed over what the registry actually needs. The `Value` split is a real
/// semantic distinction, not a stylistic one: an errors-tagged verb's method
/// receives no `cx`, so it has no `DataConTable` to interpret a core value
/// with — the conversion has to happen at Req-decode time, which is what the
/// `JsonArg` wrapper's `FromCore` impl is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RustBinding {
    /// Derived mechanically from the Haskell type: `Text`→`String`,
    /// `Int`→`i64`, `Bool`→`bool`, `[Text]`→`Vec<String>`.
    Derived,
    /// `tidepool_eval::value::Value` — a raw core value, interpreted by the
    /// method using `cx`'s table.
    CoreValue,
    /// `crate::effect_glue::JsonArg` — pre-converted to `serde_json::Value` at
    /// decode time, for a method that gets no `cx`.
    JsonValue,
    /// A wire type from `tidepool_bridge_effects`, named without its path.
    Bridged(&'static str),
    /// An explicit Rust path, for a domain type that never crosses to Haskell.
    /// The reviewed pressure valve — it carries a Rust type path, never Haskell
    /// source.
    Path(&'static str),
}

impl RustBinding {
    /// The Rust type to emit for `ty` under this binding.
    ///
    /// # Panics
    /// Panics when a type needs an explicit binding and did not get one. This
    /// is a GENERATION-time failure by design: an under-specified schema must
    /// not produce output.
    #[must_use]
    pub fn rust_type(self, ty: &HsType, whose: &str) -> String {
        match self {
            RustBinding::CoreValue => "tidepool_eval::value::Value".to_string(),
            RustBinding::JsonValue => "crate::effect_glue::JsonArg".to_string(),
            RustBinding::Bridged(n) => format!("tidepool_bridge_effects::{n}"),
            RustBinding::Path(p) => (*p).to_string(),
            RustBinding::Derived => derive_rust_type(ty).unwrap_or_else(|| {
                panic!(
                    "{whose}: {} needs an explicit RustBinding (Derived covers only \
                     Text/Int/Bool/lists of those)",
                    ty.render()
                )
            }),
        }
    }
}

/// Every [`HsType::Named`] appearing anywhere inside `ty`.
fn named_types(ty: &HsType) -> Vec<&'static str> {
    match ty {
        HsType::Named(n) => vec![n],
        HsType::List(t) | HsType::Maybe(t) => named_types(t),
        HsType::Either(a, b) => {
            let mut v = named_types(a);
            v.extend(named_types(b));
            v
        }
        HsType::Tuple(ts) => ts.iter().flat_map(named_types).collect(),
        _ => Vec::new(),
    }
}

/// The mechanical Haskell→Rust type map, or `None` when the type needs an
/// explicit binding.
fn derive_rust_type(ty: &HsType) -> Option<String> {
    Some(match ty {
        HsType::Text => "String".to_string(),
        HsType::Int => "i64".to_string(),
        HsType::Bool => "bool".to_string(),
        HsType::List(inner) => format!("Vec<{}>", derive_rust_type(inner)?),
        HsType::Maybe(inner) => format!("Option<{}>", derive_rust_type(inner)?),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An effect's typed per-verb failure ADT (#335).
#[derive(Clone, Debug)]
pub struct ErrorAdt {
    /// `"ExecError"`.
    pub name: &'static str,
    /// Its variants, in declaration order.
    pub variants: Vec<ErrorVariant>,
}

/// One variant of an error ADT.
#[derive(Clone, Debug)]
pub struct ErrorVariant {
    /// `"ExecSpawn"`.
    pub ctor: &'static str,
    /// Its fields. Field NAMES are not part of the Haskell decl (the ADT is
    /// positional) but they are the JSON keys the `ToJSON` instance emits and
    /// the pattern variables it binds, so they are contract.
    pub fields: Vec<ErrorField>,
    /// One line explaining when this variant is returned.
    pub doc: &'static str,
}

/// One field of an error variant.
#[derive(Clone, Debug)]
pub struct ErrorField {
    /// The JSON key and pattern-variable name.
    pub name: &'static str,
    /// Its Haskell type.
    pub ty: HsType,
    /// How it is spelled in Rust.
    pub rust: RustBinding,
}

impl ErrorAdt {
    /// The `data … deriving (Show, Eq)` declaration PLUS the hand-templated
    /// `ToJSON` instance.
    ///
    /// The instance is templated rather than derived because the vendored
    /// `ToJSON`'s generic default only covers single-constructor records, so
    /// every multi-constructor error ADT needs an explicit one — and without it
    /// an unhandled `Left err` crashes the render step instead of rendering as
    /// tagged JSON.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("data {} = ", self.name);
        let bodies: Vec<String> = self
            .variants
            .iter()
            .map(|v| {
                let mut s = String::from(v.ctor);
                for f in &v.fields {
                    s.push(' ');
                    s.push_str(&f.ty.render_app_arg());
                }
                s
            })
            .collect();
        out.push_str(&bodies.join(" | "));
        out.push_str(" deriving (Show, Eq)\n");
        out.push_str(&format!("instance ToJSON {} where\n", self.name));
        out.push_str("  toJSON e = case e of\n");
        for v in &self.variants {
            out.push_str("    ");
            out.push_str(v.ctor);
            for f in &v.fields {
                out.push(' ');
                out.push_str(f.name);
            }
            out.push_str(&format!(" -> object [\"tag\" .= (\"{}\" :: Text)", v.ctor));
            for f in &v.fields {
                out.push_str(&format!(", \"{}\" .= {}", f.name, f.name));
            }
            out.push_str("]\n");
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A thin send-wrapper over exactly one verb — the surface authored code calls.
///
/// The SIGNATURE is derived, not declared. In the hand-written registry a
/// helper restates a signature the constructor already implies, and the two can
/// disagree with nothing noticing; here the constructor is the only source.
///
/// [`HelperBody::Projection`] is the one shape that wraps NO verb, and it
/// therefore declares its own two types — see that variant for why it exists
/// and why it is not the thin end of an expression language.
///
/// A helper that is neither of those is not representable, and stays
/// hand-written OUTSIDE the contract until its lane makes it a deliberate
/// schema feature. That exclusion is the no-raw-hatch rule applied honestly.
#[derive(Clone, Debug)]
pub struct Helper {
    /// The Haskell function name: `"run"`.
    pub name: &'static str,
    /// The verb it wraps, or `None` for a [`HelperBody::Projection`] — a pure
    /// helper wraps nothing, and a sentinel constructor name here would be a
    /// lie the validator could not catch.
    pub ctor: Option<&'static str>,
    /// Haddock lines, WITHOUT their `-- |` / `-- ` prefixes. EMPTY is allowed
    /// and means no comment block at all — which is why Exec's `runIn` and
    /// `runArgv` needed the raw escape hatch under the old grammar, and is the
    /// one-line affordance that retires it.
    pub doc: &'static [&'static str],
    /// The wrapper's shape.
    pub body: HelperBody,
}

/// The thin-wrapper shapes.
#[derive(Clone, Debug)]
pub enum HelperBody {
    /// `v = send Ctor` — a nullary constructor.
    Nullary,
    /// `v = send . Ctor` — a unary constructor.
    Pointfree,
    /// `v a b = send (Ctor a b)` — parameters named explicitly, because the
    /// current registry's helpers do not always reuse the argument names.
    Applied(&'static [&'static str]),
    /// `v = send Ctor >>= liftEither` — a nullary, errors-tagged constructor
    /// whose helper UNWRAPS the `Either` into the effect monad's failure. The
    /// derived signature therefore carries the verb's success type bare
    /// (`listWorktrees :: M [WorktreeSummary]`, not `M (Either …)`).
    ///
    /// A shape, not a body: `liftEither` is named once here, in Rust, and there
    /// is no Haskell source in the schema.
    ///
    /// The point-free and applied `liftEither` forms are NOT added, because no
    /// migrated effect needs them: Worktree's other two `>>= liftEither`
    /// helpers (`worktreeBranch`, `worktreeHead`) additionally adapt their
    /// argument through a pure projection, so they would stay unrepresentable
    /// even with those variants. Adding them now would be speculation.
    NullaryLiftEither,
    /// `v h = h.f1.f2` — a PURE record-field projection over the helper's one
    /// argument. No verb, no `send`, no effect: the argument already carries the
    /// value, so the helper reads no state. Declares both its types, because
    /// there is no constructor to derive them from.
    ///
    /// **Why this exists.** `worktreeId` was
    /// listed among the eleven Worktree helpers to relocate into
    /// `haskell/lib/Tidepool/Worktree.hs`, and it is the one that cannot go:
    /// the RepoEvent helpers `commit` and `headChanged` CALL it, and they are
    /// emitted into the same generated `Tidepool.Effects` module — which cannot
    /// import `Tidepool.Worktree`, because that module imports IT. Relocating
    /// `worktreeId` breaks the generated module's own compile, for every row
    /// carrying RepoEvent. Defining it in both places instead would give an eval
    /// (which imports both modules unqualified) an ambiguous occurrence, i.e.
    /// exactly the duplication this migration exists to delete. So the choice
    /// was to represent it or to block the migration: a deliberate schema
    /// feature, documented where it is added.
    ///
    /// **It is a shape and stays one.** A binder, an ordered list of field
    /// names, and the two types the projection connects. No application, no
    /// nesting, no constructors, no operators — the closed Haskell EXPRESSION
    /// AST this schema rejects elsewhere is still rejected here, and the other
    /// ten helpers stay unrepresentable under this variant exactly as they
    /// were.
    Projection {
        /// The bound parameter: `"h"`.
        binder: &'static str,
        /// The argument's Haskell type. DECLARED, because there is no verb to
        /// derive a starting type from — but [`Effect::validate`] requires it to
        /// name a `type_defs` entry of this effect, so it cannot name a type the
        /// projection could not walk.
        arg: HsType,
        /// The field names to project, outermost first: `["handleReceipt",
        /// "treeId"]` renders `h.handleReceipt.treeId`. Must be non-empty.
        ///
        /// The RESULT type is DERIVED by walking this chain through the
        /// `type_defs` table ([`Effect::project`]), never declared. The
        /// principle: a restated signature is a drift class: declaring
        /// `WorktreeId` here and later retyping `WorktreeReceipt.treeId` would
        /// let the two disagree with nothing noticing. Deriving also turns a
        /// field the declaring `TypeDef` does not have into a GENERATION
        /// failure instead of a GHC error.
        fields: &'static [&'static str],
    },
}

impl Helper {
    /// The helper's rendered Haskell: doc block (if any), signature, body.
    ///
    /// # Panics
    /// Panics if the helper wraps a constructor `eff` does not declare —
    /// [`Effect::validate`] reports that as a schema error first.
    #[must_use]
    pub fn render(&self, eff: &Effect) -> String {
        let mut out = String::new();
        for (i, line) in self.doc.iter().enumerate() {
            out.push_str(if i == 0 { "-- | " } else { "-- " });
            out.push_str(line);
            out.push('\n');
        }
        // The one shape that wraps no verb: both types are declared, and the
        // body is a field chain rather than a `send`.
        if let HelperBody::Projection {
            binder,
            arg,
            fields,
        } = &self.body
        {
            // DERIVED, never declared — `Effect::validate` reports the same
            // failure as a schema error first, so this panic is unreachable
            // through a validated effect.
            let ret = eff.project(arg, fields).unwrap_or_else(|e| panic!("{e}"));
            out.push_str(&format!(
                "{} :: {} -> {}\n{} {} = {}",
                self.name,
                arg.render(),
                ret.render(),
                self.name,
                binder,
                std::iter::once((*binder).to_string())
                    .chain(fields.iter().map(|f| (*f).to_string()))
                    .collect::<Vec<_>>()
                    .join(".")
            ));
            return out;
        }
        let ctor = self
            .ctor
            .unwrap_or_else(|| panic!("{}: helper {} declares no verb", eff.name, self.name));
        let verb = eff
            .verb(ctor)
            .unwrap_or_else(|| panic!("{}: helper {} wraps unknown {ctor}", eff.name, self.name));
        let args: Vec<HsType> = verb.args.iter().map(|a| a.ty.clone()).collect();
        // `liftEither` consumes the `Either`, so the helper's result is the
        // verb's SUCCESS type. Every other shape forwards the verb's result as
        // the verb declares it.
        let result = match &self.body {
            HelperBody::NullaryLiftEither => verb.ret.clone(),
            _ => verb.result_type(),
        };
        let sig = if eff.helpers_row_polymorphic {
            crate::hs::render_member_signature(&args, eff.name, &result)
        } else {
            render_signature(&args, "M", &result)
        };
        out.push_str(&format!("{} :: {}\n", self.name, sig));
        match &self.body {
            HelperBody::Nullary => {
                out.push_str(&format!("{} = send {ctor}", self.name));
            }
            HelperBody::NullaryLiftEither => {
                out.push_str(&format!("{} = send {ctor} >>= liftEither", self.name));
            }
            HelperBody::Pointfree => {
                out.push_str(&format!("{} = send . {ctor}", self.name));
            }
            HelperBody::Applied(params) => {
                out.push_str(self.name);
                for p in *params {
                    out.push(' ');
                    out.push_str(p);
                }
                out.push_str(" = send (");
                out.push_str(ctor);
                for p in *params {
                    out.push(' ');
                    out.push_str(p);
                }
                out.push(')');
            }
            // Returned above, before the verb lookup.
            HelperBody::Projection { .. } => unreachable!(),
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Annotations for the later phases (declared, validated, emitted-nothing)
// ---------------------------------------------------------------------------

/// How a suspension carrying this verb's constructor must be routed.
///
/// Modelled on what `tidepool-harness`'s `classify_hole` ACTUALLY
/// distinguishes, not on an earlier five-name sketch that compressed away
/// real distinctions. An unrecognized constructor must fail LOUD rather than
/// falling through to [`HandlingClass::Ask`], which is the silent-misroute
/// path this schema exists to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlingClass {
    /// Suspends to the model, answered in the same context.
    RunLlmTurn,
    /// Suspends to the model as a fan-out with a join.
    Fork,
    /// Terminates the turn, handing a typed value up to the parent continuation.
    Finalize,
    /// Suspends to the operator as a typed, blocking form.
    AskUserForm,
    /// Posts to the operator feed and resumes immediately — non-blocking.
    Note,
    /// Suspends to the operator as a raw structured payload.
    Ask,
    /// Serviced immediately by the driver from cycle state.
    ReadState,
    /// Routed to the driver's subagent service.
    Subagent,
    /// Dispatched into a driver-owned outer-row handler.
    OuterDispatch(OuterEffect),
    /// Routed to the driver's green-thread (`Tidepool.Async`) scheduler.
    Green,
}

/// Which driver-owned handler an [`HandlingClass::OuterDispatch`] verb reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OuterEffect {
    /// Console — additionally posts its text to the operator feed.
    Console,
    /// Managed worktrees.
    Worktree,
    /// Typed repository events.
    RepoEvent,
    /// Shell execution.
    Exec,
    /// The durable run journal.
    Journal,
}

/// The extractor's per-verb call-site policy, for the few verbs it rewrites.
///
/// A CLOSED description of what `Translate.hs`'s `sitedVerbs` rows vary today.
/// `vsCheckType` is a function field in Haskell; enumerating its degrees of
/// freedom here is what lets a new policy be added deliberately in Haskell
/// rather than serialized through the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractPolicy {
    /// The `*Sited` sibling this call site is rewritten to.
    pub sited_name: &'static str,
    /// The module the sibling resolves from — NOT always the verb's own.
    pub sited_module: &'static str,
    /// Explicit type arguments at the call site (1 or 2 today).
    pub type_args: u8,
    /// Trailing value arguments (1 or 2 today); anything ahead of them is
    /// dictionary arguments, re-applied verbatim.
    pub value_arity: u8,
    /// Reject an answer type that mentions the effect monad. False only for
    /// `finalize`, whose value crosses in-heap and may carry a closure.
    /// (Rejecting a POLYMORPHIC site is unconditional and so is not a field.)
    pub reject_effect_monad: bool,
    /// What the site records as its answer shape.
    pub answer_shape: AnswerShape,
    /// What happens when the call cannot be rewritten.
    ///
    /// NOTE for phase 3: the Haskell counterpart (`vsMisShapeIsError`) is
    /// declared, documented, and set on two rows — but nothing reads it. Do not
    /// assume a generated form preserves today's behavior until that is
    /// resolved.
    pub mis_shape: MisShape,
}

/// The answer shape a sited call records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerShape {
    /// The site answers `T`.
    Scalar,
    /// The site answers `[T]` — a fan-out with a join.
    ListOfElement,
}

/// What a call the extractor cannot rewrite does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MisShape {
    /// Fall through to the opaque stub.
    FallThrough,
    /// Fail the extract, naming the site.
    HardError,
}
