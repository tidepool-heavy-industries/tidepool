use std::path::{Path, PathBuf};

use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{Generation, PrincipalId, SessionId, SessionModule};
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentError, ResidentSession, RuntimeCompileInputs,
    SessionCompileView, SessionRunContext, SourceImports,
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
/// Selected context accepts only materialized exact-export facades. Inherited
/// context retains its original runtime-issued lexical lease instead of adding
/// a new import of the invoking private declaration module.
#[derive(Debug, Clone, Default)]
pub struct ActorSourceImports {
    imports: SourceImports,
    inherited_scope: Option<std::sync::Arc<InheritedScopeCapture>>,
    selected_facades: Option<std::sync::Arc<SelectedFacadeCapture>>,
}

#[derive(Debug)]
struct SelectedFacadeCapture {
    facades: parking_lot::Mutex<Option<Vec<MaterializedFacade>>>,
}

#[derive(Debug)]
struct InheritedScopeCapture {
    lease: parking_lot::Mutex<
        Option<std::sync::Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    >,
}

impl PartialEq for ActorSourceImports {
    fn eq(&self, other: &Self) -> bool {
        self.imports == other.imports
            && match (&self.inherited_scope, &other.inherited_scope) {
                (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
            && match (&self.selected_facades, &other.selected_facades) {
                (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }
}
impl Eq for ActorSourceImports {}

/// A source graph retained by the configured host source owner.
///
/// Implementations retain the actual run or capture guard. These observations
/// describe the immutable graph; they are not compiler byte or negative-input
/// witnesses. Only the configured `ActorSourceLayers` implementation issues
/// this authority at its source admission boundary through its private issuer.
pub trait RetainedSourceLayer: Send + Sync {
    fn identities(&self) -> &[String];
    fn include_paths(&self) -> &[PathBuf];

    /// Compiler originals acquired with this source graph. An owner with no
    /// catalog returns `Acquired(None)` rather than consulting ambient state.
    fn catalog_selection(&self) -> tidepool_toolchain::toolchain::CatalogSelection;

    /// Complete ordered manifests acquired by the immutable source owner.
    /// Unprepared/developer owners may omit them and retain fresh inspection.
    fn source_manifests(&self) -> Option<&[tidepool_toolchain::cache::SourceRootManifest]> {
        None
    }

    fn prepared_entries(&self) -> Option<&SourceEntryStorage> {
        None
    }
}

/// Retained storage selected by the source owner. A recipe key is only a
/// location: complete original compiler custody is required when loading it.
#[derive(Clone, Debug)]
pub enum SourceEntryStorage {
    FreshCompilation {
        directory: PathBuf,
        preparation: uuid::Uuid,
    },
    CompletedOriginal {
        directory: PathBuf,
        selections: std::collections::BTreeMap<String, uuid::Uuid>,
    },
}

impl SourceEntryStorage {
    #[must_use]
    pub fn directory(&self) -> &Path {
        match self {
            Self::FreshCompilation { directory, .. }
            | Self::CompletedOriginal { directory, .. } => directory,
        }
    }
}

/// Issuing authority belonging to one configured host source service.
/// Cloning shares that issuer; constructing another issuer never authorizes
/// the first service's capsules, even when observations and digests match.
#[derive(Clone, Debug)]
pub struct SourceLayerIssuer(std::sync::Arc<uuid::Uuid>);

impl Default for SourceLayerIssuer {
    fn default() -> Self {
        Self(std::sync::Arc::new(uuid::Uuid::new_v4()))
    }
}

impl SourceLayerIssuer {
    /// Issue from the actual retained source owner, never from its descriptor.
    #[must_use]
    pub fn issue(&self, owner: std::sync::Arc<dyn RetainedSourceLayer>) -> CheckpointSourceLayer {
        CheckpointSourceLayer {
            retained: Some(std::sync::Arc::new(RetainedCheckpointSource {
                issuer: self.0.clone(),
                identities: owner.identities().to_vec(),
                include_paths: owner.include_paths().to_vec(),
                _owner: owner,
            })),
        }
    }

    #[must_use]
    pub fn owns(&self, source: &CheckpointSourceLayer) -> bool {
        source
            .retained
            .as_ref()
            .is_some_and(|source| std::sync::Arc::ptr_eq(&self.0, &source.issuer))
    }
}

/// Host-issued immutable source authority for a captured context. Clones share
/// the original owner rather than minting authority from observed paths.
/// An empty default carries no source ownership.
///
/// ```compile_fail
/// let mut source = exomonad_actor::CheckpointSourceLayer::default();
/// source.include_paths = vec![std::path::PathBuf::from("unowned")];
/// ```
#[derive(Clone, Default)]
pub struct CheckpointSourceLayer {
    retained: Option<std::sync::Arc<RetainedCheckpointSource>>,
}

struct RetainedCheckpointSource {
    _owner: std::sync::Arc<dyn RetainedSourceLayer>,
    issuer: std::sync::Arc<uuid::Uuid>,
    identities: Vec<String>,
    include_paths: Vec<PathBuf>,
}

impl CheckpointSourceLayer {
    #[must_use]
    pub fn identities(&self) -> &[String] {
        self.retained
            .as_ref()
            .map_or(&[], |source| &source.identities)
    }

    #[must_use]
    pub fn include_paths(&self) -> &[PathBuf] {
        self.retained
            .as_ref()
            .map_or(&[], |source| &source.include_paths)
    }

    #[must_use]
    pub fn catalog_selection(&self) -> tidepool_toolchain::toolchain::CatalogSelection {
        self.retained.as_ref().map_or(
            tidepool_toolchain::toolchain::CatalogSelection::Acquired(None),
            |source| source._owner.catalog_selection(),
        )
    }

    #[must_use]
    pub fn source_manifests(&self) -> Option<&[tidepool_toolchain::cache::SourceRootManifest]> {
        self.retained.as_ref()?._owner.source_manifests()
    }

    #[must_use]
    pub fn prepared_entries(&self) -> Option<&SourceEntryStorage> {
        self.retained.as_ref()?._owner.prepared_entries()
    }

    #[must_use]
    pub fn is_owned(&self) -> bool {
        self.retained.is_some()
    }

    /// Same configured source owner and ordered revisions. Helper branches
    /// may materialize the same revision at different immutable paths.
    #[must_use]
    pub fn same_revision(&self, other: &Self) -> bool {
        match (&self.retained, &other.retained) {
            (Some(left), Some(right)) => {
                std::sync::Arc::ptr_eq(&left.issuer, &right.issuer)
                    && left.identities == right.identities
            }
            (None, None) => true,
            _ => false,
        }
    }

    /// Bind the precise owner and source selection independently of the
    /// compiler's canonical cell recipe digest.
    #[must_use]
    pub fn semantic_digest(&self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        let mut frame = |bytes: &[u8]| {
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        frame(b"exomonad-retained-checkpoint-source-v1");
        if let Some(source) = &self.retained {
            frame(b"owned");
            frame(source.issuer.as_bytes());
            frame(&(source.identities.len() as u64).to_le_bytes());
            for identity in &source.identities {
                frame(identity.as_bytes());
            }
            frame(&(source.include_paths.len() as u64).to_le_bytes());
            for path in &source.include_paths {
                frame(path.as_os_str().as_encoded_bytes());
            }
            if let Some(entries) = source._owner.prepared_entries() {
                frame(entries.directory().as_os_str().as_encoded_bytes());
                match entries {
                    SourceEntryStorage::FreshCompilation { preparation, .. } => {
                        frame(b"fresh-source");
                        frame(preparation.as_bytes());
                    }
                    SourceEntryStorage::CompletedOriginal { selections, .. } => {
                        frame(b"selected-original-output");
                        frame(&(selections.len() as u64).to_le_bytes());
                        for (recipe, original) in selections {
                            frame(recipe.as_bytes());
                            frame(original.as_bytes());
                        }
                    }
                }
            }
        } else {
            frame(b"unowned-empty");
        }
        *digest.finalize().as_bytes()
    }
}

impl std::fmt::Debug for CheckpointSourceLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointSourceLayer")
            .field("owned", &self.is_owned())
            .field("identities", &self.identities())
            .field("include_paths", &self.include_paths())
            .finish()
    }
}

impl PartialEq for CheckpointSourceLayer {
    fn eq(&self, other: &Self) -> bool {
        self.same_revision(other) && self.include_paths() == other.include_paths()
    }
}
impl Eq for CheckpointSourceLayer {}

impl ActorSourceImports {
    #[must_use]
    pub fn from_exact_facades<'a>(
        facades: impl IntoIterator<Item = &'a MaterializedFacade>,
    ) -> Self {
        let facades: Vec<_> = facades.into_iter().collect();
        Self {
            imports: SourceImports::from_specs(facades.iter().map(|facade| facade.module_name())),
            inherited_scope: None,
            selected_facades: (!facades.is_empty()).then(|| {
                std::sync::Arc::new(SelectedFacadeCapture {
                    facades: parking_lot::Mutex::new(Some(facades.into_iter().cloned().collect())),
                })
            }),
        }
    }

    pub(crate) fn from_inherited_scope(
        scope: std::sync::Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    ) -> Self {
        Self {
            imports: SourceImports::default(),
            inherited_scope: Some(std::sync::Arc::new(InheritedScopeCapture {
                lease: parking_lot::Mutex::new(Some(scope)),
            })),
            selected_facades: None,
        }
    }

    pub(crate) fn inherited_scope(
        &self,
    ) -> Result<
        Option<std::sync::Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
        ActorCompileViewError,
    > {
        self.inherited_scope
            .as_ref()
            .map(|capture| {
                capture
                    .lease
                    .lock()
                    .clone()
                    .ok_or(ActorCompileViewError::ReleasedInheritedScope)
            })
            .transpose()
    }

    /// Descriptor and directory observations share this slot, so retained
    /// terminal metadata cannot keep the actor's capture alive.
    pub(crate) fn release_capture(&self) {
        if let Some(capture) = &self.inherited_scope {
            capture.lease.lock().take();
        }
        if let Some(capture) = &self.selected_facades {
            capture.facades.lock().take();
        }
    }

    pub(crate) fn declaration_projections(
        &self,
    ) -> Result<
        Vec<std::sync::Arc<tidepool_runtime::session::CertifiedDeclarationProjection>>,
        ActorCompileViewError,
    > {
        let Some(capture) = &self.selected_facades else {
            return Ok(Vec::new());
        };
        let facades = capture.facades.lock();
        let facades = facades
            .as_ref()
            .ok_or(ActorCompileViewError::ReleasedDeclarationCapture)?;
        Ok(facades
            .iter()
            .filter_map(|facade| facade.projection().cloned())
            .collect())
    }
}

/// How a host installs branch-local helper source for an actor at launch.
///
/// One implementation per host, supplied by the composition root that owns the
/// run's source. The actor engine asks once, while the actor is being
/// constructed, for its private helper include roots. The hosted run tooling
/// remains in the shared graph; a checkout's historical package is not
/// included automatically.
pub trait ActorSourceLayers: Send + Sync {
    /// Freeze the published toolset source graph independently of notebook helpers.
    /// The source owner must return direct immutable revision paths.
    fn freeze_toolset_layer(&self, _actor: PrincipalId) -> Result<CheckpointSourceLayer, String> {
        Ok(CheckpointSourceLayer::default())
    }

    /// Select the run toolset from an already admitted immutable source graph.
    fn toolset_layer_from(
        &self,
        source: &CheckpointSourceLayer,
    ) -> Result<CheckpointSourceLayer, String> {
        self.validate_source_authority(source)?;
        if source.is_owned() {
            Err("source owner does not expose its published toolset graph".into())
        } else {
            Ok(CheckpointSourceLayer::default())
        }
    }

    /// Admit an uncovered installer specialization for an already admitted
    /// immutable source graph. Optional prepared coverage is not a role grant.
    /// A selected original that fails validation must never reach this method.
    fn fresh_toolset_layer_from(
        &self,
        source: &CheckpointSourceLayer,
    ) -> Result<CheckpointSourceLayer, String> {
        self.validate_source_authority(source)?;
        Err("source owner did not admit fresh installer compilation".into())
    }

    /// Validate opaque source ownership at cell/checkpoint admission. A host
    /// that issues no source authority can admit only the empty default.
    fn validate_source_authority(&self, source: &CheckpointSourceLayer) -> Result<(), String> {
        if source.is_owned() {
            Err("host did not issue this source authority".into())
        } else {
            Ok(())
        }
    }

    fn freeze_checkpoint_layer(
        &self,
        _issuer: PrincipalId,
    ) -> Result<CheckpointSourceLayer, String> {
        Ok(CheckpointSourceLayer::default())
    }

    fn admit_checkpoint_layer(
        &self,
        checkpoint: &CheckpointSourceLayer,
        _creator: PrincipalId,
        helper_branch: &str,
    ) -> Result<Vec<PathBuf>, String> {
        self.validate_source_authority(checkpoint)?;
        let selected = self.layer_include_for(helper_branch)?;
        if checkpoint.identities().is_empty() && selected.is_empty() {
            Ok(selected)
        } else {
            Err("host cannot admit checkpoint source without owned immutable revisions".into())
        }
    }

    /// Reuse an already admitted source capsule after the creator advances.
    /// Authentication belongs to the issuing host; current draft equality is
    /// not admission authority for this retained immutable graph.
    fn admit_retained_layer(&self, source: &CheckpointSourceLayer) -> Result<Vec<PathBuf>, String> {
        self.validate_source_authority(source)?;
        Ok(source.include_paths().to_vec())
    }

    fn bind_checkpoint_for(
        &self,
        actor: PrincipalId,
        _helper_branch: &str,
        checkpoint: &CheckpointSourceLayer,
    ) -> Result<(), String> {
        let _ = actor;
        self.validate_source_authority(checkpoint)
    }
    /// Retain the creator's already selected helper source without capturing
    /// bytes, copying drafts, or publishing a new generation for a spawn.
    fn retain_helpers(&self, _creator: PrincipalId) -> Result<String, String> {
        Ok(String::new())
    }

    /// Mutable helper publication selected explicitly by source ownership.
    fn layer_include_for(&self, _helper_source: &str) -> Result<Vec<PathBuf>, String> {
        Ok(Vec::new())
    }

    fn bind_for(&self, _actor: PrincipalId, _helper_source: &str) {}

    /// Re-read and publish the source layer this principal may own, with
    /// `also_check` naming modules to pull into the checked closure. Hosts
    /// that install no layers answer
    /// [`SourceLayerReload::Unavailable`], which is also the default.
    fn reload(&self, actor: PrincipalId, also_check: &[String]) -> SourceLayerReload {
        let _ = (actor, also_check);
        SourceLayerReload::Unavailable("this host installs no source layers".into())
    }

    /// Re-read this actor's session helper draft and publish its last
    /// typechecked revision. Helpers are branch-local source, not AgentSpec
    /// tool declarations. Hosts without the helper surface report it as
    /// unavailable.
    fn reload_helpers(&self, actor: PrincipalId, also_check: &[String]) -> SourceLayerReload {
        let _ = (actor, also_check);
        SourceLayerReload::Unavailable("this host installs no session helpers".into())
    }

    /// Retain a checked AgentSpec source candidate without publishing it.
    /// The actor executes its installer and compares the registered surface
    /// before consuming the returned source-owner token.
    fn stage_spec_reload(
        self: std::sync::Arc<Self>,
        actor: PrincipalId,
        also_check: &[String],
    ) -> Result<Box<dyn StagedActorSourceReload>, SourceLayerReload>;

    fn reload_helpers_with_publication(
        &self,
        _actor: PrincipalId,
        _also_check: &[String],
        _publication: &std::sync::Arc<tidepool_runtime::session::PublicationDecision>,
    ) -> SourceLayerReload {
        SourceLayerReload::Unavailable(
            "source owner does not support cancellable helper reload".into(),
        )
    }
}

/// A source-owner candidate retaining its original expected active revision.
/// Its capsule contains direct immutable paths and the same retained source
/// ownership used by ordinary installer admission.
pub trait StagedActorSourceReload: Send + Sync {
    fn source(&self) -> &CheckpointSourceLayer;

    /// Called synchronously while the actor's installed-pointer fence is held.
    /// The source owner takes its publication gate, rejects a changed active
    /// revision, then arbitrates cancellation with the existing decision.
    /// After the visibility rename it invokes `on_visible` exactly once before
    /// confirming durability. The callback replaces the paired source/tools
    /// lease infallibly and must not await or reenter either owner.
    /// A before-visibility refusal drops the callback without invoking it.
    fn commit(
        self: Box<Self>,
        publication: &std::sync::Arc<tidepool_runtime::session::PublicationDecision>,
        on_visible: Box<dyn FnOnce() + '_>,
    ) -> SourceLayerReload;
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
    /// Cancellation won before the checked candidate became visible.
    Cancelled,
    /// The new revision is visible but durable confirmation failed. Retain the
    /// native commit claim as unconfirmed rather than retrying publication.
    PublicationUnconfirmed {
        revision: String,
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
    compile_inputs: RuntimeCompileInputs,
}

impl ActorCompileView {
    pub(crate) fn compile_inputs(&self) -> &RuntimeCompileInputs {
        &self.compile_inputs
    }

    /// Add trusted imports supplied by the hosted workbench itself. These are
    /// source vocabulary, not ambient lexical ancestry.
    pub(crate) fn with_workbench_imports(mut self, imports: &SourceImports) -> Self {
        self.external.extend(imports);
        self
    }

    pub fn exact_compile_context(
        &self,
    ) -> Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactCompileContext>> {
        self.session.exact_compile_context()
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

    /// This actor's helper layer, then the deployment-wide roots, then the
    /// session's module tree.
    ///
    /// The helper layer leads because GHC takes the first root that provides a
    /// module. The host limits helpers to the reserved namespace, so checkout
    /// tooling cannot shadow the run's graph.
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

    pub(crate) fn session_view(&self) -> &SessionCompileView {
        &self.session
    }

    pub(crate) fn exact_declaration_context(
        &self,
    ) -> Option<&std::sync::Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>
    {
        self.session.exact_declaration_context()
    }

    /// This view's session incarnation identity, forwarded to a spawned
    /// compile so its worker-side memo can retain `Tidepool.Session.*`
    /// entries across transactions within this incarnation.
    #[must_use]
    pub fn session_id(&self) -> tidepool_repr::SessionId {
        self.session.session()
    }

    #[must_use]
    pub fn injected_module_names(&self) -> Vec<String> {
        self.session.injected_module_names()
    }

    /// Names of the injected value modules this actor's turn can reach: the
    /// part of the injected set that compiler evidence depends on.
    #[must_use]
    pub(crate) fn reachable_module_names(&self) -> Vec<String> {
        self.session
            .reachable_values()
            .iter()
            .map(tidepool_repr::SessionModule::module_name)
            .collect()
    }

    #[must_use]
    pub fn next_value_generation(&self) -> Generation {
        self.session.next_value_generation()
    }

    /// Whether a split compile made against `compiled_against` may still be
    /// installed now that the session presents this view. A split compile
    /// takes a view, releases its checkout, compiles off-checkout, then
    /// re-derives a fresh view on re-checkout; a `false` here means a scope
    /// this compile reads changed what the cell imports, or a value module the
    /// cell could reach is no longer live, and the compiled result must not
    /// be installed. Other actors' binds and releases do not count, and
    /// neither does `next_value_generation` alone moving on
    /// ([`tidepool_runtime::session::SessionCompileView::is_current_for`]).
    #[must_use]
    pub fn is_current_for(&self, compiled_against: &Self) -> bool {
        self.session.is_current_for(&compiled_against.session)
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
        match self.exact_declaration_context() {
            Some(context) => {
                field(&mut hasher, &[1]);
                field(&mut hasher, &context.semantic_sha256());
            }
            None => field(&mut hasher, &[0]),
        }
        if let Some(context) = self.exact_compile_context() {
            if let Some(annotations) = context.request_annotations() {
                field(&mut hasher, &annotations.signatures().metadata_digest());
                field(&mut hasher, annotations.helper_recipe().as_str().as_bytes());
                field(&mut hasher, &context.declarations().semantic_sha256());
            }
        }
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
            .chain(self.session.reachable_values().iter().copied())
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

#[derive(Debug, thiserror::Error)]
pub enum ActorCompileViewError {
    #[error("actor inherited declaration capture has been released")]
    ReleasedInheritedScope,
    #[error("actor selected declaration capture has been released")]
    ReleasedDeclarationCapture,
    #[error(transparent)]
    CompileInputs(#[from] tidepool_runtime::CompileError),
    #[error("nonempty actor helper source requires an owned source capsule")]
    UnownedSourceLayer,
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
    pub haskell_effects_alias: tidepool_runtime::session::HaskellTypeSource,
    /// Helper include roots this actor alone compiles against, ahead of every
    /// shared root. Fixed
    /// when the actor is constructed; there is no setter, because an actor's
    /// search path must not move under a cell that is already compiling.
    pub source_layer: std::sync::Arc<[PathBuf]>,
}

impl ActorSessionContext {
    /// Select the immutable revision paths captured for an issued workbench
    /// request. Actor authority and lexical placement remain unchanged.
    pub(crate) fn with_issued_source(
        mut self,
        source: &CheckpointSourceLayer,
    ) -> Result<Self, ActorCompileViewError> {
        if !source.is_owned() && !self.source_layer.is_empty() {
            return Err(ActorCompileViewError::UnownedSourceLayer);
        }
        if source.is_owned() {
            self.source_layer = source.include_paths().to_vec().into();
        }
        Ok(self)
    }

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
        let inputs =
            RuntimeCompileInputs::new(None, self.source_imports.declaration_projections()?)?;
        self.compile_view_with_inputs(session, inputs)
    }

    pub(crate) fn compile_view_with_inputs(
        &self,
        session: SessionCompileView,
        inputs: RuntimeCompileInputs,
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
        let session = session.with_compile_inputs(&inputs)?;
        Ok(ActorCompileView {
            session,
            external: self
                .haskell_effects_alias
                .source_imports(&self.source_imports.imports),
            source_layer: self.source_layer.clone(),
            compile_inputs: inputs,
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

#[cfg(test)]
mod source_authority_tests {
    use super::*;
    use std::sync::Arc;

    struct RetainedTree {
        _tree: Arc<tempfile::TempDir>,
        identities: Vec<String>,
        paths: Vec<PathBuf>,
    }

    impl RetainedSourceLayer for RetainedTree {
        fn catalog_selection(&self) -> tidepool_toolchain::toolchain::CatalogSelection {
            tidepool_toolchain::toolchain::CatalogSelection::Acquired(None)
        }

        fn identities(&self) -> &[String] {
            &self.identities
        }
        fn include_paths(&self) -> &[PathBuf] {
            &self.paths
        }
    }

    fn context(paths: Vec<PathBuf>) -> ActorSessionContext {
        ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            placement: ActorPlacement {
                session: SessionId(1),
                resource_scope: RealmId::ROOT,
                lexical_scope: ScopeId::ROOT,
            },
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: ActorSourceImports::default(),
            haskell_effects_alias: "[]".into(),
            source_layer: paths.into(),
        }
    }

    #[test]
    fn unowned_default_cannot_select_nonempty_source() {
        let source = CheckpointSourceLayer::default();
        let result = context(vec![PathBuf::from("ambient")]).with_issued_source(&source);
        assert!(matches!(
            result,
            Err(ActorCompileViewError::UnownedSourceLayer)
        ));
        assert!(context(vec![]).with_issued_source(&source).is_ok());
    }

    #[test]
    fn issued_source_preserves_actual_capture_and_immutable_observations() {
        let tree = Arc::new(tempfile::tempdir().unwrap());
        let path = tree.path().to_path_buf();
        let owner: Arc<dyn RetainedSourceLayer> = Arc::new(RetainedTree {
            _tree: Arc::clone(&tree),
            identities: vec!["revision:1".into()],
            paths: vec![path.clone()],
        });
        let issuer = SourceLayerIssuer::default();
        let source = issuer.issue(Arc::clone(&owner));
        let retained = source.clone();
        let mut observation = source.include_paths().to_vec();
        observation.clear();
        let selected = context(vec![PathBuf::from("ambient")])
            .with_issued_source(&source)
            .unwrap();
        assert_eq!(&*selected.source_layer, &[path.clone()]);
        assert_eq!(source.include_paths(), &[path.clone()]);
        drop(selected);
        drop(source);
        drop(owner);
        drop(tree);
        assert!(path.exists());
        drop(retained);
        assert!(!path.exists());
    }

    #[test]
    fn matching_descriptor_does_not_authorize_foreign_source_issuer() {
        let tree = Arc::new(tempfile::tempdir().unwrap());
        let owner: Arc<dyn RetainedSourceLayer> = Arc::new(RetainedTree {
            paths: vec![tree.path().to_path_buf()],
            _tree: tree,
            identities: vec!["revision:1".into()],
        });
        let issuer = SourceLayerIssuer::default();
        let foreign = SourceLayerIssuer::default();
        let source = issuer.issue(Arc::clone(&owner));
        let clone = source.clone();
        let other = foreign.issue(owner);
        assert!(issuer.owns(&clone));
        assert_eq!(source.semantic_digest(), clone.semantic_digest());
        assert!(!issuer.owns(&other));
        assert!(!source.same_revision(&other));
        assert_eq!(source.identities(), other.identities());
        assert_eq!(source.include_paths(), other.include_paths());
        assert_ne!(source.semantic_digest(), other.semantic_digest());
    }
}
