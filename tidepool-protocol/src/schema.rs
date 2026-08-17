//! The effect schema — plain data describing one effect completely.
//!
//! This is the single source PRD 22 is built around. Everything an effect's
//! generated artifacts need is here as structured data: no raw Haskell, no raw
//! Rust bodies, no field-order-by-comment.
//!
//! Two annotation slots ([`Verb::handling`] and [`Verb::extract`]) exist for
//! LATER phases. They are required fields, so a verb cannot be added without
//! answering them — that is PRD 22's acceptance line "a verb missing a
//! handling-class annotation fails generation, not runtime" — but no phase-1
//! generator reads them. See the scaffold doc §3.5.

use crate::hs::{render_signature, HsType};

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
    /// eval surface. These reach the eval preamble and the decl plane, NOT the
    /// generated `Tidepool.Effects` module.
    pub extra_imports: &'static [&'static str],
    /// Supporting Haskell declarations emitted before the GADT. The error ADT
    /// is NOT listed here — it is derived from [`Effect::errors`].
    pub type_defs: Vec<TypeDef>,
    /// This effect's typed per-verb failure ADT (#335), if it has one.
    pub errors: Option<ErrorAdt>,
    /// The GADT constructors, one per verb.
    pub verbs: Vec<Verb>,
    /// The thin send-wrapper surface authored code actually calls.
    pub helpers: Vec<Helper>,
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

    /// Every rendered `type_defs` entry, in emission order: the authored
    /// supporting declarations first, then the derived error ADT — matching the
    /// order the hand-written projection emits them.
    #[must_use]
    pub fn type_def_texts(&self) -> Vec<String> {
        let mut out: Vec<String> = self.type_defs.iter().map(TypeDef::render).collect();
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
            let Some(v) = self.verb(h.ctor) else {
                errs.push(format!(
                    "{}: helper {} wraps `{}`, which is not a verb of this effect",
                    self.name, h.name, h.ctor
                ));
                continue;
            };
            let arity = v.args.len();
            match &h.body {
                HelperBody::Nullary if arity != 0 => errs.push(format!(
                    "{}: helper {} is nullary but {} takes {arity} argument(s)",
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
// Supporting type declarations
// ---------------------------------------------------------------------------

/// A supporting Haskell declaration emitted before the GADT.
///
/// Phase 1 needs none (Exec's only `type_defs` entry is its derived error ADT),
/// but the slot is structured from the start so a later lane adds a record
/// rather than a string.
#[derive(Clone, Debug)]
pub enum TypeDef {
    /// `data R = R { f :: T, … } deriving (Show, Eq)`.
    Record {
        /// The type and constructor name.
        name: &'static str,
        /// Its fields, in wire order — the order IS the wire contract.
        fields: Vec<RecordField>,
    },
}

/// One field of a record `TypeDef`.
#[derive(Clone, Debug)]
pub struct RecordField {
    /// The Haskell field name.
    pub name: &'static str,
    /// Its type.
    pub ty: HsType,
}

impl TypeDef {
    /// The Haskell declaration.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            TypeDef::Record { name, fields } => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{} :: {}", f.name, f.ty.render()))
                    .collect();
                format!(
                    "data {name} = {name} {{ {} }} deriving (Show, Eq)",
                    fs.join(", ")
                )
            }
        }
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
/// A helper that is not a thin wrapper over one verb is not representable, and
/// stays hand-written OUTSIDE the contract until its lane makes it a deliberate
/// schema feature. That exclusion is the no-raw-hatch rule applied honestly.
#[derive(Clone, Debug)]
pub struct Helper {
    /// The Haskell function name: `"run"`.
    pub name: &'static str,
    /// The verb it wraps.
    pub ctor: &'static str,
    /// Haddock lines, WITHOUT their `-- |` / `-- ` prefixes. EMPTY is allowed
    /// and means no comment block at all — which is why Exec's `runIn` and
    /// `runArgv` needed the raw escape hatch under the old grammar, and is the
    /// one-line affordance that retires it.
    pub doc: &'static [&'static str],
    /// The wrapper's shape.
    pub body: HelperBody,
}

/// The three thin-wrapper shapes.
#[derive(Clone, Debug)]
pub enum HelperBody {
    /// `v = send Ctor` — a nullary constructor.
    Nullary,
    /// `v = send . Ctor` — a unary constructor.
    Pointfree,
    /// `v a b = send (Ctor a b)` — parameters named explicitly, because the
    /// current registry's helpers do not always reuse the argument names.
    Applied(&'static [&'static str]),
}

impl Helper {
    /// The helper's rendered Haskell: doc block (if any), signature, body.
    ///
    /// # Panics
    /// Panics if the helper wraps a constructor `eff` does not declare —
    /// [`Effect::validate`] reports that as a schema error first.
    #[must_use]
    pub fn render(&self, eff: &Effect) -> String {
        let verb = eff.verb(self.ctor).unwrap_or_else(|| {
            panic!(
                "{}: helper {} wraps unknown {}",
                eff.name, self.name, self.ctor
            )
        });
        let mut out = String::new();
        for (i, line) in self.doc.iter().enumerate() {
            out.push_str(if i == 0 { "-- | " } else { "-- " });
            out.push_str(line);
            out.push('\n');
        }
        let args: Vec<HsType> = verb.args.iter().map(|a| a.ty.clone()).collect();
        out.push_str(&format!(
            "{} :: {}\n",
            self.name,
            render_signature(&args, "M", &verb.result_type())
        ));
        match &self.body {
            HelperBody::Nullary => {
                out.push_str(&format!("{} = send {}", self.name, self.ctor));
            }
            HelperBody::Pointfree => {
                out.push_str(&format!("{} = send . {}", self.name, self.ctor));
            }
            HelperBody::Applied(params) => {
                out.push_str(self.name);
                for p in *params {
                    out.push(' ');
                    out.push_str(p);
                }
                out.push_str(" = send (");
                out.push_str(self.ctor);
                for p in *params {
                    out.push(' ');
                    out.push_str(p);
                }
                out.push(')');
            }
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
/// distinguishes, not on PRD 22's five-name sketch — the sketch was
/// compression and adopting it would lose information. See the scaffold doc
/// §3.5, including the phase-3 requirement that an unrecognized constructor
/// must fail LOUD rather than falling through to [`HandlingClass::Ask`], which
/// is the silent-misroute path the whole migration exists to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlingClass {
    /// Suspends to the model, answered in the same context.
    RunLlmTurn,
    /// Suspends to the model as a fan-out with a join.
    Fork,
    /// Terminates the turn, handing a typed value up to the parent hole.
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
