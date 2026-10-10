//! Capture of a public Haskell actor launch or replacement entry.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::collections::BTreeSet;
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, FromHaskell};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_repr::{DataConTable, Generation};
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentHole, ResidentSession, RootCustody,
};

use crate::generated::actor::ActorReq;
use crate::ActorDescriptor;
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    tidepool_bridge_derive::FromHaskell,
    tidepool_bridge_derive::ToHaskell,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ForkEffort {
    #[haskell(module = "Tidepool.Effects.Core")]
    Low,
    #[haskell(module = "Tidepool.Effects.Core")]
    Medium,
    #[haskell(module = "Tidepool.Effects.Core")]
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]
pub enum Model {
    Alias(String),
    Literal(String),
}

impl Model {
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Alias(value) | Self::Literal(value) => value,
        }
    }
}

impl std::ops::Deref for Model {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.value()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]
pub enum WorkerLifetime {
    InvocationOwned,
    ActorOwned,
    RunOwned,
    InScope(tidepool_bridge_effects::ResourceScopeId),
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]
pub enum SpawnContextWire {
    CapturedSpawn(String),
    FreshSpawn(String),
}

#[derive(Debug, Clone, tidepool_bridge_derive::ToHaskell)]
pub enum SpawnRetainedResources {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRetainedWorkspace(tidepool_bridge_effects::WtWorktreeHandle),
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRetainedActor(
        (i64, i64),
        Option<tidepool_bridge_effects::WtWorktreeHandle>,
    ),
}

#[derive(Debug, Clone, tidepool_bridge_derive::ToHaskell)]
pub enum SpawnError {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRefused(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnPartialFailure(
        SpawnRetainedResources,
        crate::lineage::SpawnCleanupOutcome,
        String,
    ),
}

pub(crate) struct SpawnDefinition {
    pub context: SpawnContextWire,
    pub workspace: crate::fork_workspace::SpawnWorkspaceWire,
}

pub(crate) struct ActorStartRequest {
    pub label: String,
    pub profile: ActorEffectProfileWire,
    pub workspace: Option<tidepool_bridge_effects::WtWorkspaceHandle>,
    pub session_id: tidepool_repr::SessionId,
    pub parent_actor: crate::ActorRef,
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub enum ActorEffectProfileWire {
    ActorReadWriteProfile,
    ActorReadOnlyProfile,
    ActorSelectedProfile(Vec<ActorEffectKeyWire>),
}

impl ActorEffectProfileWire {
    fn resolve(self) -> (crate::ActorEffectProfile, Option<Vec<ActorEffectKeyWire>>) {
        match self {
            Self::ActorSelectedProfile(keys) => (crate::ActorEffectProfile::ReadOnly, Some(keys)),
            Self::ActorReadOnlyProfile => (crate::ActorEffectProfile::ReadOnly, None),
            Self::ActorReadWriteProfile => (crate::ActorEffectProfile::ReadWrite, None),
        }
    }
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub enum ActorEffectKeyWire {
    EffectResourceScopes,
    EffectReplies,
    EffectWatches,
    EffectActorContext,
    EffectAgentLaunch,
    EffectAgentInspection,
    EffectAgentControl,
    EffectBoundWorktree,
    EffectWorktreeRegistry,
    EffectWorktreeAllocation,
    EffectWorktreeIntegration,
    EffectSleep,
    EffectNotifications,
    EffectJev,
    EffectModelCall,
    EffectCommands,
    EffectConsole,
    EffectAskUser,
    EffectGreen,
    EffectActor,
    EffectReflect,
    EffectLookup,
    EffectRepoEvent,
    EffectSource,
    EffectJournal,
}

impl From<ActorEffectKeyWire> for crate::ActorEffectKey {
    fn from(value: ActorEffectKeyWire) -> Self {
        match value {
            ActorEffectKeyWire::EffectResourceScopes => Self::ResourceScopes,
            ActorEffectKeyWire::EffectReplies => Self::Replies,
            ActorEffectKeyWire::EffectWatches => Self::Watches,
            ActorEffectKeyWire::EffectActorContext => Self::ActorContext,
            ActorEffectKeyWire::EffectAgentLaunch => Self::AgentLaunch,
            ActorEffectKeyWire::EffectAgentInspection => Self::AgentInspection,
            ActorEffectKeyWire::EffectAgentControl => Self::AgentControl,
            ActorEffectKeyWire::EffectBoundWorktree => Self::BoundWorktree,
            ActorEffectKeyWire::EffectWorktreeRegistry => Self::WorktreeRegistry,
            ActorEffectKeyWire::EffectWorktreeAllocation => Self::WorktreeAllocation,
            ActorEffectKeyWire::EffectWorktreeIntegration => Self::WorktreeIntegration,
            ActorEffectKeyWire::EffectSleep => Self::Sleep,
            ActorEffectKeyWire::EffectNotifications => Self::Notifications,
            ActorEffectKeyWire::EffectJev => Self::Jev,
            ActorEffectKeyWire::EffectModelCall => Self::ModelCall,
            ActorEffectKeyWire::EffectCommands => Self::Commands,
            ActorEffectKeyWire::EffectConsole => Self::Console,
            ActorEffectKeyWire::EffectAskUser => Self::AskUser,
            ActorEffectKeyWire::EffectGreen => Self::Green,
            ActorEffectKeyWire::EffectActor => Self::Actor,
            ActorEffectKeyWire::EffectReflect => Self::Reflect,
            ActorEffectKeyWire::EffectLookup => Self::Lookup,
            ActorEffectKeyWire::EffectRepoEvent => Self::RepoEvent,
            ActorEffectKeyWire::EffectSource => Self::Source,
            ActorEffectKeyWire::EffectJournal => Self::Journal,
        }
    }
}

/// One parked parent continuation paired with exclusive ownership of its child
/// entry. Compiler provenance travels with the rooted entry itself.
pub struct ResidentActorStart {
    pub(crate) parent_hole: ResidentHole,
    pub(crate) child: CapturedChildLaunch,
}

/// Rooted replacement recipe admitted by the controlling resident actor.
pub struct ActorReplacementDefinition {
    pub(crate) child: CapturedChildLaunch,
}

pub(crate) struct CapturedChildLaunch {
    pub lifetime: WorkerLifetime,
    pub descriptor: ActorDescriptor,
    pub spawn: Option<SpawnDefinition>,
    pub entry: RootCustody,
    pub launch_worktrees: Vec<String>,
    pub record_workspace: Option<tidepool_bridge_effects::WtWorkspaceHandle>,
    pub seed: Option<ChildSessionSeed>,
    pub exit_destination: Option<std::sync::Arc<crate::owned_result::RequestResultDestination>>,
}

/// Selected compiler interface and its original implementation closure captured
/// before the parent checkout is released. The descriptor retains the same
/// capsule through protected compilation on the fresh child session.
pub(crate) struct ChildSessionSeed {
    pub facade: Option<MaterializedFacade>,
    pub val_generation: Generation,
    pub declaration_high_water: Option<Generation>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorStartCaptureError {
    #[error(transparent)]
    Resident(#[from] tidepool_runtime::session::ResidentError),
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("actor start decoder received a non-start request")]
    UnexpectedRequest,
    #[error("actor start suspended without its child entry live payload")]
    MissingEntry,
    #[error(transparent)]
    ExactExports(#[from] tidepool_runtime::session::ExactExportError),
    #[error(transparent)]
    Facade(#[from] tidepool_runtime::session::ExactFacadeError),
    #[error("actor export `{head}` drifted from rooted definition {expected:?} to {actual:?}")]
    ShadowDrift {
        head: String,
        expected: tidepool_runtime::NominalHead,
        actual: tidepool_toolchain::declaration_join::ExportIdentity,
    },
    #[error("actor export `{head}` has more than one rooted nominal incarnation: {identities:?}")]
    AmbiguousIncarnation {
        head: String,
        identities: Vec<tidepool_runtime::NominalHead>,
    },
    #[error("actor start has no live declaration plane")]
    NoCompileView,
    #[error("requested model must be a nonempty model identifier")]
    InvalidModel,
}

impl ResidentActorStart {
    /// Decode and claim a newly suspended start request while the resident
    /// machine is checked out. The entry root is born in the unpublished
    /// child's resource scope so parent cleanup cannot revoke a successfully accepted
    /// child computation.
    pub fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: &HaskellValue,
        table: &DataConTable,
        session_id: tidepool_repr::SessionId,
        parent_actor: crate::ActorRef,
        settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let (label, site, profile, workspace) = match ActorReq::from_value(request, table)? {
            ActorReq::ActorStartWith(label, site, _, profile, workspace) => {
                (label, site, profile, workspace)
            }
            ActorReq::ActorReplaceWith(_, site, _, label, profile) => (label, site, profile, None),
            _ => return Err(ActorStartCaptureError::UnexpectedRequest),
        };
        let site = u64::try_from(site).map_err(|_| ActorStartCaptureError::UnexpectedRequest)?;
        let witness = session.request_result_type_witness(site, &parent_hole)?;
        let destination = std::sync::Arc::new(crate::owned_result::RequestResultDestination::new(
            parent_actor,
            session_id,
            witness,
            session.lease_bindings(&[]),
        ));
        let mut captured = Self::capture_decoded(
            session,
            parent_hole,
            ActorStartRequest {
                label,
                profile,
                workspace,
                session_id,
                parent_actor,
            },
            settlement,
        )?;
        captured.child.exit_destination = Some(destination);
        Ok(captured)
    }

    pub(crate) fn capture_spawn<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        context: SpawnContextWire,
        workspace: crate::fork_workspace::SpawnWorkspaceWire,
        effects: Vec<ActorEffectKeyWire>,
        label: Option<String>,
        model: Option<Model>,
        effort: Option<ForkEffort>,
        instructions: Option<String>,
        lifetime: WorkerLifetime,
        limits: Option<(i64, i64)>,
        session_id: tidepool_repr::SessionId,
        parent_actor: crate::ActorRef,
        settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        if model.as_ref().is_some_and(|model| {
            model.value().is_empty() || model.value().chars().any(char::is_whitespace)
        }) {
            return Err(ActorStartCaptureError::InvalidModel);
        }
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        // Only the explicit installer closure's exact dependencies cross a
        // fresh context boundary; ambient parent lexical bindings do not.
        let facade = materialize_entry_facade(session, entry.provenance(), settlement)?;
        if facade.is_some() && session.declaration_generation_high_water().is_none() {
            return Err(ActorStartCaptureError::NoCompileView);
        }
        let keys: Vec<_> = effects.into_iter().map(Into::into).collect();
        let fresh = matches!(&context, SpawnContextWire::FreshSpawn(_));
        let independent_machine = fresh && !keys.contains(&crate::ActorEffectKey::RepoEvent);
        let seed = independent_machine.then(|| ChildSessionSeed {
            facade: facade.clone(),
            val_generation: session.val_gen(),
            declaration_high_water: session.declaration_generation_high_water(),
        });
        let (child_session, lexical_scope) = if independent_machine {
            (
                tidepool_runtime::session::fresh_session_id(),
                tidepool_codegen::scope::ScopeId::ROOT,
            )
        } else {
            (session_id, session.mint_isolated_scope())
        };
        let checkpoint = match &context {
            SpawnContextWire::CapturedSpawn(token) => Some(token.clone()),
            SpawnContextWire::FreshSpawn(_) => None,
        };
        let mut descriptor = ActorDescriptor::new_optional(
            label,
            crate::ActorPlacement {
                session: child_session,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_capabilities(crate::ActorCapabilities::default().with_effect_keys(keys))
        .with_creator(parent_actor)
        .with_checkpoint_token(checkpoint)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades(facade.iter()))
        .with_model(model)
        .with_fork_effort(effort)
        .with_instructions(instructions)
        .with_fork_budget(limits);
        if lifetime != WorkerLifetime::RunOwned {
            descriptor = descriptor.with_supervisor_parent(parent_actor);
        }
        Ok(Self {
            parent_hole,
            child: CapturedChildLaunch {
                lifetime,
                descriptor,
                spawn: Some(SpawnDefinition { context, workspace }),
                entry,
                launch_worktrees: Vec::new(),
                record_workspace: None,
                seed,
                exit_destination: None,
            },
        })
    }

    pub(crate) fn capture_decoded<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: ActorStartRequest,
        settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorStartRequest {
            label,
            profile,
            workspace,
            session_id,
            parent_actor,
        } = request;
        let (profile, effects) = profile.resolve();
        let capabilities = match effects {
            Some(keys) => crate::ActorCapabilities::default()
                .with_effect_keys(keys.into_iter().map(Into::into).collect()),
            None => crate::ActorCapabilities::default(),
        };
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let facade = materialize_entry_facade(session, entry.provenance(), settlement)?;
        if facade.is_some() && session.declaration_generation_high_water().is_none() {
            return Err(ActorStartCaptureError::NoCompileView);
        }
        let independent_machine = !capabilities
            .effect_keys()
            .contains(&crate::ActorEffectKey::RepoEvent);
        let seed = independent_machine.then(|| ChildSessionSeed {
            facade: facade.clone(),
            val_generation: session.val_gen(),
            declaration_high_water: session.declaration_generation_high_water(),
        });
        let (launch_session, lexical_scope) = if independent_machine {
            (
                tidepool_runtime::session::fresh_session_id(),
                tidepool_codegen::scope::ScopeId::ROOT,
            )
        } else {
            (session_id, session.mint_isolated_scope())
        };
        let descriptor = ActorDescriptor::new(
            label,
            crate::ActorPlacement {
                session: launch_session,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_profile(profile)
        .with_capabilities(capabilities)
        .with_creator(parent_actor)
        .with_supervisor_parent(parent_actor)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades(facade.iter()));
        Ok(Self {
            parent_hole,
            child: CapturedChildLaunch {
                lifetime: WorkerLifetime::ActorOwned,
                descriptor,
                spawn: None,
                entry,
                launch_worktrees: Vec::new(),
                record_workspace: workspace,
                seed,
                exit_destination: None,
            },
        })
    }
}

fn materialize_entry_facade<H, O>(
    session: &ResidentSession<H, O>,
    provenance: &tidepool_runtime::session::ProgramProvenance,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<Option<MaterializedFacade>, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let heads = facade_heads(provenance);
    let scope = session.run_context().lexical_scope;
    let names: Vec<_> = heads.iter().map(String::as_str).collect();
    let surface = session.exact_exports_in_namespace(
        scope,
        tidepool_toolchain::declaration_join::ExportNamespace::Type,
        &names,
    )?;
    validate_head_incarnations(surface.declarations()?, provenance, &heads)?;
    // A child with no declaration exports needs no module or source import.
    // Its executable entry and type-site provenance remain owned by custody.
    if surface.items().is_empty() {
        return Ok(None);
    }
    let view = session
        .compile_view_in(scope)
        .ok_or(ActorStartCaptureError::NoCompileView)?;
    Ok(Some(surface.materialize(&view, settlement)?))
}

fn validate_head_incarnations(
    declarations: &[tidepool_toolchain::declaration_join::DeclarationExport],
    provenance: &tidepool_runtime::session::ProgramProvenance,
    selected: &BTreeSet<String>,
) -> Result<(), ActorStartCaptureError> {
    use tidepool_toolchain::declaration_join::ExportNamespace;
    let mut rooted: std::collections::BTreeMap<String, BTreeSet<tidepool_runtime::NominalHead>> =
        std::collections::BTreeMap::new();
    for site in provenance.sites() {
        for head in site
            .heads
            .iter()
            .chain(site.inputs.iter().flat_map(|input| input.heads.iter()))
            .filter(|head| head.module.starts_with("Tidepool.Session.Lib.G"))
        {
            if selected.contains(&head.name) {
                rooted
                    .entry(head.name.clone())
                    .or_default()
                    .insert(head.clone());
            }
        }
    }

    for (head, identities) in rooted {
        if identities.len() != 1 {
            return Err(ActorStartCaptureError::AmbiguousIncarnation {
                head,
                identities: identities.into_iter().collect(),
            });
        }
        let expected = identities.into_iter().next().expect("one rooted identity");
        let actual = declarations
            .iter()
            .find(|export| {
                export.head.namespace == ExportNamespace::Type && export.head.occurrence == head
            })
            .ok_or(tidepool_runtime::session::ExactExportError::UncertifiedExports)?;
        if actual.head.unit != expected.unit
            || actual.head.module != expected.module
            || actual.head.occurrence != expected.name
        {
            return Err(ActorStartCaptureError::ShadowDrift {
                head,
                expected,
                actual: actual.head.clone(),
            });
        }
    }
    Ok(())
}

fn facade_heads(provenance: &tidepool_runtime::session::ProgramProvenance) -> BTreeSet<String> {
    let mut heads = BTreeSet::new();
    for site in provenance.sites() {
        for head in site
            .heads
            .iter()
            .chain(site.inputs.iter().flat_map(|input| input.heads.iter()))
        {
            if head.module.starts_with("Tidepool.Session.Lib.G") {
                heads.insert(head.name.clone());
            }
        }
    }
    heads
}

#[cfg(test)]
mod tests {

    #[test]
    fn empty_provenance_requires_no_child_facade() {
        let (session, root) = facade_selection_fixture();
        let facade = super::materialize_entry_facade(
            &session,
            &tidepool_runtime::session::ProgramProvenance::default(),
        )
        .unwrap();
        assert!(facade.is_none());
        assert_eq!(
            crate::ActorSourceImports::from_exact_facades(facade.iter()),
            crate::ActorSourceImports::default()
        );
        assert!(!root.path().join("Tidepool/Actor/Surface").exists());
    }

    fn facade_selection_fixture() -> (
        tidepool_runtime::session::ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        tempfile::TempDir,
    ) {
        let root = tempfile::tempdir().unwrap();
        let lib = tidepool_runtime::session::SessionLib::open(
            tidepool_repr::SessionId(17),
            root.path(),
            tidepool_runtime::session::ModuleEnv::standalone_default(),
        )
        .unwrap();
        (
            tidepool_runtime::session::ResidentSession::unbootstrapped(
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                Some(lib),
            ),
            root,
        )
    }

    #[test]
    fn nonempty_facade_selection_still_refuses_unknown_exports() {
        let (session, root) = facade_selection_fixture();
        let provenance = tidepool_runtime::session::ProgramProvenance::from_sites(&[
            tidepool_runtime::YieldSite {
                input_type_witnesses: Vec::new(),
                site: 17,
                origin: "entry".into(),
                ordinal: 0,
                ty: "Missing".into(),
                modules: vec!["Tidepool.Session.Lib.G1".into()],
                heads: vec![tidepool_runtime::NominalHead {
                    unit: "main".into(),
                    module: "Tidepool.Session.Lib.G1".into(),
                    name: "Missing".into(),
                }],
                inputs: vec![],
                reply_declaration: None,
                request_type_signatures: None,
            },
        ])
        .unwrap();
        assert!(
            matches!(super::materialize_entry_facade(&session, &provenance),
            Err(super::ActorStartCaptureError::ExactExports(
                tidepool_runtime::session::ExactExportError::UnknownExport { name, .. },
            )) if name == "Missing")
        );
        assert!(!root.path().join("Tidepool/Actor/Surface").exists());
    }

    fn nominal_provenance(
        heads: Vec<tidepool_runtime::NominalHead>,
    ) -> tidepool_runtime::session::ProgramProvenance {
        tidepool_runtime::session::ProgramProvenance::from_sites(&[tidepool_runtime::YieldSite {
            input_type_witnesses: Vec::new(),
            site: 17,
            origin: "entry".into(),
            ordinal: 0,
            ty: "Original".into(),
            modules: heads.iter().map(|head| head.module.clone()).collect(),
            heads,
            inputs: vec![],
            reply_declaration: None,
            request_type_signatures: None,
        }])
        .unwrap()
    }

    fn original_nominal() -> tidepool_runtime::NominalHead {
        tidepool_runtime::NominalHead {
            unit: "main".into(),
            module: "Tidepool.Session.Lib.G1".into(),
            name: "Original".into(),
        }
    }

    fn original_declaration() -> tidepool_toolchain::declaration_join::DeclarationExport {
        use tidepool_toolchain::declaration_join::{
            DeclarationExport, DeclarationKind, ExportIdentity, ExportNamespace,
        };
        let head = original_nominal();
        DeclarationExport {
            kind: DeclarationKind::Type,
            head: ExportIdentity {
                unit: head.unit,
                module: head.module,
                namespace: ExportNamespace::Type,
                occurrence: head.name,
                record_parent: None,
            },
            children: Vec::new(),
        }
    }

    #[test]
    fn facade_nominal_identity_preserves_original_owner_after_join() {
        let provenance = nominal_provenance(vec![original_nominal()]);
        let selected = super::facade_heads(&provenance);
        // The facade's current wrapper generation is deliberately not an input
        // to validation: the compiler export retains its original G1 owner.
        super::validate_head_incarnations(&[original_declaration()], &provenance, &selected)
            .unwrap();
    }

    #[test]
    fn facade_nominal_identity_refuses_unit_module_namespace_and_ambiguity_drift() {
        use tidepool_toolchain::declaration_join::ExportNamespace;
        let provenance = nominal_provenance(vec![original_nominal()]);
        let selected = super::facade_heads(&provenance);
        for foreign_unit in [false, true] {
            let mut actual = original_declaration();
            if foreign_unit {
                actual.head.unit = "foreign".into();
            } else {
                actual.head.module = "Tidepool.Session.Lib.G9".into();
            }
            assert!(matches!(
                super::validate_head_incarnations(&[actual], &provenance, &selected),
                Err(super::ActorStartCaptureError::ShadowDrift { .. })
            ));
        }
        let mut value = original_declaration();
        value.head.namespace = ExportNamespace::Value;
        assert!(matches!(
            super::validate_head_incarnations(&[value], &provenance, &selected),
            Err(super::ActorStartCaptureError::ExactExports(
                tidepool_runtime::session::ExactExportError::UncertifiedExports
            ))
        ));
        let mut other = original_nominal();
        other.unit = "foreign".into();
        let ambiguous = nominal_provenance(vec![original_nominal(), other]);
        assert!(matches!(
            super::validate_head_incarnations(&[original_declaration()], &ambiguous, &selected),
            Err(super::ActorStartCaptureError::AmbiguousIncarnation { .. })
        ));
    }
}
