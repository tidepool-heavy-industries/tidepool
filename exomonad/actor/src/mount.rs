use std::path::{Path, PathBuf};

use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{Generation, PrincipalId, SessionId, SessionModule};
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentError, ResidentSession, SessionCompileView,
    SessionRunContext, SourceImports,
};

use crate::ActorRef;

/// Immutable location of one actor incarnation in the resident Haskell
/// machine. The actor owns this mapping; callers select an actor, not an
/// independently assembled set of scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorPlacement {
    pub session: SessionId,
    pub resource_scope: RealmId,
    pub lexical_scope: ScopeId,
}

/// Exact model-visible declaration imports for one actor incarnation.
///
/// The only public constructor accepts materialized exact-export facades, so
/// actor startup cannot smuggle an ambient parent module or arbitrary import
/// string across the fresh-scope boundary. Standard Tidepool imports remain
/// part of the shared turn template rather than this actor-local membrane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActorSourceImports {
    imports: SourceImports,
}

impl ActorSourceImports {
    #[must_use]
    pub fn from_exact_facades<'a>(
        facades: impl IntoIterator<Item = &'a MaterializedFacade>,
    ) -> Self {
        let facades: Vec<_> = facades.into_iter().collect();
        Self {
            imports: SourceImports::from_specs(facades.iter().map(|facade| facade.module_name())),
        }
    }
}

/// How a host turns the checkout an actor is launched with into that actor's
/// own source layer.
///
/// One implementation per host, supplied by the composition root that owns the
/// run's source. The actor engine neither reads source roots nor knows where a
/// run keeps its revisions: it asks once, while the actor is being
/// constructed, for the include roots that actor alone compiles against, and
/// then names the actor those roots belong to. Everything after that is
/// structural — the layer is fixed on the actor's descriptor, and the source
/// service the host hands out for that principal is bound to the same layer,
/// so an actor cannot reach another actor's source by asking differently.
pub trait ActorSourceLayers: Send + Sync {
    /// The include roots for an actor launched with `worktrees`, ahead of
    /// every shared root. Empty when that checkout contributes no source of
    /// its own, which is the ordinary case.
    fn layer_include(&self, worktrees: &[String]) -> Vec<PathBuf>;

    /// Bind the layer [`Self::layer_include`] just answered to the actor that
    /// will compile against it, so that actor's own source calls act on
    /// exactly that layer and no other.
    fn bind(&self, actor: PrincipalId, worktrees: &[String]);

    /// Re-read and publish `actor`'s OWN layer, with `also_check` naming
    /// modules to pull into the checked closure beyond the configured list.
    ///
    /// The layer is the one [`Self::bind`] fixed for this principal and cannot
    /// be chosen per call, so a reload is scoped to the actor that asked and
    /// never upgrades a child. Hosts that install no layers answer
    /// [`SourceLayerReload::Unavailable`], which is also the default.
    fn reload(&self, actor: PrincipalId, also_check: &[String]) -> SourceLayerReload {
        let _ = (actor, also_check);
        SourceLayerReload::Unavailable("this host installs no source layers".into())
    }
}

/// What publishing one actor's own layer did, as the actor engine needs to
/// read it: enough to decide whether to recompile a spec, and enough to put in
/// a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLayerReload {
    /// The roots still hold the active revision's content. Nothing was
    /// rebuilt and nothing was republished.
    Unchanged { revision: String },
    /// A new revision is live for this actor's later compiles.
    Published {
        previous: String,
        revision: String,
        changed: Vec<String>,
    },
    /// The affected module graph did not typecheck. The previous revision is
    /// still active and the edited files are untouched on disk.
    Rejected {
        active: String,
        rejected: String,
        diagnostics: String,
    },
    /// This actor has no layer of its own to publish into.
    Unavailable(String),
}

/// The installed [`ActorSourceLayers`], shared by every actor in one forest.
pub type ActorSourceLayerResolver = std::sync::Arc<dyn ActorSourceLayers>;

/// An actor's exact, owned source-side compilation snapshot.
///
/// Construction validates the session and lexical scope against the actor
/// context before pairing them with the actor's explicit facade imports. A
/// compiler can therefore consume this value without separately carrying an
/// ambient session view or caller-selected import set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorCompileView {
    session: SessionCompileView,
    external: SourceImports,
    source_layer: std::sync::Arc<[PathBuf]>,
}

impl ActorCompileView {
    /// Add trusted imports supplied by the hosted workbench itself. These are
    /// source vocabulary, not ambient lexical ancestry.
    pub(crate) fn with_workbench_imports(mut self, imports: &SourceImports) -> Self {
        self.external.extend(imports);
        self
    }

    /// Extend this exact actor view with modules GHC named while describing a
    /// typed suspension. These are compiler-derived type dependencies, not an
    /// authored escape hatch around [`ActorSourceImports`]. Returning another
    /// immutable view keeps turn compilation and persisted declaration source
    /// on the same import set.
    pub(crate) fn with_type_modules(mut self, modules: &[String]) -> Self {
        for module in modules {
            self.external.extend_text(module);
        }
        self
    }

    /// Exact external, declaration, and live-value imports for a turn template.
    #[must_use]
    pub fn turn_imports(&self) -> String {
        self.session.turn_imports(&self.external)
    }

    #[must_use]
    pub(crate) fn shadow_preamble(&self, preamble: &str) -> String {
        self.session.shadow_preamble(preamble)
    }

    /// Exact non-session imports needed when persisting a declaration. This
    /// includes configured vocabulary and compiler-derived type dependencies,
    /// but excludes generated declaration/value modules used to carry state.
    #[must_use]
    pub fn workbench_imports(&self) -> SourceImports {
        self.session.workbench_imports(&self.external)
    }

    /// This actor's own source layer, then the deployment-wide roots, then the
    /// session's module tree.
    ///
    /// The layer leads because GHC takes the first root that provides a module:
    /// an actor editing a module inside its own checkout shadows the run's copy
    /// for its own cells and for nobody else's. An actor with no checkout of
    /// its own carries an empty layer and gets exactly the deployment-wide
    /// list, which is what the root itself carries.
    #[must_use]
    pub fn include_paths(&self, base: &[PathBuf]) -> Vec<PathBuf> {
        if self.source_layer.is_empty() {
            return self.session.include_paths(base);
        }
        let mut include = self.source_layer.to_vec();
        include.extend(self.session.include_paths(base));
        include
    }

    #[must_use]
    pub fn session_root(&self) -> &Path {
        self.session.session_root()
    }

    #[must_use]
    pub fn injected_module_names(&self) -> Vec<String> {
        self.session.injected_module_names()
    }

    #[must_use]
    pub fn next_value_generation(&self) -> Generation {
        self.session.next_value_generation()
    }

    /// Whether `other` still names the same source-side environment this
    /// view compiled against, ignoring `next_value_generation` (a caller
    /// that reserved its own generation before releasing its checkout
    /// expects that counter alone to have moved). A split compile takes this
    /// view, releases its checkout, compiles off-checkout, then re-derives a
    /// fresh view on re-checkout; a `false` here means something else wrote
    /// to a scope this compile actually read from, and the compiled result
    /// must not be installed.
    #[must_use]
    pub fn compile_relevant_eq(&self, other: &Self) -> bool {
        self.session.compile_relevant_eq(&other.session)
    }

    /// Opaque identity for compiler evidence produced against this exact
    /// immutable source/import/session snapshot.
    #[must_use]
    pub(crate) fn evidence_key(&self) -> String {
        fn field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        let mut hasher = blake3::Hasher::new();
        field(&mut hasher, &self.session.session().0.to_le_bytes());
        field(&mut hasher, &self.session.lexical_scope().0.to_le_bytes());
        field(
            &mut hasher,
            self.session.session_root().as_os_str().as_encoded_bytes(),
        );
        field(
            &mut hasher,
            self.session.persistent_imports().template_text().as_bytes(),
        );
        field(&mut hasher, self.external.template_text().as_bytes());
        field(
            &mut hasher,
            &self.session.next_value_generation().0.to_le_bytes(),
        );
        for module in self
            .session
            .library()
            .into_iter()
            .chain(self.session.visible_values().iter().copied())
            .chain(self.session.injected_values().iter().copied())
        {
            field(&mut hasher, module.module_name().as_bytes());
        }
        for path in self.source_layer.iter() {
            field(&mut hasher, path.as_os_str().as_encoded_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    /// The declaration module this view currently imports unqualified for
    /// this scope (`None` before the first declaration ever lands). Needed to
    /// name the exact "current" generation a same-cell redeclaration retry
    /// must hide from — see `resident_workbench::prepare_cell`.
    #[must_use]
    pub fn library(&self) -> Option<SessionModule> {
        self.session.library()
    }

    #[must_use]
    pub(crate) fn with_staged_library(
        mut self,
        module: tidepool_repr::SessionModule,
        declared: &[tidepool_runtime::session::ExportItem],
    ) -> Self {
        self.session = self.session.with_staged_library(module, declared);
        self
    }

    #[must_use]
    pub(crate) fn with_staged_values(
        mut self,
        module: tidepool_repr::SessionModule,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        self.session = self.session.with_staged_values(module, names);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorCompileViewError {
    #[error("actor source view belongs to session {actual:?}, expected {expected:?}")]
    WrongSession {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("actor source view belongs to lexical scope {actual:?}, expected {expected:?}")]
    WrongLexicalScope { expected: ScopeId, actual: ScopeId },
}

/// Actor-owned portion of a resident machine mount. Machine checkout remains
/// in `tidepool-runtime`; this value prevents scope and authority selection
/// from drifting apart at the actor boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorSessionContext {
    pub actor: ActorRef,
    pub placement: ActorPlacement,
    pub effect_policy: EffectRunPolicy,
    pub live_payload: LivePayloadPolicy,
    pub source_imports: ActorSourceImports,
    pub haskell_effects_alias: String,
    /// Include roots this actor alone compiles against, ahead of every shared
    /// root. Empty for an actor with no checkout of its own — the root, the
    /// operator workbench, and every actor launched without a worktree. Fixed
    /// when the actor is constructed; there is no setter, because an actor's
    /// search path must not move under a cell that is already compiling.
    pub source_layer: std::sync::Arc<[PathBuf]>,
}

impl ActorSessionContext {
    #[must_use]
    pub fn run_context(&self) -> SessionRunContext {
        SessionRunContext::new(
            self.placement.resource_scope,
            self.placement.lexical_scope,
            PrincipalId::from(self.actor),
        )
    }

    /// Bind an immutable session snapshot to this actor's exact import
    /// membrane. A parent, sibling, or foreign-session view is rejected before
    /// any source is rendered.
    pub fn compile_view(
        &self,
        session: SessionCompileView,
    ) -> Result<ActorCompileView, ActorCompileViewError> {
        if session.session() != self.placement.session {
            return Err(ActorCompileViewError::WrongSession {
                expected: self.placement.session,
                actual: session.session(),
            });
        }
        if session.lexical_scope() != self.placement.lexical_scope {
            return Err(ActorCompileViewError::WrongLexicalScope {
                expected: self.placement.lexical_scope,
                actual: session.lexical_scope(),
            });
        }
        Ok(ActorCompileView {
            session,
            external: self.source_imports.imports.clone(),
            source_layer: self.source_layer.clone(),
        })
    }
}

/// What retiring one placement released: the parked-frame half from closing
/// its runtime resource scope plus the binding-store half from retiring its lexical
/// scope. Mirrors the union of `ResidentSession::close_realm`'s
/// `(frames, handles)` pair and `ResidentSession::retire_scope`'s
/// `ScopeRetirement`; an engine with no lexical-scope frames of its own (the
/// prepared-STG engine) reports `0` for `scope_roots` rather than inventing a
/// value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlacementRetirement {
    /// Parked continuation frames dropped when the runtime resource scope closes.
    pub frames: usize,
    /// Value handles/roots released when the runtime resource scope closes.
    pub handles: usize,
    /// Import leases released when the runtime resource scope closes.
    pub leases: usize,
    /// Persistent GC roots released by retiring the lexical scope.
    pub scope_roots: usize,
}

/// Narrow target seam used to mount the real resident session without moving
/// machine ownership into actor-local code.
pub trait ActorRunTarget {
    type Error;

    /// The registry hole type this machine parks a suspended continuation
    /// under (the `SessionRegistry`'s checkout-index value type).
    type Hole: Clone + PartialEq + std::fmt::Debug;

    fn install_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) -> Result<(), Self::Error>;

    /// Retire one actor placement: release everything its runtime resource scope and
    /// lexical scope solely owned. Idempotent on an already-retired
    /// placement, mirroring `close_realm`/`retire_scope`'s own all-zero
    /// no-op receipts.
    fn retire_placement(
        &mut self,
        resource_scope: RealmId,
        lexical_scope: ScopeId,
    ) -> PlacementRetirement;
}

impl<H, O> ActorRunTarget for ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    type Error = ResidentError;
    type Hole = String;

    fn install_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) -> Result<(), Self::Error> {
        self.set_actor_execution(context, effect_policy, live_payload)
    }

    fn retire_placement(
        &mut self,
        resource_scope: RealmId,
        lexical_scope: ScopeId,
    ) -> PlacementRetirement {
        let (frames, handles) = self.close_realm(resource_scope);
        let scope = self.retire_scope(lexical_scope);
        PlacementRetirement {
            frames,
            handles,
            leases: 0,
            scope_roots: scope.roots_released,
        }
    }
}
