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

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]
pub enum ForkContext {
    InheritedContext,
    SelectedContext,
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
pub enum SpawnError {
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnRefused(String),
    #[haskell(module = "Tidepool.Effects.Core")]
    SpawnPartiallyStarted((i64, i64), Option<tidepool_bridge_effects::WtWorktreeHandle>, String),
}

pub(crate) struct SpawnDefinition {
    pub context: SpawnContextWire,
    pub workspace: crate::fork_workspace::SpawnWorkspaceWire,
}

pub(crate) struct ActorStartRequest {
    pub label: String,
    pub role: ActorLaunchRoleWire,
    pub profile: ActorEffectProfileWire,
    pub launch_worktrees: Vec<String>,
    pub fork_group: Option<crate::ForkGroupId>,
    pub fork_workspace: Option<crate::ForkWorkspaceSeed>,
    pub effect_keys: Option<Vec<ActorEffectKeyWire>>,
    pub fork_effort: Option<ForkEffort>,
    pub model: Option<Model>,
    pub instructions: Option<String>,
    pub context: ForkContext,
    pub checkpoint: Option<String>,
    pub lifetime: WorkerLifetime,
    pub fork_budget: Option<(i64, i64)>,
    pub session_id: tidepool_repr::SessionId,
    pub parent_actor: crate::ActorRef,
    /// `Just label` on the wire exactly when this launch is the stdlib's
    /// `agentDefinitionUnbound label` (`startForkedAgent`/`AgentLaunchWith`'s
    /// bare `startAgent`), the shape a selected-context launch is eligible to
    /// give its own child session — its entry closure reaches that session by
    /// evacuation. See `child_session_eligibility`.
    pub unbound_label: Option<String>,
}

impl ActorStartRequest {
    /// Explicit record services remain owned by their creating actor.
    pub(crate) const RECORD_SERVICE_LIFETIME: WorkerLifetime = WorkerLifetime::ActorOwned;
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub enum ActorEffectProfileWire {
    ActorReadWriteProfile,
    ActorReadOnlyProfile,
    ActorSelectedProfile(Vec<ActorEffectKeyWire>),
}

impl ActorEffectProfileWire {
    fn resolve(
        self,
        launch_keys: Option<Vec<ActorEffectKeyWire>>,
    ) -> Result<(crate::ActorEffectProfile, Option<Vec<ActorEffectKeyWire>>), ActorStartCaptureError>
    {
        match (self, launch_keys) {
            (Self::ActorSelectedProfile(_), Some(_)) => {
                Err(ActorStartCaptureError::DuplicateEffectSelection)
            }
            (Self::ActorSelectedProfile(keys), None) => {
                Ok((crate::ActorEffectProfile::ReadOnly, Some(keys)))
            }
            (Self::ActorReadOnlyProfile, keys) => Ok((crate::ActorEffectProfile::ReadOnly, keys)),
            (Self::ActorReadWriteProfile, keys) => Ok((crate::ActorEffectProfile::ReadWrite, keys)),
        }
    }
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub enum ActorLaunchRoleWire {
    ActorRootRole,
    ActorResearchRole,
    ActorCodingRole,
    ActorScaffoldingRole,
    ActorIntegrationRole,
    ActorInheritedRole,
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub enum ActorEffectKeyWire {
    EffectResourceScopes,
    EffectReplies,
    EffectWatches,
    EffectForks,
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
            ActorEffectKeyWire::EffectForks => Self::Forks,
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
    /// Still resident on the LAUNCHING session's machine, whether or not
    /// the descriptor's own placement names a different, freshly minted
    /// one (an eligible `SelectedContext` launch — see
    /// `child_session_eligibility`). Crossing it to the child's own machine
    /// is the launch path's job, not capture's: `resident_actor.rs`'s
    /// `child_launch::await_launch` (and `replacement.rs`'s matching resolution)
    /// provisions that session and calls
    /// `ResidentActorRunner::transfer_custody` once the checkout that
    /// captured this launch has long since been released — never here,
    /// while it is still held.
    pub entry: RootCustody,
    pub launch_worktrees: Vec<String>,
    pub fork_workspace: Option<crate::ForkWorkspaceSeed>,
    /// `Some` exactly for an eligible launch (see `child_session_eligibility`):
    /// everything `ResidentActorRunner::provision_child_session` needs to
    /// give the child's own, freshly built session what its first cell (the
    /// tool installer) will need to compile against any selected declaration
    /// facade. A launch with no declaration exports carries no facade.
    /// `None` for a same-session launch, whose selected source is already
    /// reachable from the session that will run it.
    pub seed: Option<ChildSessionSeed>,
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
    #[error("a checkpoint requires an inherited fork group")]
    InvalidCheckpointContext,
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
    #[error("context fork parent lexical scope is no longer live")]
    ParentScopeRetired,
    #[error("context fork carried invalid group id {0}")]
    InvalidForkGroup(i64),
    #[error("requested model must be a nonempty model identifier")]
    InvalidModel,
    #[error("select actor effects in its profile or launch options, not both")]
    DuplicateEffectSelection,
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
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let (label, role, profile, launch_worktrees, fork_group) =
            match ActorReq::from_value(request, table)? {
                ActorReq::ActorStartWith(label, _, role, profile, worktrees) => {
                    (label, role, profile, worktrees, None)
                }
                ActorReq::ActorForkWith(label, _, group, role, profile, worktrees) => {
                    let group = u64::try_from(group)
                        .map_err(|_| ActorStartCaptureError::InvalidForkGroup(group))?;
                    (
                        label,
                        role,
                        profile,
                        worktrees,
                        Some(crate::ForkGroupId(group)),
                    )
                }
                ActorReq::ActorReplaceWith(_, _, label, profile, worktrees) => (
                    label,
                    ActorLaunchRoleWire::ActorInheritedRole,
                    profile,
                    worktrees,
                    None,
                ),
                _ => return Err(ActorStartCaptureError::UnexpectedRequest),
            };
        Self::capture_decoded(
            session,
            parent_hole,
            ActorStartRequest {
                label,
                role,
                profile,
                launch_worktrees,
                fork_group,
                fork_workspace: None,
                effect_keys: None,
                fork_effort: None,
                model: None,
                instructions: None,
                context: ForkContext::SelectedContext,
                checkpoint: None,
                lifetime: ActorStartRequest::RECORD_SERVICE_LIFETIME,
                fork_budget: None,
                session_id,
                parent_actor,
                // `Tidepool.Actor`'s own `start`/`fork` (this entry point's
                // caller) always carries a caller-authored `ActorDefinition`,
                // never the stdlib's `agentDefinitionUnbound` — but that no
                // longer decides eligibility (see `child_session_eligibility`):
                // a `SelectedContext` launch from here is eligible for its
                // own machine session too, its entry crossing as a parcel.
                unbound_label: None,
            },
        )
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
    ) -> Result<Self, ActorStartCaptureError>
    where H: DispatchEffect<O> + Send, O: OutputSink + Sync,
    {
        if model.as_ref().is_some_and(|model| {
            model.value().is_empty() || model.value().chars().any(char::is_whitespace)
        }) {
            return Err(ActorStartCaptureError::InvalidModel);
        }
        let child_realm = RealmId::fresh();
        let entry = session.live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        // Only the explicit installer closure's exact dependencies cross a
        // fresh context boundary; ambient parent lexical bindings do not.
        let facade = materialize_entry_facade(session, entry.provenance())?;
        let keys: Vec<_> = effects.into_iter().map(Into::into).collect();
        let fresh = matches!(&context, SpawnContextWire::FreshSpawn(_));
        let independent_machine = fresh && !keys.contains(&crate::ActorEffectKey::RepoEvent);
        let seed = independent_machine.then(|| ChildSessionSeed {
            facade: facade.clone(),
            val_generation: session.val_gen(),
            declaration_high_water: session.declaration_generation_high_water(),
        });
        let (child_session, lexical_scope) = if independent_machine {
            (tidepool_runtime::session::fresh_session_id(), tidepool_codegen::scope::ScopeId::ROOT)
        } else {
            (session_id, session.mint_isolated_scope())
        };
        let checkpoint = match &context {
            SpawnContextWire::CapturedSpawn(token) => Some(token.clone()),
            SpawnContextWire::FreshSpawn(_) => None,
        };
        let mut descriptor = ActorDescriptor::new(label.unwrap_or_default(), crate::ActorPlacement {
            session: child_session, resource_scope: child_realm, lexical_scope,
        })
        .with_capabilities(crate::ActorCapabilities::default().with_effect_keys(keys))
        .with_creator(parent_actor)
        .with_checkpoint_token(checkpoint)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades(facade.iter()))
        .with_model(model).with_fork_effort(effort).with_instructions(instructions)
        .with_fork_budget(limits);
        if lifetime != WorkerLifetime::RunOwned {
            descriptor = descriptor.with_supervisor_parent(parent_actor);
        }
        Ok(Self { parent_hole, child: CapturedChildLaunch {
            lifetime, descriptor, spawn: Some(SpawnDefinition { context, workspace }),
            entry, launch_worktrees: Vec::new(), fork_workspace: None, seed,
        } })
    }

    pub(crate) fn capture_decoded<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: ActorStartRequest,
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorStartRequest {
            label,
            role,
            profile,
            launch_worktrees,
            fork_group,
            fork_workspace,
            effect_keys,
            fork_effort,
            model,
            instructions,
            context,
            checkpoint,
            lifetime,
            fork_budget,
            session_id,
            parent_actor,
            unbound_label,
        } = request;
        if model.as_ref().is_some_and(|model| {
            model.value().is_empty() || model.value().chars().any(char::is_whitespace)
        }) {
            return Err(ActorStartCaptureError::InvalidModel);
        }
        let (profile, effect_keys) = profile.resolve(effect_keys)?;
        if checkpoint.is_some()
            && (fork_group.is_none() || context != ForkContext::InheritedContext)
        {
            return Err(ActorStartCaptureError::InvalidCheckpointContext);
        }
        let context_fork = fork_group.is_some() && context == ForkContext::InheritedContext;
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        // Capture independently of the invoking private scope's retirement.
        // The original compiler context and binding shares stay with this lease.
        let inherited_scope = if context_fork && checkpoint.is_none() {
            Some(session.retain_lexical_scope(session.run_context().lexical_scope)?)
        } else {
            None
        };
        let facade = if inherited_scope.is_some() {
            None
        } else {
            materialize_entry_facade(session, entry.provenance())?
        };
        let source_imports = match &inherited_scope {
            Some(scope) => crate::ActorSourceImports::from_inherited_scope(scope.clone()),
            None => crate::ActorSourceImports::from_exact_facades(facade.iter()),
        };
        let effective_role = role.effective_role(!launch_worktrees.is_empty());
        let effective_role = match effect_keys {
            Some(keys) => {
                effective_role.with_effect_keys(keys.into_iter().map(Into::into).collect())
            }
            None => effective_role,
        };
        // Decided from the RESOLVED effect keys, not the wire role alone: a
        // future change that grants RepoEvent (or widens the row some other
        // way) to an `ActorInheritedRole`/`ActorSelectedProfile` launch turns
        // eligibility off by itself, rather than this check silently going
        // stale and a launch reaching the inert `RepoEventHandler` source a
        // fresh session installs for it (see `bridge/handlers`'s
        // `InertObservationSource`).
        let eligibility = child_session_eligibility(
            context,
            unbound_label.as_deref(),
            effective_role.effect_keys(),
        );
        tracing::info!(
            actor_label = %label,
            parent = ?parent_actor,
            context = ?context,
            eligible = eligibility.eligible,
            reason = eligibility.reason,
            "selected-context child session eligibility decided"
        );
        // Only an eligible launch needs a seed at all — read while this,
        // the parent's, checkout is still held, since the child session
        // this seeds does not exist yet (see `ChildSessionSeed`'s doc
        // comment).
        let seed = if eligibility.eligible {
            let declaration_high_water = session.declaration_generation_high_water();
            if facade.is_some() && declaration_high_water.is_none() {
                return Err(ActorStartCaptureError::NoCompileView);
            }
            Some(ChildSessionSeed {
                facade: facade.clone(),
                val_generation: session.val_gen(),
                declaration_high_water,
            })
        } else {
            None
        };
        // An eligible launch gets its own fresh session id here, recorded
        // in the descriptor's placement — but the entry itself stays
        // resident on THIS, the parent's, session; crossing it to the
        // child's machine is `try_start_child`'s job, once the checkout
        // that captured this launch is long since released (never both
        // sessions checked out at once). An ineligible launch, or any
        // `InheritedContext` fork, is unchanged: it keeps the launching
        // session's id, and never crosses at all.
        //
        // The lexical scope is minted on THIS, the parent's, session too —
        // but only when there is no later child session to mint it on
        // instead: an eligible launch's own scope belongs to the CHILD's
        // scope forest, minted there once `try_start_child` provisions it
        // (`ActorDescriptor::with_lexical_scope` replaces this placeholder).
        // Minting one here anyway, for a scope nothing on the parent will
        // ever use, would leak an empty scope into the parent's forest on
        // every eligible launch.
        let (launch_session, lexical_scope) = if eligibility.eligible {
            (
                tidepool_runtime::session::fresh_session_id(),
                tidepool_codegen::scope::ScopeId::ROOT,
            )
        } else if context_fork {
            (
                session_id,
                match &inherited_scope {
                    Some(scope) => session.mint_scope_from_lease(scope)?,
                    None => session
                        .mint_scope(session.run_context().lexical_scope)
                        .ok_or(ActorStartCaptureError::ParentScopeRetired)?,
                },
            )
        } else {
            (session_id, session.mint_isolated_scope())
        };
        let mut descriptor = ActorDescriptor::new(
            label,
            crate::ActorPlacement {
                session: launch_session,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_profile(profile)
        .with_capabilities(effective_role)
        .with_fork_effort(fork_effort)
        .with_model(model)
        .with_instructions(instructions)
        .with_fork_budget(fork_budget)
        .with_creator(parent_actor)
        .with_checkpoint_token(checkpoint)
        .with_source_imports(source_imports);
        if lifetime != WorkerLifetime::RunOwned {
            descriptor = descriptor.with_supervisor_parent(parent_actor);
        }
        if context_fork {
            descriptor = descriptor.with_context_parent(parent_actor);
        }
        if let Some(group) = fork_group {
            descriptor = descriptor.with_fork_group(group);
        }
        Ok(Self {
            parent_hole,
            child: CapturedChildLaunch {
                lifetime,
                descriptor,
                spawn: None,
                entry,
                launch_worktrees,
                fork_workspace,
                seed,
            },
        })
    }
}

/// Whether a launch's `entry` closure is eligible to run on its own fresh
/// child session, decided from data the wire request already carries —
/// never by inspecting the captured `entry` value itself (closures/thunks
/// never cross the bridge; see `tidepool_bridge::HaskellValue`'s doc
/// comment). An eligible entry reaches its child session by evacuation.
struct ChildSessionEligibility {
    eligible: bool,
    reason: &'static str,
}

/// A launch is eligible for its own machine session exactly when the model
/// asked for a selected (not inherited) context, and the resolved effect row
/// does not need RepoEvent (a fresh session's `RepoEventHandler` is always
/// the inert one — see below). Any `InheritedContext` fork keeps running on
/// the launching session, unchanged: it shares the parent's lexical scope
/// chain and declaration generations, which only make sense on one machine.
fn child_session_eligibility(
    context: ForkContext,
    // No longer decides eligibility (every `SelectedContext` launch is
    // eligible, unbound label or not — see this function's doc comment) but
    // kept as a parameter: every caller already has it in hand from the wire
    // request, and it stays useful as tracing context for the "selected-context
    // child session eligibility decided" log line at the one call site.
    _unbound_label: Option<&str>,
    resolved_effect_keys: &[crate::ActorEffectKey],
) -> ChildSessionEligibility {
    match context {
        ForkContext::SelectedContext => {
            // The inert `RepoEventHandler` a fresh session installs for this
            // actor never dispatches RepoEvent; if the RESOLVED row somehow
            // grants that key anyway, refuse instead of reaching it. Checked
            // here, not assumed from any particular launch shape, precisely
            // so a later row change is caught by this check rather than by a
            // runtime error inside the inert source.
            if resolved_effect_keys.contains(&crate::ActorEffectKey::RepoEvent) {
                ChildSessionEligibility {
                    eligible: false,
                    reason: "selected context, but the resolved effect row includes \
                             RepoEvent — a fresh session's RepoEventHandler cannot \
                             serve it",
                }
            } else {
                ChildSessionEligibility {
                    eligible: true,
                    reason: "selected context, no RepoEvent in the resolved row",
                }
            }
        }
        ForkContext::InheritedContext => ChildSessionEligibility {
            eligible: false,
            reason: "inherited context shares the parent's scope chain and generations",
        },
    }
}

fn materialize_entry_facade<H, O>(
    session: &ResidentSession<H, O>,
    provenance: &tidepool_runtime::session::ProgramProvenance,
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
    Ok(Some(surface.materialize(&view)?))
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

impl ActorLaunchRoleWire {
    pub(crate) fn effective_role(self, has_worktree: bool) -> crate::EffectiveRole {
        match self {
            ActorLaunchRoleWire::ActorRootRole => crate::EffectiveRole::root(),
            ActorLaunchRoleWire::ActorResearchRole => crate::EffectiveRole::research(),
            ActorLaunchRoleWire::ActorCodingRole => crate::ActorCapabilities::default(),
            ActorLaunchRoleWire::ActorScaffoldingRole => {
                crate::EffectiveRole::scaffolding(crate::DescendantBudget {
                    maximum_depth: 0,
                    maximum_active_children: Some(0),
                })
            }
            ActorLaunchRoleWire::ActorIntegrationRole => crate::EffectiveRole::integration(),
            ActorLaunchRoleWire::ActorInheritedRole => {
                if !has_worktree {
                    crate::EffectiveRole::research()
                } else {
                    crate::ActorCapabilities::default()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn selected_profile_preserves_exact_keys_and_rejects_a_second_selection() {
        use super::{
            ActorEffectKeyWire as Key, ActorEffectProfileWire as Profile, ActorStartCaptureError,
        };
        let (profile, keys) =
            Profile::ActorSelectedProfile(vec![Key::EffectReplies, Key::EffectActor])
                .resolve(None)
                .unwrap();
        assert_eq!(profile, crate::ActorEffectProfile::ReadOnly);
        assert_eq!(
            keys.unwrap()
                .into_iter()
                .map(crate::ActorEffectKey::from)
                .collect::<Vec<_>>(),
            vec![crate::ActorEffectKey::Replies, crate::ActorEffectKey::Actor]
        );
        assert!(matches!(
            Profile::ActorSelectedProfile(vec![Key::EffectReplies])
                .resolve(Some(vec![Key::EffectActor])),
            Err(ActorStartCaptureError::DuplicateEffectSelection)
        ));
        let (_, empty) = Profile::ActorSelectedProfile(vec![]).resolve(None).unwrap();
        assert!(empty.unwrap().is_empty());
    }

    #[test]
    fn unbound_launch_in_selected_context_is_eligible() {
        use super::{child_session_eligibility, ForkContext};
        let decision =
            child_session_eligibility(ForkContext::SelectedContext, Some("luna/implement"), &[]);
        assert!(decision.eligible);
    }

    #[test]
    fn selected_context_without_unbound_label_is_eligible() {
        // A caller-authored `ActorDefinition` (no `agentDefinitionUnbound`
        // label) is now just as eligible as an unbound launch: its entry
        // crosses to the child session as a parcel instead of being
        // reconstructed there.
        use super::{child_session_eligibility, ForkContext};
        let decision = child_session_eligibility(ForkContext::SelectedContext, None, &[]);
        assert!(decision.eligible);
    }

    #[test]
    fn inherited_context_is_ineligible_even_with_unbound_label() {
        use super::{child_session_eligibility, ForkContext};
        let decision =
            child_session_eligibility(ForkContext::InheritedContext, Some("luna/implement"), &[]);
        assert!(!decision.eligible);
    }

    #[test]
    fn unbound_launch_is_ineligible_when_the_resolved_row_grants_repo_event() {
        use super::{child_session_eligibility, ForkContext};
        let decision = child_session_eligibility(
            ForkContext::SelectedContext,
            Some("luna/implement"),
            &[crate::ActorEffectKey::RepoEvent],
        );
        assert!(
            !decision.eligible,
            "a resolved row granting RepoEvent must turn eligibility off, \
             since a fresh session's RepoEventHandler cannot serve it"
        );
    }

    #[test]
    fn bound_launch_is_also_ineligible_when_the_resolved_row_grants_repo_event() {
        // The RepoEvent exclusion is decided from the resolved effect row
        // alone, independent of whether the entry is an unbound stdlib
        // launch or a caller-authored `ActorDefinition`.
        use super::{child_session_eligibility, ForkContext};
        let decision = child_session_eligibility(
            ForkContext::SelectedContext,
            None,
            &[crate::ActorEffectKey::RepoEvent],
        );
        assert!(!decision.eligible);
    }

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

    /// The errand's authority comes from holding no worktree, not from a
    /// request the caller makes: `AgentLaunchWith` carries the inherited role,
    /// and a worktree-less launch resolves it to research. An errand child
    /// cannot write, and cannot start a descendant of its own.
    #[test]
    fn a_worktree_less_launch_can_neither_write_nor_spawn() {
        let errand = super::ActorLaunchRoleWire::ActorInheritedRole.effective_role(false);
        assert_eq!(errand.role(), crate::ActorRole::Research);
        assert_eq!(
            errand.native_tools(),
            crate::NativeToolClass::InspectionOnly
        );
        assert_eq!(errand.workspace(), crate::WorkspaceAccess::InspectOnly);
        assert_eq!(errand.descendants().maximum_active_children, Some(0));
        assert_eq!(errand.descendants().maximum_depth, 0);
        assert!(
            !errand.permits_child(&crate::EffectiveRole::research()),
            "an errand child must not admit a child of its own"
        );
        assert!(
            !errand
                .effect_keys()
                .contains(&crate::ActorEffectKey::AgentLaunch),
            "an errand child must not hold launch authority"
        );

        // The same launch WITH a worktree is the coding role, which is exactly
        // what the errand declines to allocate.
        let with_tree = super::ActorLaunchRoleWire::ActorInheritedRole.effective_role(true);
        assert_eq!(with_tree.workspace(), crate::WorkspaceAccess::WritableBound);
    }
}

/// Static launch inputs after actor authority attenuation. The host resolves its
/// provider defaults and frozen prompts; the actor runtime owns admission.
pub struct WorkerLaunchRequest {
    pub capabilities: crate::ActorCapabilities,
    pub model: Option<Model>,
    pub effort: Option<ForkEffort>,
    pub context: ForkContext,
    pub instructions: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core")]
pub struct WorkerLaunchPreview {
    pub model: Option<String>,
    pub effort: ForkEffort,
    pub instructions: String,
    pub base_fingerprint: String,
    pub workspace_identity: Option<String>,
    pub modules: Vec<String>,
}

/// Installed once when constructing a forest. No provider call or file reload.
pub type WorkerLaunchResolver = std::sync::Arc<
    dyn Fn(&WorkerLaunchRequest) -> Result<WorkerLaunchPreview, String> + Send + Sync,
>;
