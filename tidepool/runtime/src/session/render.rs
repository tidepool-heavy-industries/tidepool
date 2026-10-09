//! Pure rendering of the gen-versioned `Tidepool.Session.Lib.G<g>` declaration
//! modules.
//!
//! The whole module source is a **pure function of the decl log**: given the
//! ordered turns (each carrying GHC-normalized declaration source, the
//! GHC-sourced [`ExportItem`]s it introduces, and the generation it chains
//! from — [`DeclTurn::parent`]), [`render_module`] produces the source of any
//! one generation's module. Each generation imports its **parent** generation
//! **selectively** — `import …G<parent> hiding (<names redefined this
//! turn>)` — and re-exports it plus this turn's items. `parent` is `g - 1` for
//! a flat (ROOT-only) session, but need not be — a sibling turn's parent can
//! be any earlier generation, which is what turns the flat generation chain
//! into a tree of independent, mutually invisible branches.
//! That selective re-export is what lets a redefined `data` type coexist with
//! its older shape without GHC's conflicting-export error: the two `Foo`s
//! live in distinct gen-versioned modules and only the newest is in scope
//! unqualified.
//!
//! Binder names come from GHC (see `super::binders`), never a Rust-side Haskell
//! parser — this module only *renders* the structured items.

use std::collections::BTreeMap;
use std::sync::Arc;

use tidepool_repr::{Generation, SessionModule};

use super::SourceImports;

/// A name a declaration turn brings into scope, as classified by GHC.
///
/// `Value` is a function/value binder (`slug`). `Type` is a type/data
/// constructor head plus its data-constructor children (`Foo` with `[A, B]`),
/// so it can be rendered as `Foo(..)` for both export and `hiding`. `Class`
/// is a typeclass head with its method names, rendered as `Class(..)` so
/// that later `instance` declarations can see the methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportItem {
    /// A function/value binder.
    Value { name: String },
    /// A type/data constructor head plus its data-constructor children.
    Type { name: String, cons: Vec<String> },
    /// A typeclass head plus its method names.
    Class { name: String, methods: Vec<String> },
}

/// The namespace-level kind of one GHC-reported declaration export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    Value,
    Type,
    Class,
}

impl DeclarationKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeclarationKind::Value => "value",
            DeclarationKind::Type => "type",
            DeclarationKind::Class => "class",
        }
    }
}

/// Parenthesize an operator name for an export/`hiding` list (`.+` → `(.+)`).
/// Normal identifiers (alphanumeric/`_`-leading) and already-parenthesized names
/// pass through unchanged. Without this, a `session_def`'d operator like `(.+)`
/// emits `module …Lib.G1 (.+) where` with the operator UNPARENTHESIZED in the
/// export list — a GHC parse error that broke the whole session.
fn op_wrap(name: &str) -> String {
    match name.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' || c == '(' => name.to_string(),
        _ => format!("({name})"),
    }
}

impl From<&tidepool_toolchain::declaration_join::DeclarationExport> for ExportItem {
    fn from(export: &tidepool_toolchain::declaration_join::DeclarationExport) -> Self {
        use tidepool_toolchain::declaration_join::DeclarationKind;
        let name = export.head.occurrence.clone();
        let children = || {
            export
                .children
                .iter()
                .map(|child| child.occurrence.clone())
                .collect()
        };
        match export.kind {
            DeclarationKind::Value => Self::Value { name },
            DeclarationKind::Type => Self::Type {
                name,
                cons: children(),
            },
            DeclarationKind::Class => Self::Class {
                name,
                methods: children(),
            },
        }
    }
}

impl ExportItem {
    pub(crate) fn head_namespace(&self) -> tidepool_toolchain::declaration_join::ExportNamespace {
        use tidepool_toolchain::declaration_join::ExportNamespace;
        match self {
            Self::Value { .. } => ExportNamespace::Value,
            Self::Type { .. } | Self::Class { .. } => ExportNamespace::Type,
        }
    }
    /// The closed declaration kind GHC assigned this export.
    #[must_use]
    pub fn kind(&self) -> DeclarationKind {
        match self {
            ExportItem::Value { .. } => DeclarationKind::Value,
            ExportItem::Type { .. } => DeclarationKind::Type,
            ExportItem::Class { .. } => DeclarationKind::Class,
        }
    }

    /// The head identifier (the value name, the type/class name).
    #[must_use]
    pub fn head_name(&self) -> &str {
        match self {
            ExportItem::Value { name }
            | ExportItem::Type { name, .. }
            | ExportItem::Class { name, .. } => name,
        }
    }

    /// Value-namespace names this declaration replaces in the live binding plane.
    /// Type and class heads coexist with values of the same occurrence.
    pub(crate) fn value_names(&self) -> impl Iterator<Item = &str> {
        let head = match self {
            Self::Value { name } => Some(name.as_str()),
            _ => None,
        };
        let children: &[String] = match self {
            Self::Type { cons, .. } => cons,
            Self::Class { methods, .. } => methods,
            Self::Value { .. } => &[],
        };
        head.into_iter().chain(children.iter().map(String::as_str))
    }

    /// Every identifier this item introduces: the head plus constructors or methods.
    /// Used to decide whether a later turn redefines (shadows) this item.
    pub fn all_names(&self) -> impl Iterator<Item = &str> {
        let head = std::iter::once(self.head_name());
        let cons: Box<dyn Iterator<Item = &str>> = match self {
            ExportItem::Type { cons, .. } => Box::new(cons.iter().map(String::as_str)),
            ExportItem::Class { methods, .. } => Box::new(methods.iter().map(String::as_str)),
            ExportItem::Value { .. } => Box::new(std::iter::empty()),
        };
        head.chain(cons)
    }

    /// Render this item as an export-list / `hiding`-list entry. A value is its
    /// bare name; a type with constructors exports via `(..)` so the value shape
    /// stays usable; a type synonym (no constructors) renders as a bare head
    /// (GHC rejects `Synonym(..)` for synonyms); a class always renders as
    /// `Class(..)` so later instances can see its methods.
    #[must_use]
    pub fn render_entry(&self) -> String {
        match self {
            ExportItem::Value { name } => op_wrap(name),
            // A type synonym or family (empty cons) renders as a bare head;
            // `(..)` is rejected by GHC for synonyms.
            ExportItem::Type { name, cons } if cons.is_empty() => {
                let rendered = op_wrap(name);
                if rendered != *name {
                    format!("type {rendered}")
                } else {
                    rendered
                }
            }
            ExportItem::Type { name, .. } => format!("{}(..)", op_wrap(name)),
            // A class always exports with `(..)` so methods are visible to instances.
            ExportItem::Class { name, .. } => format!("{}(..)", op_wrap(name)),
        }
    }
}

/// Replace complete export groups by namespace and head name. Constructor or method
/// collisions between different heads remain for GHC to diagnose.
pub(super) fn extend_exports_by_head(exports: &mut Vec<ExportItem>, introduced: &[ExportItem]) {
    for item in introduced {
        exports.retain(|prior| {
            prior.head_name() != item.head_name() || prior.head_namespace() != item.head_namespace()
        });
        exports.push(item.clone());
    }
}

/// Compose selected compiler identities, never identities reconstructed from source heads.
/// Replacements remove complete groups in the same namespace; hidden instance and
/// family evidence remains owned independently by the declaration certificates.
pub(super) fn select_authored_exports(
    inherited: &[tidepool_toolchain::declaration_join::DeclarationExport],
    retractions: &[super::DeclarationRetraction],
    introduced: &[tidepool_toolchain::declaration_join::DeclarationExport],
) -> Vec<tidepool_toolchain::declaration_join::DeclarationExport> {
    let mut selected = inherited.to_vec();
    selected.retain(|export| {
        !retractions
            .iter()
            .any(|retraction| retraction.selects(export.head.namespace, &export.head.occurrence))
    });
    for export in introduced {
        selected.retain(|prior| {
            prior.head.namespace != export.head.namespace
                || prior.head.occurrence != export.head.occurrence
        });
        selected.push(export.clone());
    }
    selected
}

/// One declaration turn: the raw source text(s) appended this turn and the
/// export items GHC says they introduce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclTurn {
    /// GHC-parsed source and imports used for compilation. Rendering never
    /// scans the authored source for Haskell header syntax.
    pub normalized: super::DeclarationSource,
    pub external_imports: SourceImports,
    /// Replay source reconstructed from the normalized header and body,
    /// including trusted imports required to compile it outside this frontend.
    pub sources: Vec<String>,
    /// Imports authored in this turn, separate from trusted imports prefixed
    /// onto `sources` for declaration validation.
    pub workbench_imports: SourceImports,
    /// The exportable binders this turn introduces (from GHC).
    pub items: Vec<ExportItem>,
    /// GHC-rendered types for the term exports introduced by this generation.
    ///
    /// Empty is a supported compatibility state for declarations reconstructed
    /// from metadata written before types were retained. The status path may
    /// fill those entries after one batched inspection, fenced by generation.
    pub value_types: BTreeMap<String, String>,
    /// Names this turn REMOVES from the persistent declaration environment (no replacement). A name is
    /// retracted when its binding migrates to the persistent binding store (e.g. a
    /// self-referential `n <- pure (n+1)` that must materialize) — the decl
    /// declaration module must then stop exporting it, or a later `let`/`def` would compile
    /// against the stale decl. A pure-retraction turn carries empty
    /// `sources`/`items` and one or more `retracts`. Honored by every scoping
    /// fold (`cumulative_exports_before`, `current_heads`, `replayable_sources`,
    /// `render_module`) so all persistent declaration environment views stay consistent; a later
    /// `define` of the same name naturally un-retracts it (latest-wins).
    pub retracts: Vec<DeclarationRetraction>,
    /// The generation this turn chains from — `None` only for the very first
    /// turn ever pushed to the log. `Generation` stays a single globally
    /// monotone counter (`turns.len()`) so module names never collide across
    /// branches; this field is what turns the flat generation sequence into a
    /// tree — a scope's turns chain from that scope's own tip, which may be
    /// any earlier generation, not necessarily `g - 1`. Flat (ROOT-only) usage
    /// always has `parent == Some(g - 1)` (or `None` for `g == 1`), which is
    /// the back-compat degeneracy every fold below must preserve.
    pub parent: Option<Generation>,
}

/// Explicit withdrawal names select every matching admitted namespace;
/// materialized values withdraw only an exact value head.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeclarationRetraction {
    Name(String),
    Head {
        namespace: tidepool_toolchain::declaration_join::ExportNamespace,
        occurrence: String,
    },
}

impl From<String> for DeclarationRetraction {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}
impl From<&str> for DeclarationRetraction {
    fn from(name: &str) -> Self {
        Self::Name(name.into())
    }
}

impl DeclarationRetraction {
    pub fn occurrence(&self) -> &str {
        match self {
            Self::Name(name)
            | Self::Head {
                occurrence: name, ..
            } => name,
        }
    }
    pub(crate) fn selects(
        &self,
        namespace: tidepool_toolchain::declaration_join::ExportNamespace,
        name: &str,
    ) -> bool {
        self.occurrence() == name
            && match self {
                Self::Name(_) => true,
                Self::Head {
                    namespace: selected,
                    ..
                } => *selected == namespace,
            }
    }
    fn selects_item(&self, item: &ExportItem) -> bool {
        self.selects(item.head_namespace(), item.head_name())
    }
}

/// The sparse generation-addressed declaration graph. A reserved identity
/// has no lexical meaning until a certified join commits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclLog {
    /// Changes at every graph mutation, including reserved-slot settlement.
    /// Zero permanently fences publication if the revision space is exhausted.
    revision: u64,
    /// Highest allocated identity, independent of public visibility.
    high_water: Generation,
    /// Sparse generation-addressed nodes; abandoned reservations remain
    /// allocated without retaining empty vector slots up to high_water.
    turns: BTreeMap<Generation, DeclarationSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DeclarationSlot {
    Reserved,
    Committed(DeclTurn),
    CertifiedAuthored {
        turn: DeclTurn,
        prepared: Arc<super::lexical_projection::PreparedAuthoredDeclaration>,
    },
    Joined(JoinedDeclaration),
    Recovered(RecoveredDeclaration),
}

/// A compiler-certified merged declaration interface with a public lexical
/// parent and exact private module provenance. Its interface artifact is
/// retained by the toolchain cache; there is deliberately no source renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JoinedDeclaration {
    pub turn: DeclTurn,
    pub evidence: super::paired_publication::PublicationEvidence,
    pub context: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    pub surface: AdmittedDeclarationSurface,
}

/// A source-free public tip restored from the compiler's retained-artifact
/// readback. Its original module identity is never allocated again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveredDeclaration {
    pub turn: DeclTurn,
    pub evidence: Arc<tidepool_toolchain::declaration_join::RecoveredDeclarationTip>,
    pub surface: AdmittedDeclarationSurface,
}

/// Explicit source-surface roots and their compiler-resolved import graph.
/// Implementation requirements and synthetic Join ancestry do not select roots.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct AdmittedDeclarationSurface {
    pub roots: Vec<tidepool_toolchain::declaration_join::ExactModuleIdentity>,
    pub lexical: Vec<tidepool_toolchain::declaration_join::ExactLexicalNode>,
}

/// Prepared reserved slots contain only the newly admitted range. The existing
/// declaration graph stays in its sole owner until the manifest becomes visible.
pub(crate) struct PreparedDeclarationReservations {
    previous_high_water: Generation,
    previous_revision: u64,
    high_water: Generation,
    revision: u64,
    slots: BTreeMap<Generation, DeclarationSlot>,
    generations: Vec<Generation>,
}

impl DeclLog {
    pub(crate) fn prepare_reservations(
        &self,
        count: usize,
    ) -> Option<PreparedDeclarationReservations> {
        let count_u64 = u64::try_from(count).ok()?;
        let high_water = Generation(self.high_water.0.checked_add(count_u64)?);
        let revision = self.publication_revision()?.checked_add(count_u64)?;
        let mut generations = Vec::new();
        generations.try_reserve_exact(count).ok()?;
        let mut slots = BTreeMap::new();
        for offset in 0..count_u64 {
            let generation = Generation(self.high_water.0 + offset + 1);
            generations.push(generation);
            slots.insert(generation, DeclarationSlot::Reserved);
        }
        Some(PreparedDeclarationReservations {
            previous_high_water: self.high_water,
            previous_revision: self.revision,
            high_water,
            revision,
            slots,
            generations,
        })
    }

    pub(crate) fn commit_reservations(
        &mut self,
        reservations: PreparedDeclarationReservations,
    ) -> Vec<Generation> {
        assert_eq!(self.high_water, reservations.previous_high_water);
        assert_eq!(self.revision, reservations.previous_revision);
        for (generation, slot) in reservations.slots {
            assert!(self.turns.insert(generation, slot).is_none());
        }
        self.high_water = reservations.high_water;
        self.revision = reservations.revision;
        reservations.generations
    }
    /// An empty log (`Generation(0)`, no turns).
    #[must_use]
    pub fn new() -> DeclLog {
        DeclLog {
            revision: 1,
            high_water: Generation(0),
            turns: BTreeMap::new(),
        }
    }

    pub(crate) fn publication_revision(&self) -> Option<u64> {
        (self.revision != 0).then_some(self.revision)
    }

    fn advance_revision(&mut self) {
        self.revision = if self.revision == 0 {
            0
        } else {
            self.revision.checked_add(1).unwrap_or(0)
        };
    }

    /// Highest allocated generation. This includes invisible reservations and
    /// is not a public visibility version.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.high_water
    }

    /// Carry burned identities into a new incarnation before any declaration
    /// is admitted. Recovery must not create a different module at an old G<n>.
    pub(crate) fn restore_high_water(&mut self, high_water: Generation) -> bool {
        if !self.turns.is_empty() || self.high_water != Generation(0) {
            return false;
        }
        self.high_water = high_water;
        self.advance_revision();
        true
    }

    pub(crate) fn restore_recovered(
        &mut self,
        generation: Generation,
        kind: super::recovery::RecoveryNodeKind,
        recovered: RecoveredDeclaration,
    ) -> bool {
        let original_matches = match kind {
            super::recovery::RecoveryNodeKind::Authored => {
                recovered.evidence.authored_generation() == Some(generation.0)
            }
            super::recovery::RecoveryNodeKind::Join => {
                // A join's protected nominal root can be a reused Surface.H
                // interface; the durable graph owns its logical generation.
                recovered.evidence.authored_generation().is_none()
            }
        };
        if generation.0 == 0
            || generation > self.high_water
            || self.turns.contains_key(&generation)
            || recovered.turn.parent.is_some()
            || !original_matches
        {
            return false;
        }
        self.turns
            .insert(generation, DeclarationSlot::Recovered(recovered));
        self.advance_revision();
        true
    }

    /// Append a committed turn (its `parent` must already be set by the caller —
    /// `DeclLog` has no notion of scope and cannot infer it), returning the
    /// new (current) generation.
    pub fn push(&mut self, turn: DeclTurn) -> Generation {
        let generation = self.next_generation();
        self.turns
            .insert(generation, DeclarationSlot::Committed(turn));
        generation
    }

    /// Allocate an immutable module identity before an off-checkout join
    /// validation. A failed candidate leaves this slot reserved forever.
    pub fn reserve(&mut self) -> Generation {
        let generation = self.next_generation();
        self.turns.insert(generation, DeclarationSlot::Reserved);
        generation
    }

    pub(crate) fn is_reserved(&self, generation: Generation) -> bool {
        matches!(self.turns.get(&generation), Some(DeclarationSlot::Reserved))
    }

    pub(crate) fn commit_reserved_authored(
        &mut self,
        generation: Generation,
        turn: DeclTurn,
    ) -> bool {
        if generation.0 == 0
            || turn
                .parent
                .is_some_and(|parent| parent.0 >= generation.0 || self.turn(parent).is_none())
        {
            return false;
        }
        let Some(slot) = self.turns.get_mut(&generation) else {
            return false;
        };
        if !matches!(slot, DeclarationSlot::Reserved) {
            return false;
        }
        *slot = DeclarationSlot::Committed(turn);
        self.advance_revision();
        true
    }

    pub(super) fn commit_reserved_certified_authored(
        &mut self,
        generation: Generation,
        turn: DeclTurn,
        prepared: Arc<super::lexical_projection::PreparedAuthoredDeclaration>,
    ) -> bool {
        if prepared.generation != generation
            || prepared.parent != turn.parent.unwrap_or(Generation(0))
            || prepared.evidence.product().owner().module
                != SessionModule::lib(generation).module_name()
            || !self.commit_reserved_authored(generation, turn.clone())
        {
            return false;
        }
        self.turns.insert(
            generation,
            DeclarationSlot::CertifiedAuthored { turn, prepared },
        );
        true
    }

    pub(crate) fn projection_at(
        &self,
        generation: Generation,
    ) -> Option<&Arc<super::CertifiedDeclarationProjection>> {
        match self.turns.get(&generation)? {
            DeclarationSlot::CertifiedAuthored { prepared, .. } => Some(&prepared.projection),
            DeclarationSlot::Joined(joined) => joined.evidence.projection(),
            _ => None,
        }
    }

    pub(crate) fn certified_authored_at(
        &self,
        generation: Generation,
    ) -> Option<&tidepool_toolchain::declaration_join::CertifiedAuthoredDeclaration> {
        match self.turns.get(&generation)? {
            DeclarationSlot::CertifiedAuthored { prepared, .. } => Some(&prepared.evidence),
            _ => None,
        }
    }

    pub(crate) fn certified_authored_arc_at(
        &self,
        generation: Generation,
    ) -> Option<Arc<tidepool_toolchain::declaration_join::CertifiedAuthoredDeclaration>> {
        match self.turns.get(&generation)? {
            DeclarationSlot::CertifiedAuthored { prepared, .. } => Some(prepared.evidence.clone()),
            _ => None,
        }
    }

    pub(crate) fn joined_context_at(
        &self,
        generation: Generation,
    ) -> Option<Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>> {
        match self.turns.get(&generation)? {
            DeclarationSlot::Joined(joined) => Some(joined.context.clone()),
            DeclarationSlot::CertifiedAuthored { prepared, .. } => {
                Some(prepared.projection.context().clone())
            }
            DeclarationSlot::Recovered(recovered) => Some(recovered.evidence.context().clone()),
            _ => None,
        }
    }

    pub(crate) fn retained_value_generation_high_water(&self) -> Generation {
        self.turns
            .keys()
            .filter_map(|generation| self.joined_context_at(*generation))
            .flat_map(|context| context.value_interfaces())
            .filter(|interface| interface.interface().unit() == "main")
            .filter_map(|interface| {
                interface
                    .interface()
                    .module()
                    .strip_prefix("Tidepool.Session.Val.G")?
                    .parse::<u64>()
                    .ok()
            })
            .max()
            .map(Generation)
            .unwrap_or(Generation(0))
    }

    /// Original selected export identities certified at this exact lexical tip.
    /// A joined wrapper's generation does not identify the definitions it exports.
    pub(crate) fn certified_exports_at(
        &self,
        generation: Generation,
    ) -> Option<&[tidepool_toolchain::declaration_join::DeclarationExport]> {
        match self.turns.get(&generation)? {
            DeclarationSlot::CertifiedAuthored { prepared, .. } => {
                Some(prepared.projection.receipt().exports())
            }
            DeclarationSlot::Joined(joined) => Some(joined.evidence.exports()),
            DeclarationSlot::Recovered(recovered) => Some(recovered.evidence.exports()),
            DeclarationSlot::Reserved | DeclarationSlot::Committed(_) => None,
        }
    }

    pub(crate) fn admitted_surface_at(
        &self,
        generation: Generation,
    ) -> Option<&AdmittedDeclarationSurface> {
        match self.turns.get(&generation)? {
            DeclarationSlot::Joined(joined) => Some(&joined.surface),
            DeclarationSlot::CertifiedAuthored { prepared, .. } => Some(&prepared.surface),
            DeclarationSlot::Recovered(recovered) => Some(&recovered.surface),
            _ => None,
        }
    }

    pub(crate) fn joined_at(&self, generation: Generation) -> Option<&JoinedDeclaration> {
        match self.turns.get(&generation)? {
            DeclarationSlot::Joined(joined) => Some(joined),
            _ => None,
        }
    }

    pub(crate) fn recovered_at(&self, generation: Generation) -> Option<&RecoveredDeclaration> {
        match self.turns.get(&generation)? {
            DeclarationSlot::Recovered(recovered) => Some(recovered),
            _ => None,
        }
    }

    fn next_generation(&mut self) -> Generation {
        self.advance_revision();
        self.high_water = Generation(
            self.high_water
                .0
                .checked_add(1)
                .expect("declaration generation exhausted"),
        );
        self.high_water
    }

    pub(crate) fn commit_reserved(
        &mut self,
        generation: Generation,
        joined: JoinedDeclaration,
    ) -> bool {
        if generation.0 == 0 {
            return false;
        }
        if !joined.evidence.matches_generation(generation) {
            return false;
        }
        let turn = &joined.turn;
        if turn
            .parent
            .is_some_and(|parent| parent.0 >= generation.0 || self.turn(parent).is_none())
        {
            return false;
        }
        let Some(slot) = self.turns.get_mut(&generation) else {
            return false;
        };
        if !matches!(slot, DeclarationSlot::Reserved) {
            return false;
        }
        *slot = DeclarationSlot::Joined(joined);
        self.advance_revision();
        true
    }

    pub fn turn(&self, generation: Generation) -> Option<&DeclTurn> {
        match self.turns.get(&generation)? {
            DeclarationSlot::Committed(turn) => Some(turn),
            DeclarationSlot::CertifiedAuthored { turn, .. } => Some(turn),
            DeclarationSlot::Joined(joined) => Some(&joined.turn),
            DeclarationSlot::Recovered(recovered) => Some(&recovered.turn),
            DeclarationSlot::Reserved => None,
        }
    }

    pub fn turn_mut(&mut self, generation: Generation) -> Option<&mut DeclTurn> {
        self.advance_revision();
        match self.turns.get_mut(&generation)? {
            DeclarationSlot::Committed(turn) => Some(turn),
            DeclarationSlot::CertifiedAuthored { turn, .. } => Some(turn),
            DeclarationSlot::Joined(joined) => Some(&mut joined.turn),
            DeclarationSlot::Recovered(recovered) => Some(&mut recovered.turn),
            DeclarationSlot::Reserved => None,
        }
    }

    pub fn pop_latest_committed(&mut self, generation: Generation) -> bool {
        if generation != self.generation()
            || !matches!(
                self.turns.get(&generation),
                Some(DeclarationSlot::Committed(_))
            )
        {
            return false;
        }
        self.turns.insert(generation, DeclarationSlot::Reserved);
        self.advance_revision();
        true
    }

    fn latest_committed(&self) -> Option<Generation> {
        self.turns.iter().rev().find_map(|(generation, slot)| {
            matches!(
                slot,
                DeclarationSlot::Committed(_)
                    | DeclarationSlot::CertifiedAuthored { .. }
                    | DeclarationSlot::Joined(_)
                    | DeclarationSlot::Recovered(_)
            )
            .then_some(*generation)
        })
    }

    fn is_joined(&self, generation: Generation) -> bool {
        matches!(
            self.turns.get(&generation),
            Some(DeclarationSlot::Joined(_) | DeclarationSlot::Recovered(_))
        )
    }

    /// `tip`'s parent chain, oldest first, INCLUSIVE of `tip` itself — the
    /// walk order every scoping fold below applies turns in (a later turn's
    /// hide/retract must be applied after the earlier turn it shadows).
    /// Empty for `Generation(0)`. `pub(crate)` so `SessionLib` (a sibling
    /// module) can build its own scope-keyed folds (e.g. `decl_value_names_in`)
    /// on the same walk.
    pub(crate) fn chain_from_root(&self, tip: Generation) -> Vec<Generation> {
        let mut chain = Vec::new();
        let mut cur = (tip.0 > 0).then_some(tip);
        while let Some(g) = cur {
            chain.push(g);
            cur = self
                .turn(g)
                .expect("scope tip must reference a committed declaration")
                .parent;
        }
        chain.reverse();
        chain
    }

    /// The currently in-scope declaration heads (value/type/class names) paired
    /// with the generation of their LATEST defining turn (latest-wins across
    /// turns, mirroring the eval-time module scoping), as seen from `tip` —
    /// walks `tip`'s parent chain rather than assuming the log is one flat
    /// history. Lets a caller with several tips in flight (one per scope)
    /// query each independently. Backs the persistent declaration environment half of the
    /// `tidepool://session/bindings` live-state snapshot.
    #[must_use]
    pub fn current_heads_at(&self, tip: Generation) -> Vec<(String, u64)> {
        self.current_items_at(tip)
            .into_iter()
            .map(|(item, generation)| (item.head_name().to_string(), generation))
            .collect()
    }

    /// The exact current export items paired with their defining generation.
    /// This is the metadata-preserving form of [`Self::current_heads_at`].
    #[must_use]
    pub fn current_items_at(&self, tip: Generation) -> Vec<(ExportItem, u64)> {
        let mut map: std::collections::BTreeMap<
            (
                tidepool_toolchain::declaration_join::ExportNamespace,
                String,
            ),
            (ExportItem, u64),
        > = std::collections::BTreeMap::new();
        for g in self.chain_from_root(tip) {
            let turn = self
                .turn(g)
                .expect("scope chain contains only committed nodes");
            for r in &turn.retracts {
                map.retain(|(namespace, name), _| !r.selects(*namespace, name));
            }
            for item in &turn.items {
                map.insert(
                    (item.head_namespace(), item.head_name().to_string()),
                    (item.clone(), g.0),
                );
            }
        }
        map.into_values().collect()
    }

    /// The retained type of a value export in its exact defining generation.
    /// A generation is part of the lookup so a fallback result can never be
    /// attached to a same-named declaration that shadowed it meanwhile.
    #[must_use]
    pub fn value_type_at(&self, generation: Generation, name: &str) -> Option<&str> {
        let turn = self.turn(generation)?;
        turn.value_types.get(name).map(String::as_str)
    }

    /// Retain fallback inspection results only while the supplied generation
    /// remains the visible definition at `tip`.
    pub fn retain_value_types_at(&mut self, tip: Generation, types: &[(String, u64, String)]) {
        let current = self
            .current_items_at(tip)
            .into_iter()
            .filter_map(|(item, generation)| match item {
                ExportItem::Value { name } => Some((name, generation)),
                ExportItem::Type { .. } | ExportItem::Class { .. } => None,
            })
            .collect::<BTreeMap<_, _>>();
        for (name, generation, ty) in types {
            if current.get(name) != Some(generation) || *generation == 0 {
                continue;
            }
            if let Some(turn) = self.turn_mut(Generation(*generation)) {
                turn.value_types
                    .entry(name.clone())
                    .or_insert_with(|| ty.clone());
            }
        }
    }

    /// Exact export items visible at `tip`, latest definition winning by head
    /// name and retractions removing the head. Unlike `current_heads_at`, this
    /// preserves GHC's constructor/method metadata for selective imports.
    pub(crate) fn exports_at(&self, tip: Generation) -> Vec<ExportItem> {
        let mut exports = Vec::new();
        for g in self.chain_from_root(tip) {
            let turn = self
                .turn(g)
                .expect("scope chain contains only committed nodes");
            for retracted in &turn.retracts {
                exports.retain(|item: &ExportItem| !retracted.selects_item(item));
            }
            extend_exports_by_head(&mut exports, &turn.items);
        }
        exports
    }

    /// The declaration source texts of a **replayable** notebook skeleton: turn
    /// sources in log order, but with fully-superseded turns dropped so a name
    /// redefined across SEPARATE turns emits only its LATEST definition.
    ///
    /// A flat `:program` replay concatenates top-level decls, so two turns that
    /// both define `rf` (`rf x = x+1` then `rf x = x+2`) would emit two
    /// conflicting `rf` equations — an overlapping-clause pair GHC rejects as
    /// "multiple declarations of rf". The eval-time scope already resolves this
    /// latest-wins (see `cumulative_exports_before`); this mirrors that rule
    /// for the flat repaint.
    ///
    /// The rule matches the module scoping: a turn is dropped iff **every** head
    /// it introduces (by head name) is redefined in a *later* turn. Consequences:
    /// - Cross-turn redefinition (`rf` then `rf`): the earlier turn is fully
    ///   superseded → dropped; only the latest `rf` source is emitted.
    /// - A genuine multi-clause function in ONE turn (`f 0 = ..\nf n = ..`) is a
    ///   single source with one head → never self-supersedes → emitted verbatim,
    ///   both clauses preserved.
    /// - A turn with no exportable head (e.g. a bare `instance`) is always kept.
    ///
    /// Limitation (matches the "one declaration per item" idiom's blind spot): a
    /// turn co-defining a later-redefined head *and* a still-live head is NOT
    /// fully superseded, so it is kept — the live head is preserved faithfully,
    /// but the co-defined stale head can still duplicate in the flat replay. The
    /// runtime resolves that via the gen-versioned module split; a flat skeleton
    /// cannot, short of re-parsing the source per-binder. Single-head turns (the
    /// documented norm) never hit this.
    #[must_use]
    pub fn replayable_sources(&self) -> Vec<&str> {
        let chain = self.chain_from_root(self.latest_committed().unwrap_or(Generation(0)));
        let mut out: Vec<&str> = Vec::new();
        for (pos, &g) in chain.iter().enumerate() {
            let turn = self
                .turn(g)
                .expect("scope chain contains only committed nodes");
            let heads: Vec<_> = turn
                .items
                .iter()
                .map(|item| (item.head_namespace(), item.head_name()))
                .collect();
            // A turn's source is dropped once every head it introduced is later
            // redefined OR retracted (later IN THIS CHAIN) — the
            // migrated/superseded decl must not reappear in a flat `:program`
            // replay.
            let fully_superseded = !heads.is_empty()
                && heads.iter().all(|h| {
                    chain[pos + 1..].iter().any(|&later_g| {
                        let later = self
                            .turn(later_g)
                            .expect("scope chain contains only committed nodes");
                        later
                            .items
                            .iter()
                            .any(|it| (it.head_namespace(), it.head_name()) == *h)
                            || later.retracts.iter().any(|r| r.selects(h.0, h.1))
                    })
                });
            if !fully_superseded {
                out.extend(turn.sources.iter().map(String::as_str));
            }
        }
        out
    }
}

impl Default for DeclLog {
    fn default() -> Self {
        Self::new()
    }
}

/// The import preamble the generated session module needs so user declarations
/// type-check (the same surface evals see). Held as a parameter so the
/// standalone Lane-A test uses a small pure surface while the full server can
/// later pass the stable effect preamble unchanged.
#[derive(Clone, Debug)]
pub struct ModuleEnv {
    /// The `{-# LANGUAGE … #-}` pragma block (one line, no trailing newline).
    pub pragmas: String,
    /// Import lines (without the leading `import` keyword is NOT assumed —
    /// each entry is a full `import …` line), emitted before the prior-gen import.
    pub imports: Vec<String>,
}

impl ModuleEnv {
    /// Hide facade-replaced names from one unqualified import while retaining
    /// every other declaration and all instances from that module.
    pub fn hide_unqualified_import_names(&mut self, module: &str, names: &[&str]) {
        if names.is_empty() {
            return;
        }
        let plain = format!("import {module}");
        let replacement = format!("{plain} hiding ({})", names.join(", "));
        for import in &mut self.imports {
            if import == &plain {
                *import = replacement.clone();
            }
        }
    }

    /// A minimal **lens-free** pure surface sufficient for standalone
    /// declarations: the JIT-safe `T.` text vocabulary (`Tidepool.Data.Text`)
    /// and `Map.`, over the base `Prelude`. Deliberately avoids
    /// `Tidepool.Prelude` (which pulls `Control.Lens`, demanding the
    /// `with-packages` GHC) so the standalone declaration REPL compiles
    /// against the plain toolchain. The full server passes its own
    /// stable effect [`ModuleEnv`] instead.
    #[must_use]
    pub fn standalone_default() -> ModuleEnv {
        ModuleEnv {
            pragmas: super::standalone_declaration_pragmas(),
            imports: vec![
                "import qualified Tidepool.Data.Text as T".to_string(),
                "import qualified Data.Map.Strict as Map".to_string(),
            ],
        }
    }
}

/// A rendered session-library module: its name and source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedModule {
    /// The generation-versioned module name (`Tidepool.Session.Lib.G<g>`).
    pub module: SessionModule,
    /// The rendered module's full Haskell source text.
    pub source: String,
    /// Number of generated lines (pragmas, header, imports) before the user's
    /// declaration text — the offset for mapping GHC's line numbers back to
    /// item-relative ones (see [`crate::diag::render_diagnostics`]).
    pub body_line: usize,
    /// True when pragma/import hoisting REMOVED lines from the user's source,
    /// making the offset mapping inexact — coordinate remapping is skipped.
    pub hoisted_lines: bool,
}

/// Rewrite one `env.imports` line so no name in `all_session_heads` reaches
/// scope through it. Three shapes, because GHC allows at most one of an
/// explicit import list and a `hiding` clause per import:
/// - `import M hiding (…)` — merge the heads into the existing clause.
/// - `import M (a, b)` — SUBTRACT colliding entries from the list (appending
///   `hiding` here would be a parse error); an emptied list stays as
///   `import M ()`, which is valid and imports nothing but instances.
/// - `import M` — append a `hiding (…)` clause.
///   A `import qualified …` line is returned unchanged — qualified names can
///   never collide with an unqualified session decl. Empty `all_session_heads`
///   also returns the line unchanged (no session decls yet to guard against).
pub(super) fn hide_session_heads(imp: &str, all_session_heads: &[&ExportItem]) -> String {
    if all_session_heads.is_empty() || import_is_qualified(imp) {
        return imp.to_string();
    }
    let mut hides: Vec<String> = all_session_heads.iter().map(|p| p.render_entry()).collect();
    let base = if let Some(hiding_at) = imp.find(" hiding (") {
        let prefix = &imp[..hiding_at];
        let list_start = hiding_at + " hiding (".len();
        if let Some(list_end) = imp.rfind(')') {
            let existing = &imp[list_start..list_end];
            hides.extend(
                split_top_level_commas(existing)
                    .into_iter()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
            );
        }
        prefix.to_string()
    } else {
        let head_names: Vec<&str> = all_session_heads.iter().map(|p| p.head_name()).collect();
        if let Some(rewritten) = subtract_import_list_names(imp, &head_names) {
            return rewritten;
        }
        imp.to_string()
    };
    hides.sort();
    hides.dedup();
    format!("{base} hiding ({})", hides.join(", "))
}

fn import_is_qualified(line: &str) -> bool {
    line.split('(')
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .any(|token| token == "qualified")
}

/// Subtract `names` from an unqualified `import M (a, b)` explicit-list line.
/// GHC forbids combining an explicit list with a `hiding` clause, so shadowing
/// a listed name means REMOVING its entry; an emptied list stays `import M ()`
/// (valid — imports only instances). An entry collides when its head
/// identifier (the text before any `(..)`/constructor suffix; the whole
/// `(op)` for an operator entry) matches a name, bare or parenthesized.
///
/// Returns `None` when the line is not an explicit-list import (qualified,
/// carries a `hiding` clause, or has no list) — callers fall back to their
/// `hiding`-clause handling. Shared by the session decl-module renderer
/// ([`hide_session_heads`]) and `tidepool-repl`'s per-turn eval-preamble
/// patching, so the two shadowing stores can't drift on this shape again.
#[must_use]
pub fn subtract_import_list_names(line: &str, names: &[&str]) -> Option<String> {
    let t = line.trim_start();
    if !t.starts_with("import ") || import_is_qualified(t) || line.contains(" hiding (") {
        return None;
    }
    let (open, close) = (line.find('(')?, line.rfind(')')?);
    if open >= close {
        return None;
    }
    let kept: Vec<&str> = split_top_level_commas(&line[open + 1..close])
        .into_iter()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .filter(|e| {
            let head = match e.split('(').next().map(str::trim) {
                Some("") | None => *e, // operator entry like `(<+>)`
                Some(h) => h,
            };
            !names.iter().any(|n| *n == head || format!("({n})") == head)
        })
        .collect();
    Some(format!(
        "{}({}){}",
        &line[..open],
        kept.join(", "),
        &line[close + 1..]
    ))
}

/// Split an import/export list on commas at paren depth 0, so entries like
/// `Foo(A, B)` survive intact.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// The export items in scope (and re-exported) by `Lib.G<g-1>`, i.e. after
/// applying shadowing across turns `0..g-1`. A later turn redefines a prior item
/// iff their **head names** match (a function redefines a same-named function; a
/// `data Foo` redefines a prior `data Foo` — the spec's reshape case), removing
/// the prior item before adding its own (latest-wins).
///
/// Matching is deliberately **head-name only**, NOT every introduced identifier:
/// hiding on any shared constructor name would silently nuke an *unrelated*
/// prior type that merely reused a constructor name. Cross-type constructor
/// reuse instead surfaces as a loud GHC conflicting-export error at compile —
/// the honest outcome for a genuinely ambiguous program; this renderer does not
/// attempt to disambiguate it. Reshape-coexistence of a redefined type lives
/// in the gen-versioned module split, not here.
fn cumulative_exports_before(log: &DeclLog, gen_one_based: usize) -> Vec<ExportItem> {
    // The generation whose exports we're folding forward from: `gen_one_based`'s
    // own `parent` link when it already exists in the log; otherwise (a
    // one-past-the-end query, e.g. "what would the next flat turn inherit")
    // fall back to the log's current tip — the pre-tree, positional meaning of
    // "everything defined so far".
    let parent = if log.turn(Generation(gen_one_based as u64)).is_some() {
        log.turn(Generation(gen_one_based as u64))
            .expect("only committed declarations can be rendered")
            .parent
    } else {
        log.latest_committed()
    };
    let chain = match parent {
        Some(p) => log.chain_from_root(p),
        None => Vec::new(),
    };
    let mut acc: Vec<ExportItem> = Vec::new();
    for g in chain {
        let turn = log
            .turn(g)
            .expect("scope chain contains only committed nodes");
        // A turn removes prior exports it either redefines OR retracts; then
        // re-adds its own. (A retraction adds nothing.)
        acc.retain(|prior| !turn.retracts.iter().any(|r| r.selects_item(prior)));
        extend_exports_by_head(&mut acc, &turn.items);
    }
    acc
}

/// Render a committed generation as a `Tidepool.Session.Lib.G<gen>` module.
/// A reserved identity has no source to render.
///
/// This turn's (and prior turns') decl heads are always hidden from every
/// unqualified `env.imports` line (`Library`, `Tidepool.Prelude`, …) so a decl
/// reusing a name those modules also export (e.g. `over`, which
/// `Tidepool.Prelude` re-exports from `Control.Lens`) shadows gracefully
/// instead of an "ambiguous occurrence" — GHCi parity for ANY session decl,
/// pure or genuine: a pure `let`/`<-` bind promoted into a decl for
/// GHCi-parity type generalization (see `tidepool-repl`'s
/// `try_pure_bind_as_decl`) must shadow a colliding wildcard-imported name
/// exactly as a genuine top-level declaration would, so pure and effectful
/// binds stay interchangeable.
#[must_use]
pub fn render_module(log: &DeclLog, gen: Generation, env: &ModuleEnv) -> RenderedModule {
    render_module_with_vals(log, gen, env, &[])
}

/// [`render_module`] plus `import`ing each of `val_modules` unqualified —
/// the live `Tidepool.Session.Val.G<g>` persistent bindings (one per still-live
/// name, newest gen only) — so a decl can reference a prior `x <- e`/`let x = e`
/// session value the same way a genuine GHCi top-level definition would. The
/// caller must ALSO pass the same module names as `--inject-val` to the extract
/// invocation that validates this module (see `SessionLib::validate_candidate`)
/// — the import line alone doesn't make GHC able to find the `.hi`.
#[must_use]
pub fn render_module_with_vals(
    log: &DeclLog,
    gen: Generation,
    env: &ModuleEnv,
    val_modules: &[String],
) -> RenderedModule {
    let g = gen.0 as usize;
    assert!(
        log.turn(gen).is_some(),
        "render_module: generation {g} is not committed"
    );
    let module = SessionModule::lib(gen);
    assert!(
        !log.is_joined(gen),
        "certified join interfaces have no source renderer"
    );
    let this = log
        .turn(gen)
        .expect("only committed declarations can be rendered");
    let prior = cumulative_exports_before(log, g);

    let mut hoisted_imports = this.external_imports.source_lines();
    hoisted_imports.extend(
        this.normalized
            .prologue
            .imports
            .iter()
            .map(|import| import.source.clone()),
    );
    let merged_pragmas = format!(
        "{}\n{}",
        env.pragmas,
        this.normalized.prologue.pragma_text()
    );
    let stripped_sources = [&this.normalized.body];

    // Typed heads this turn defines drive the prior-generation hiding list.
    let new_heads: Vec<_> = this
        .items
        .iter()
        .map(|item| (item.head_namespace(), item.head_name()))
        .collect();
    // Hide from the prior-gen import every head this turn REDEFINES or RETRACTS.
    // For a retraction the name is still in `prior` (retracts take effect for
    // LATER gens via `cumulative_exports_before`); hiding it here drops it from
    // this gen's `import Prev hiding (…)` and — since `module Prev` only
    // re-exports in-scope names — from the re-export too, so the name is gone.
    let hidden_prior: Vec<&ExportItem> = prior
        .iter()
        .filter(|p| {
            new_heads.contains(&(p.head_namespace(), p.head_name()))
                || this.retracts.iter().any(|r| r.selects_item(p))
        })
        .collect();

    // Every head this session has ever (re)defined, prior gens + this turn —
    // guards every UNQUALIFIED `env.imports` line against colliding with a
    // session's own decls (see `hide_session_heads` below). Without this, a
    // decl defining a name some wildcard-imported module also exports (e.g.
    // `data Hit` vs. the project `Library` facade's `Hit`, or `over` vs.
    // `Tidepool.Prelude`'s Control.Lens re-export) becomes an "ambiguous
    // occurrence" — the same collision class `hide_module_names`
    // (tidepool-repl) guards on the stmt-preamble side, ported here to EVERY
    // unqualified decl-module import (not just `Library`) since the decl env
    // always carries the full `Tidepool.Prelude` surface (see
    // `session_decl_module_env`). Applies to ANY session decl, pure or
    // genuine — a pure-bind-promoted decl shadows exactly like a real one.
    let all_session_heads: Vec<&ExportItem> = prior.iter().chain(this.items.iter()).collect();

    let prev_module = this.parent.map(|parent| {
        log.projection_at(parent)
            .map(|projection| projection.module_name().to_owned())
            .or_else(|| {
                log.recovered_at(parent)
                    .map(|recovered| recovered.evidence.root().module.clone())
            })
            .unwrap_or_else(|| SessionModule::lib(parent).module_name())
    });

    let mut out = String::new();
    out.push_str(&merged_pragmas);
    out.push('\n');
    out.push_str(
        "-- GENERATED (Lane A) — accumulated session declarations. Do not edit;\n\
         -- regenerated as a pure function of the declaration log each turn.\n",
    );

    // Export list: re-export the prior gen (its non-hidden names) selectively,
    // then this turn's items explicitly.
    let mut exports: Vec<String> = Vec::new();
    if let Some(prev) = &prev_module {
        exports.push(format!("module {}", prev));
    }
    for item in &this.items {
        exports.push(item.render_entry());
    }
    out.push_str(&format!("module {} (", module.module_name()));
    if exports.is_empty() {
        out.push_str(") where\n");
    } else {
        out.push('\n');
        for (i, e) in exports.iter().enumerate() {
            let comma = if i + 1 < exports.len() { "," } else { "" };
            out.push_str(&format!("    {e}{comma}\n"));
        }
        out.push_str("  ) where\n");
    }

    // Standard imports, then the selective prior-gen import, then any user
    // imports hoisted from decl sources (deduped against the standard set).
    for imp in &env.imports {
        out.push_str(&hide_session_heads(imp, &all_session_heads));
        out.push('\n');
    }
    if let Some(prev) = &prev_module {
        if hidden_prior.is_empty() {
            out.push_str(&format!("import {}\n", prev));
        } else {
            let hides: Vec<String> = hidden_prior.iter().map(|p| p.render_entry()).collect();
            out.push_str(&format!("import {} hiding ({})\n", prev, hides.join(", ")));
        }
    }
    for imp in &hoisted_imports {
        if !env.imports.contains(imp) {
            out.push_str(imp);
            out.push('\n');
        }
    }
    for m in val_modules {
        out.push_str(&format!("import {m}\n"));
    }
    out.push('\n');

    // Normalized sources carry GHC LINE mappings. Legacy offset-based
    // diagnostics may only remap when source normalization preserved lines.
    let body_line = out.matches('\n').count();
    let hoisted_lines = stripped_sources
        .iter()
        .zip(&this.sources)
        .any(|(stripped, original)| stripped.lines().count() != original.lines().count());

    // Header syntax has already been separated by GHC.
    for src in &stripped_sources {
        let body = src.trim_end();
        if !body.is_empty() {
            out.push_str(body);
            out.push_str("\n\n");
        }
    }

    RenderedModule {
        module,
        source: out,
        body_line,
        hoisted_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(name: &str) -> ExportItem {
        ExportItem::Value { name: name.into() }
    }
    fn ty(name: &str, cons: &[&str]) -> ExportItem {
        ExportItem::Type {
            name: name.into(),
            cons: cons.iter().map(|s| (*s).into()).collect(),
        }
    }
    /// `parent` is filled in by [`push_chained`] at push time — building it
    /// here would need the log this turn hasn't been pushed to yet.
    fn turn(src: &str, items: Vec<ExportItem>) -> DeclTurn {
        DeclTurn {
            normalized: super::super::DeclarationSource {
                prologue: Default::default(),
                body: src.into(),
            },
            external_imports: SourceImports::new(),
            sources: vec![src.into()],
            workbench_imports: SourceImports::new(),
            items,
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: None,
        }
    }
    fn header_turn(
        body: &str,
        items: Vec<ExportItem>,
        pragmas: &[&str],
        imports: &[&str],
    ) -> DeclTurn {
        use super::super::{CellSourceSpan, LocatedImport, LocatedPragma, PragmaKind};
        let mut turn = turn(body, items);
        let span = CellSourceSpan {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 2,
        };
        turn.normalized.prologue.pragmas = pragmas
            .iter()
            .map(|source| LocatedPragma {
                kind: PragmaKind::Language,
                span,
                source: (*source).into(),
            })
            .collect();
        turn.normalized.prologue.imports = imports
            .iter()
            .map(|source| LocatedImport {
                span,
                source: (*source).into(),
            })
            .collect();
        turn
    }

    /// A pure-retraction turn: removes `names` from the persistent declaration environment, no source.
    fn retract_turn(names: &[&str]) -> DeclTurn {
        DeclTurn {
            normalized: Default::default(),
            external_imports: SourceImports::new(),
            sources: Vec::new(),
            workbench_imports: SourceImports::new(),
            items: Vec::new(),
            value_types: BTreeMap::new(),
            retracts: names.iter().map(|s| (*s).into()).collect(),
            parent: None,
        }
    }

    /// Push `turn` as the next turn in the log's FLAT chain — sets `parent` to
    /// the log's current tip (or `None` for the very first turn), mirroring
    /// the pre-tree positional behavior every existing test below assumes.
    /// The tree tests further down set `parent` explicitly instead and call
    /// `log.push` directly.
    fn push_chained(log: &mut DeclLog, mut t: DeclTurn) -> Generation {
        t.parent = (log.generation().0 > 0).then_some(log.generation());
        log.push(t)
    }

    #[test]
    fn admitted_export_selection_preserves_original_groups_and_exact_namespace_replacements() {
        use tidepool_toolchain::declaration_join::{
            DeclarationExport, DeclarationKind, ExportIdentity, ExportNamespace,
        };
        let export = |generation, namespace, occurrence: &str| DeclarationExport {
            kind: if namespace == ExportNamespace::Type {
                DeclarationKind::Class
            } else {
                DeclarationKind::Value
            },
            head: ExportIdentity {
                unit: "main".into(),
                module: SessionModule::lib(Generation(generation)).module_name(),
                namespace,
                occurrence: occurrence.into(),
                record_parent: None,
            },
            children: vec![],
        };
        let mut original_type = export(1, ExportNamespace::Type, "Same");
        original_type
            .children
            .push(export(1, ExportNamespace::Value, "method").head);
        original_type
            .children
            .push(export(1, ExportNamespace::Type, "Family").head);
        let original_value = export(1, ExportNamespace::Value, "Same");
        let inherited = vec![original_type.clone(), original_value.clone()];
        let unrelated = export(3, ExportNamespace::Type, "M2JoinA");
        let selection = select_authored_exports(&inherited, &[], &[unrelated.clone()]);
        assert_eq!(
            selection,
            vec![original_type.clone(), original_value.clone(), unrelated]
        );
        let shadow = export(4, ExportNamespace::Type, "Same");
        let selection = select_authored_exports(&selection, &[], &[shadow.clone()]);
        assert!(!selection.contains(&original_type));
        assert!(selection.contains(&original_value));
        assert!(selection.contains(&shadow));
        let selection = select_authored_exports(
            &selection,
            &[DeclarationRetraction::Head {
                namespace: ExportNamespace::Value,
                occurrence: "Same".into(),
            }],
            &[],
        );
        assert!(!selection.contains(&original_value));
        assert!(selection.contains(&shadow));
        let selection = select_authored_exports(
            &selection,
            &[DeclarationRetraction::Name("Same".into())],
            &[],
        );
        assert!(!selection.iter().any(|item| item.head.occurrence == "Same"));
        assert_eq!(inherited, vec![original_type, original_value]);
    }

    #[test]
    fn retained_value_types_are_fenced_by_visible_generation() {
        let mut log = DeclLog::new();
        let first = push_chained(&mut log, turn("answer = 1", vec![val("answer")]));
        log.retain_value_types_at(first, &[("answer".into(), first.0, "Int".into())]);
        assert_eq!(log.value_type_at(first, "answer"), Some("Int"));

        let second = push_chained(&mut log, turn("answer = True", vec![val("answer")]));
        log.retain_value_types_at(
            second,
            &[
                ("answer".into(), first.0, "stale".into()),
                ("answer".into(), second.0, "Bool".into()),
            ],
        );
        assert_eq!(log.value_type_at(first, "answer"), Some("Int"));
        assert_eq!(log.value_type_at(second, "answer"), Some("Bool"));

        // A fork still viewing the first tip retains its own exact generation,
        // and cannot overwrite metadata already owned by that generation.
        log.retain_value_types_at(first, &[("answer".into(), first.0, "Wrong".into())]);
        assert_eq!(log.value_type_at(first, "answer"), Some("Int"));
        assert_eq!(log.value_type_at(second, "answer"), Some("Bool"));
    }

    #[test]
    fn shadowing_preserves_qualified_names_and_nested_hiding_entries() {
        let after = val("after");
        for import in ["import qualified M as Q", "import M qualified as Q (after)"] {
            assert_eq!(hide_session_heads(import, &[&after]), import);
        }
        assert_eq!(
            hide_session_heads("import M hiding (Choice(Left, Right), old)", &[&after]),
            "import M hiding (Choice(Left, Right), after, old)"
        );
    }

    #[test]
    fn explicit_import_list_subtracts_colliding_head() {
        // `import M (a, b) hiding (x)` is a GHC parse error — a colliding name
        // must be SUBTRACTED from an explicit list, never `hiding`-appended.
        // The live case: the eval preamble's `import Tidepool.Shell (sh)` vs a
        // session decl named `sh`.
        let sh = val("sh");
        let heads = [&sh];
        assert_eq!(
            hide_session_heads("import Tidepool.Shell (sh)", &heads),
            "import Tidepool.Shell ()"
        );
        let who = val("who");
        let heads = [&who];
        assert_eq!(
            hide_session_heads("import Tidepool.Shell (sh)", &heads),
            "import Tidepool.Shell (sh)"
        );
    }

    #[test]
    fn subtract_returns_none_for_non_list_shapes() {
        // hiding-clause, qualified, and clause-less imports are the callers'
        // hiding-path business, not a list to subtract from.
        let names = ["sh"];
        assert_eq!(
            subtract_import_list_names("import Tidepool.Prelude hiding (error)", &names),
            None
        );
        assert_eq!(
            subtract_import_list_names("import qualified Tidepool.Shell as Shell", &names),
            None
        );
        assert_eq!(
            subtract_import_list_names("import Tidepool.Effects", &names),
            None
        );
        assert_eq!(
            subtract_import_list_names("default (Int, Text)", &names),
            None
        );
    }

    #[test]
    fn explicit_import_list_keeps_noncolliding_entries_and_nested_commas() {
        let empty = val("empty");
        let heads = [&empty];
        assert_eq!(
            hide_session_heads("import Data.Map (Map(Bin, Tip), empty, lookup)", &heads),
            "import Data.Map (Map(Bin, Tip), lookup)"
        );
        // Operator entries match through op_wrap.
        let op = val("<+>");
        let heads = [&op];
        assert_eq!(
            hide_session_heads("import M ((<+>), pure)", &heads),
            "import M (pure)"
        );
    }

    #[test]
    fn operator_value_is_parenthesized_in_exports() {
        // A session_def'd operator like `(.+)` must export as `(.+)`, not a bare
        // `.+` (which is a GHC parse error in an export list).
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("a .+ b = a + b", vec![val(".+")]));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        assert!(
            r.source.contains("(.+)"),
            "operator export must be parenthesized:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("\n    .+\n"),
            "bare operator in export list:\n{}",
            r.source
        );
        // And the prior-gen `hiding` clause must parenthesize too (redefine `.+`).
        push_chained(&mut log, turn("a .+ b = a - b", vec![val(".+")]));
        let r2 = render_module(&log, Generation(2), &ModuleEnv::standalone_default());
        assert!(
            r2.source.contains("hiding ((.+))"),
            "hiding clause must parenthesize:\n{}",
            r2.source
        );
    }

    #[test]
    fn first_gen_has_no_prior_import() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("slug t = T.toLower t", vec![val("slug")]));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        assert_eq!(r.module.module_name(), "Tidepool.Session.Lib.G1");
        assert!(r.source.contains("module Tidepool.Session.Lib.G1 ("));
        assert!(r.source.contains("    slug\n"));
        assert!(!r.source.contains("import Tidepool.Session.Lib.G0"));
        assert!(r.source.contains("slug t = T.toLower t"));
    }

    #[test]
    fn second_gen_reexports_prior_when_no_redef() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("slug t = t", vec![val("slug")]));
        push_chained(&mut log, turn("shout t = T.toUpper t", vec![val("shout")]));
        let r = render_module(&log, Generation(2), &ModuleEnv::standalone_default());
        // No redefinition → plain import + module re-export.
        assert!(r.source.contains("import Tidepool.Session.Lib.G1\n"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G1,"));
        assert!(r.source.contains("    shout"));
        // No redefinition → the prior-gen import carries no `hiding` clause.
        assert!(!r.source.contains("Session.Lib.G1 hiding"));
    }

    #[test]
    fn redefined_function_is_hidden_from_prior_import() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("slug t = t", vec![val("slug")]));
        push_chained(&mut log, turn("other t = t", vec![val("other")]));
        push_chained(
            &mut log,
            turn("slug t = T.replace \" \" \"-\" t", vec![val("slug")]),
        );
        let r = render_module(&log, Generation(3), &ModuleEnv::standalone_default());
        // G3 redefines slug → hide it from G2's re-export (latest-wins).
        assert!(r
            .source
            .contains("import Tidepool.Session.Lib.G2 hiding (slug)"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G2,"));
        // `other` (defined at G2, not redefined) stays re-exported transitively.
        assert!(!r.source.contains("    other"));
    }

    #[test]
    fn redefined_data_type_hides_with_dotdot_no_conflict() {
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            turn("data Foo = A | B", vec![ty("Foo", &["A", "B"])]),
        );
        push_chained(
            &mut log,
            turn("data Foo = X | A | B", vec![ty("Foo", &["X", "A", "B"])]),
        );
        let r2 = render_module(&log, Generation(2), &ModuleEnv::standalone_default());
        // The reshape hides the OLD Foo and its constructors, avoiding GHC's
        // conflicting-export error, and re-declares + exports the new shape.
        assert!(r2
            .source
            .contains("import Tidepool.Session.Lib.G1 hiding (Foo(..))"));
        assert!(r2.source.contains("    Foo(..)"));
        assert!(r2.source.contains("data Foo = X | A | B"));
        // G1 still renders standalone (old shape stays compilable / resolvable).
        let r1 = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        assert!(r1.source.contains("data Foo = A | B"));
        assert!(r1.source.contains("    Foo(..)"));
    }

    #[test]
    fn unrelated_constructor_reuse_does_not_hide_prior_type() {
        // Regression: head-name matching only. A later turn reusing a prior
        // type's CONSTRUCTOR name in a *different* type must NOT hide the prior
        // type — that would silently drop `Foo` and its sibling `B`.
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            turn("data Foo = A | B", vec![ty("Foo", &["A", "B"])]),
        );
        push_chained(
            &mut log,
            turn("data Bar = A | C", vec![ty("Bar", &["A", "C"])]),
        );
        let r = render_module(&log, Generation(2), &ModuleEnv::standalone_default());
        // Foo is NOT redefined → no `hiding (Foo(..))`; it stays re-exported.
        assert!(!r.source.contains("hiding (Foo(..))"));
        assert!(r.source.contains("import Tidepool.Session.Lib.G1\n"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G1,"));
        assert!(r.source.contains("    Bar(..)"));
    }

    #[test]
    fn multi_item_turn_hides_only_redefined_heads() {
        // A turn that both redefines `slug` and adds a fresh `Greeter` type:
        // only `slug` is hidden from the prior import; the new type is added.
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("slug t = t", vec![val("slug")]));
        push_chained(
            &mut log,
            turn(
                "slug t = T.toUpper t\ndata Greeter = Hi | Yo",
                vec![val("slug"), ty("Greeter", &["Hi", "Yo"])],
            ),
        );
        let r = render_module(&log, Generation(2), &ModuleEnv::standalone_default());
        assert!(r
            .source
            .contains("import Tidepool.Session.Lib.G1 hiding (slug)"));
        assert!(r.source.contains("    slug,"));
        assert!(r.source.contains("    Greeter(..)"));
    }

    #[test]
    fn replayable_sources_drops_cross_turn_redefinition() {
        // `rf x = x+1` then `rf x = x+2` in SEPARATE turns. A flat
        // `:program` replay must emit ONLY the latest `rf`, not both (the two
        // equations would be an overlapping-clause pair GHC rejects).
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("rf x = x + 1", vec![val("rf")]));
        push_chained(&mut log, turn("rf x = x + 2", vec![val("rf")]));
        let srcs = log.replayable_sources();
        assert_eq!(
            srcs,
            vec!["rf x = x + 2"],
            "only the latest rf definition survives the flat repaint"
        );
    }

    #[test]
    fn replayable_sources_preserves_multiclause_single_item() {
        // A GENUINE multi-clause function lives in ONE turn (one source, one
        // head). It must survive verbatim — both clauses — never treated as a
        // self-redefinition.
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            turn("f 0 = 0\nf n = n * f (n - 1)", vec![val("f")]),
        );
        let srcs = log.replayable_sources();
        assert_eq!(srcs, vec!["f 0 = 0\nf n = n * f (n - 1)"]);
        // And it still stands when an unrelated later turn is added.
        push_chained(&mut log, turn("g y = y", vec![val("g")]));
        assert_eq!(
            log.replayable_sources(),
            vec!["f 0 = 0\nf n = n * f (n - 1)", "g y = y"],
        );
    }

    #[test]
    fn replayable_sources_keeps_unredefined_and_orders_stably() {
        // Interleaved: define a, define b, redefine a. Latest `a` and the sole
        // `b` survive, in log order; the stale first `a` is dropped.
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("a = 1", vec![val("a")]));
        push_chained(&mut log, turn("b = 2", vec![val("b")]));
        push_chained(&mut log, turn("a = 3", vec![val("a")]));
        assert_eq!(log.replayable_sources(), vec!["b = 2", "a = 3"]);
    }

    #[test]
    fn replayable_sources_keeps_headless_turn() {
        // A turn with no exportable head (e.g. a bare instance) is always kept.
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            turn("instance Show Foo where show _ = \"foo\"", vec![]),
        );
        assert_eq!(
            log.replayable_sources(),
            vec!["instance Show Foo where show _ = \"foo\""],
        );
    }

    #[test]
    fn type_synonym_renders_bare_not_dotdot() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("type Name = T.Text", vec![ty("Name", &[])]));
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        assert!(r.source.contains("    Name\n"));
        assert!(!r.source.contains("Name(..)"));
    }

    #[test]
    fn user_language_pragma_hoisted_above_module_header() {
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            header_turn(
                "data Foo = Foo deriving (Eq, Show)",
                vec![ty("Foo", &["Foo"])],
                &["{-# LANGUAGE DeriveAnyClass #-}"],
                &[],
            ),
        );
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        let pragma_pos = r.source.find("{-# LANGUAGE").expect("pragma block present");
        let module_pos = r
            .source
            .find("module Tidepool.Session.Lib.G1")
            .expect("module header present");
        assert!(
            pragma_pos < module_pos,
            "LANGUAGE pragma must precede module header"
        );
        // DeriveAnyClass must appear in the merged pragma block (before the module line).
        assert!(
            r.source[..module_pos].contains("DeriveAnyClass"),
            "DeriveAnyClass must be in merged pragma block"
        );
        // The original pragma line must NOT appear in the body (after module header).
        let body = &r.source[module_pos..];
        assert!(
            !body.contains("{-# LANGUAGE DeriveAnyClass #-}"),
            "LANGUAGE pragma must be stripped from body"
        );
        // The declaration body must still be present.
        assert!(r.source.contains("data Foo = Foo deriving (Eq, Show)"));
    }

    #[test]
    fn compiler_option_order_preserves_explicit_negation() {
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            header_turn(
                "data Bar = Bar",
                vec![ty("Bar", &["Bar"])],
                &[
                    "{-# LANGUAGE NoOverloadedStrings #-}",
                    "{-# LANGUAGE OverloadedStrings #-}",
                ],
                &[],
            ),
        );
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        let module_pos = r.source.find("module Tidepool.Session.Lib.G1").unwrap();
        let preamble = &r.source[..module_pos];
        let disabled = preamble
            .find("{-# LANGUAGE NoOverloadedStrings #-}")
            .unwrap();
        let enabled = preamble.find("{-# LANGUAGE OverloadedStrings #-}").unwrap();
        assert!(disabled < enabled);
    }

    #[test]
    fn user_import_hoisted_to_import_section() {
        // An `import` inside a session_def body must be lifted into the module
        // header (import section), not left in the declaration body — a
        // body-position import is a GHC parse error.
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            header_turn(
                "toUpper' c = toUpper c",
                vec![val("toUpper'")],
                &[],
                &["import Data.Char (toUpper)"],
            ),
        );
        let r = render_module(&log, Generation(1), &ModuleEnv::standalone_default());
        let module_pos = r
            .source
            .find("module Tidepool.Session.Lib.G1")
            .expect("module header present");
        let import_pos = r
            .source
            .find("import Data.Char (toUpper)")
            .expect("import must appear in output");
        let decl_pos = r
            .source
            .find("toUpper' c = toUpper c")
            .expect("decl body present");
        // Import must be in the header region (after module line, before decl body).
        assert!(
            import_pos > module_pos,
            "import must appear after module header:\n{}",
            r.source
        );
        assert!(
            import_pos < decl_pos,
            "import must appear before declaration body:\n{}",
            r.source
        );
        // The import must NOT reappear inside the declaration body.
        assert!(
            !r.source[decl_pos..].contains("import Data.Char"),
            "import must be stripped from declaration body:\n{}",
            r.source
        );
    }

    // --- Retraction (a name leaving the persistent declaration environment on decl→value migration) ---

    fn heads(log: &DeclLog) -> Vec<String> {
        log.current_heads_at(log.generation())
            .into_iter()
            .map(|(h, _)| h)
            .collect()
    }

    #[test]
    fn retraction_removes_name_from_every_scoping_view() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("findings = []", vec![val("findings")]));
        push_chained(&mut log, turn("keep t = t", vec![val("keep")]));
        // Before retraction: both are live in every view.
        assert_eq!(heads(&log), vec!["findings", "keep"]);
        let before = cumulative_exports_before(&log, log.generation().0 as usize + 1);
        assert!(before.iter().any(|e| e.head_name() == "findings"));

        // findings migrate to the persistent binding store → retract them.
        push_chained(&mut log, retract_turn(&["findings"]));

        // current_heads, cumulative exports, and decl replay all drop it;
        // `keep` is untouched.
        assert_eq!(heads(&log), vec!["keep"]);
        let after = cumulative_exports_before(&log, log.generation().0 as usize + 1);
        assert!(!after.iter().any(|e| e.head_name() == "findings"));
        assert!(after.iter().any(|e| e.head_name() == "keep"));
        // The migrated decl's source is dropped from a flat replay.
        let replay = log.replayable_sources();
        assert!(!replay.iter().any(|s| s.contains("findings = []")));
        assert!(replay.iter().any(|s| s.contains("keep t = t")));
    }

    #[test]
    fn retraction_turn_hides_name_from_rendered_module() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("findings = []", vec![val("findings")]));
        push_chained(&mut log, turn("keep t = t", vec![val("keep")]));
        push_chained(&mut log, retract_turn(&["findings"]));
        let r = render_module(&log, Generation(3), &ModuleEnv::standalone_default());
        // The retraction shell hides `findings` from the prior-gen import (so
        // `module Prev` no longer re-exports it) and adds no new decl for it.
        assert!(
            r.source.contains("Session.Lib.G2 hiding (findings)"),
            "retraction must hide the name from the prior import:\n{}",
            r.source
        );
        assert!(
            !r.source.contains("\n    findings"),
            "retracted name must not appear in the export list:\n{}",
            r.source
        );
    }

    #[test]
    fn define_after_retraction_unretracts_latest_wins() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("findings = []", vec![val("findings")]));
        push_chained(&mut log, retract_turn(&["findings"]));
        assert!(!heads(&log).contains(&"findings".to_string()));
        // Re-defining the name brings it back (a later value→decl rebind).
        push_chained(&mut log, turn("findings = [1]", vec![val("findings")]));
        assert!(heads(&log).contains(&"findings".to_string()));
        let exports = cumulative_exports_before(&log, log.generation().0 as usize + 1);
        assert!(exports.iter().any(|e| e.head_name() == "findings"));
        assert!(log
            .replayable_sources()
            .iter()
            .any(|s| s.contains("findings = [1]")));
    }

    #[test]
    fn retracting_absent_name_leaves_exports_unchanged() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("keep t = t", vec![val("keep")]));
        let before = cumulative_exports_before(&log, log.generation().0 as usize + 1);
        push_chained(&mut log, retract_turn(&["never_defined"]));
        let after = cumulative_exports_before(&log, log.generation().0 as usize + 1);
        assert_eq!(before, after, "retracting an absent name is a no-op fold");
    }

    // --- Scope trees: `parent` as a tree edge, not a position ---

    #[test]
    fn root_only_chain_has_parent_equal_to_g_minus_1() {
        // Flat-chain proof (design doc "flat degeneracy is the back-compat
        // proof"): every ROOT-only turn's parent is exactly `g - 1` (`None`
        // for g == 1), and the rendered import/re-export names that same
        // generation — today's `Lib.G<g-1>` rendering, unchanged.
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("a = 1", vec![val("a")]));
        push_chained(&mut log, turn("b = 2", vec![val("b")]));
        push_chained(&mut log, turn("c = 3", vec![val("c")]));
        assert_eq!(log.turn(Generation(1)).unwrap().parent, None);
        assert_eq!(log.turn(Generation(2)).unwrap().parent, Some(Generation(1)));
        assert_eq!(log.turn(Generation(3)).unwrap().parent, Some(Generation(2)));

        let r = render_module(&log, Generation(3), &ModuleEnv::standalone_default());
        assert!(r.source.contains("import Tidepool.Session.Lib.G2\n"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G2,"));
    }

    #[test]
    fn reserved_join_identity_does_not_enter_lexical_chain_or_get_reused() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("public = 1", vec![val("public")]));
        let reserved = log.reserve();
        assert_eq!(reserved, Generation(2));
        assert!(log.turn(reserved).is_none());
        assert_eq!(log.replayable_sources(), vec!["public = 1"]);

        let mut private = turn("private = 2", vec![val("private")]);
        private.parent = Some(Generation(1));
        let private_generation = log.push(private);
        assert_eq!(private_generation, Generation(3));
        assert_eq!(
            log.chain_from_root(private_generation),
            vec![Generation(1), Generation(3)]
        );
        assert!(log.is_reserved(reserved));
        assert!(log.turn(reserved).is_none());
        assert_eq!(log.reserve(), Generation(4));
    }

    #[test]
    fn branch_turn_names_its_actual_parent_not_positional_g_minus_1() {
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("a = 1", vec![val("a")])); // gen 1
        push_chained(&mut log, turn("b = 2", vec![val("b")])); // gen 2
                                                               // A turn at position 3, chained from gen 1 (NOT gen 2) — a child
                                                               // scope that forked off before `b` was defined.
        log.push(DeclTurn {
            normalized: super::super::DeclarationSource {
                prologue: Default::default(),
                body: "a = 99".into(),
            },
            external_imports: SourceImports::new(),
            sources: vec!["a = 99".into()],
            workbench_imports: SourceImports::new(),
            items: vec![val("a")],
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: Some(Generation(1)),
        }); // gen 3, parent = 1

        let r = render_module(&log, Generation(3), &ModuleEnv::standalone_default());
        // Names its ACTUAL parent (gen 1) in both the hiding import and the
        // re-export, not the positional `g - 1` (gen 2).
        assert!(r
            .source
            .contains("import Tidepool.Session.Lib.G1 hiding (a)"));
        assert!(r.source.contains("module Tidepool.Session.Lib.G1,"));
        assert!(
            !r.source.contains("Lib.G2"),
            "must not reference the positional but non-parent gen 2:\n{}",
            r.source
        );
        // `b`, defined off the OTHER branch (gen 2), is invisible here — the
        // parent link is a tree edge, not a position.
        assert!(!r.source.contains("    b"));
    }

    #[test]
    fn sibling_turns_off_one_parent_each_shadow_independently() {
        // The design doc's worked example ("gens 5 and 6 both with parent
        // 4"), scaled down: two sibling turns off gen 1 each redefine
        // `helper`; neither's body leaks into the other's rendered module,
        // and both correctly hide the shared parent's `helper`.
        let mut log = DeclLog::new();
        push_chained(&mut log, turn("helper x = x", vec![val("helper")])); // gen 1
        log.push(DeclTurn {
            normalized: super::super::DeclarationSource {
                prologue: Default::default(),
                body: "helper x = x + 1".into(),
            },
            external_imports: SourceImports::new(),
            sources: vec!["helper x = x + 1".into()],
            workbench_imports: SourceImports::new(),
            items: vec![val("helper")],
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: Some(Generation(1)),
        }); // gen 2 (left sibling)
        log.push(DeclTurn {
            normalized: super::super::DeclarationSource {
                prologue: Default::default(),
                body: "helper x = x * 2".into(),
            },
            external_imports: SourceImports::new(),
            sources: vec!["helper x = x * 2".into()],
            workbench_imports: SourceImports::new(),
            items: vec![val("helper")],
            value_types: BTreeMap::new(),
            retracts: Vec::new(),
            parent: Some(Generation(1)),
        }); // gen 3 (right sibling)

        let left = render_module(&log, Generation(2), &ModuleEnv::standalone_default());
        let right = render_module(&log, Generation(3), &ModuleEnv::standalone_default());

        for r in [&left, &right] {
            assert!(r
                .source
                .contains("import Tidepool.Session.Lib.G1 hiding (helper)"));
            assert!(r.source.contains("    helper"));
        }
        assert!(left.source.contains("helper x = x + 1"));
        assert!(right.source.contains("helper x = x * 2"));
        assert!(!left.source.contains("x * 2"), "sibling body must not leak");
        assert!(
            !right.source.contains("x + 1"),
            "sibling body must not leak"
        );
    }

    #[test]
    fn declaration_signatures_are_rendered_verbatim() {
        let mut log = DeclLog::new();
        push_chained(
            &mut log,
            turn(
                "probe :: Eff effects Int\nprobe = pure 0",
                vec![val("probe")],
            ),
        );

        let rendered = render_module(
            &log,
            Generation(1),
            &ModuleEnv {
                pragmas: String::new(),
                imports: vec![
                    "import Tidepool.Effects.Core".into(),
                    "import Control.Monad.Freer (Eff)".into(),
                ],
            },
        );

        assert!(rendered.source.contains("probe :: Eff effects Int"));
        assert!(rendered.source.contains("probe = pure 0"));
    }
}
