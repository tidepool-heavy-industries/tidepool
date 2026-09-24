//! Capture of a public Haskell actor launch or replacement entry.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::collections::BTreeSet;
use std::path::Path;
use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, FromHaskell};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_repr::{DataConTable, Generation, SessionModule};
use tidepool_runtime::session::{
    assemble_display_expression_module, run_turn, ExpressionLift, MaterializedFacade, OutputSink,
    ResidentHole, ResidentOutcome, ResidentSession, RootCustody, TemplateSelector, TurnRequest,
    TurnResult, TurnTemplate,
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
)]
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
    ParentOwned,
    SwarmOwned,
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
    pub lifetime: WorkerLifetime,
    pub fork_budget: Option<(i64, i64)>,
    pub session_id: tidepool_repr::SessionId,
    pub parent_actor: crate::ActorRef,
    /// `Just label` on the wire exactly when this launch is the stdlib's
    /// `agentDefinitionUnbound label` (`startForkedAgent`/`AgentLaunchWith`'s
    /// bare `startAgent`), the only launch shape whose captured `entry` is
    /// data-reconstructible. See `child_session_eligibility`.
    pub unbound_label: Option<String>,
}

impl ActorStartRequest {
    /// A launch that opens no fork group — `startActor` and `startAgent`,
    /// including the read-only errand — belongs to the actor that asked for
    /// it. `ParentOwned` records a supervisor parent (below) and spawns the
    /// child linked (`local_actor.rs`), so nothing has to retire it by hand.
    pub(crate) const FRESH_LAUNCH_LIFETIME: WorkerLifetime = WorkerLifetime::ParentOwned;
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
    pub descriptor: ActorDescriptor,
    pub entry: RootCustody,
    pub launch_worktrees: Vec<String>,
    pub fork_workspace: Option<crate::ForkWorkspaceSeed>,
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
    #[error(
        "actor export `{head}` drifted from rooted definition module `{expected}` to `{actual}`"
    )]
    ShadowDrift {
        head: String,
        expected: String,
        actual: String,
    },
    #[error("actor export `{head}` has more than one rooted nominal incarnation: {modules:?}")]
    AmbiguousIncarnation { head: String, modules: Vec<String> },
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
    #[error("could not reconstruct an unbound agent launch's entry on its own session: {0}")]
    UnboundReconstructionFailed(String),
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
                lifetime: ActorStartRequest::FRESH_LAUNCH_LIFETIME,
                fork_budget: None,
                session_id,
                parent_actor,
                // `Tidepool.Actor`'s own `start`/`fork` (this entry point's
                // caller) always carries a caller-authored `ActorDefinition`,
                // never the stdlib's `agentDefinitionUnbound` — never eligible.
                unbound_label: None,
            },
        )
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
        let context_fork = fork_group.is_some() && context == ForkContext::InheritedContext;
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)?
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let facade = materialize_entry_facade(session, &entry)?;
        let lexical_scope = if context_fork {
            session
                .mint_scope(session.run_context().lexical_scope)
                .ok_or(ActorStartCaptureError::ParentScopeRetired)?
        } else {
            session.mint_isolated_scope()
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
        // The fresh-machine factory (a shared "spawn child session" primitive
        // extracted from `bridge/facade`'s root bootstrap) does not exist yet
        // — see plans/wave3/dives/per-child-sessions-design.md parcel 2.
        // Every launch, eligible or not, still gets the launching session's
        // id until that factory lands and its caller wires it in; this call
        // site is the seam.
        let mut descriptor = ActorDescriptor::new(
            label,
            crate::ActorPlacement {
                session: session_id,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_profile(profile)
        .with_effective_role(effective_role)
        .with_fork_effort(fork_effort)
        .with_model(model)
        .with_instructions(instructions)
        .with_fork_budget(fork_budget)
        .with_creator(parent_actor)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades([&facade]));
        if lifetime == WorkerLifetime::ParentOwned {
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
                descriptor,
                entry,
                launch_worktrees,
                fork_workspace,
            },
        })
    }
}

/// Whether a launch's `entry` closure is reconstructible on a fresh session,
/// decided from data the wire request already carries — never by inspecting
/// the captured `entry` value itself (closures/thunks never cross the bridge;
/// see `tidepool_bridge::HaskellValue`'s doc comment).
struct ChildSessionEligibility {
    eligible: bool,
    reason: &'static str,
}

/// A launch is eligible for its own machine session only when BOTH: the
/// model asked for a selected (not inherited) context, AND the entry is the
/// stdlib's `agentDefinitionUnbound <label>` — the one shape whose captured
/// `entry` closes over nothing but that label (see
/// `bridge/haskell/actors/Tidepool/Actors/Internal/Agent.hs`'s
/// `agentDefinitionUnbound`/`startForkedAgent`). Every other launch (a
/// caller-authored `ActorDefinition`, or any `InheritedContext` fork) keeps
/// running on the launching session, unchanged.
fn child_session_eligibility(
    context: ForkContext,
    unbound_label: Option<&str>,
    resolved_effect_keys: &[crate::ActorEffectKey],
) -> ChildSessionEligibility {
    match (context, unbound_label) {
        (ForkContext::SelectedContext, Some(_)) => {
            // The inert `RepoEventHandler` a fresh session installs for this
            // actor never dispatches RepoEvent; if the RESOLVED row somehow
            // grants that key anyway, refuse instead of reaching it. Checked
            // here, not assumed from `agentDefinitionUnbound`'s fixed
            // `ReadOnlyEffects AgentProtocol` shape, precisely so a later row
            // change is caught by this check rather than by a runtime error
            // inside the inert source.
            if resolved_effect_keys.contains(&crate::ActorEffectKey::RepoEvent) {
                ChildSessionEligibility {
                    eligible: false,
                    reason: "selected context, unbound agent launch, but the resolved \
                             effect row includes RepoEvent — a fresh session's \
                             RepoEventHandler cannot serve it",
                }
            } else {
                ChildSessionEligibility {
                    eligible: true,
                    reason:
                        "selected context, unbound agent launch, no RepoEvent in the resolved row",
                }
            }
        }
        (ForkContext::SelectedContext, None) => ChildSessionEligibility {
            eligible: false,
            reason: "selected context, but entry is a caller-authored ActorDefinition \
                     (may close over live state beyond the label)",
        },
        (ForkContext::InheritedContext, _) => ChildSessionEligibility {
            eligible: false,
            reason: "inherited context shares the parent's scope chain and generations",
        },
    }
}

/// A malformed label would otherwise splice broken Haskell into the
/// reconstruction turn's source; this mirrors the workbench's own
/// string-literal escaping (`escape_workbench_haskell_string`) rather than
/// depending on it, since a wire label is untrusted the same way workbench
/// key text is.
#[allow(dead_code, reason = "wired by the child-session launch parcel")]
fn escape_haskell_string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// Compile and run, as a standalone turn on `session` (already checked out —
/// the caller owns admission), the exact expression an eligible launch's
/// entry reconstructs to: `Tidepool.Actors.Internal.Agent.unboundAgentEntry`
/// applied to `label`. `session_root` is the machine's own session root (the
/// `ChildSessionFactory` that built it knows this; `ResidentSession` does not
/// expose it back — see `reap_evicted_stub_sources_in` for the same pattern).
///
/// The turn's type is fully closed (`Eff (ActorKernel ': ReadOnlyEffects
/// AgentProtocol) ()` — see `unboundAgentEntry`'s doc comment for why this
/// specific shape, alone, can carry a standalone signature at all), so
/// `effect_stack` below names it exactly; nothing here depends on the
/// session's own declared workbench effects.
#[allow(dead_code, reason = "wired by the child-session launch parcel")]
pub(crate) fn run_unbound_entry_reconstruction<H, O>(
    session: &mut ResidentSession<H, O>,
    session_root: &Path,
    include: &[&Path],
    // The session's own base preamble (module header, standard prelude) —
    // `tidepool_mcp::build_preamble`'s result, the same call
    // `ResidentSession::unbootstrapped`'s caller already made to build this
    // session's declarations, and NOT something this function builds itself:
    // `exomonad-actor` does not depend on `tidepool-mcp` (see the crate's own
    // `H`/`O` genericity), and a hand-rolled `module Main where` won't do —
    // a bare GHC module here defaults to `Main` and demands a `main`, which
    // this turn (like any other resident turn) does not have.
    base_preamble: &str,
    label: &str,
) -> Result<ResidentOutcome, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    // `base_preamble` already aliases `T` (to `Tidepool.Data.Text`, whose
    // `pack` the workbench scaffold's own hardcoded `T.pack` line relies on)
    // and already brings `Eff` into scope — this only adds what it doesn't.
    let preamble = tidepool_runtime::session::insert_preamble_imports(
        base_preamble,
        "qualified Tidepool.Actors.Internal.Agent as TidepoolChildEntry",
    );
    let preamble = tidepool_runtime::session::insert_preamble_imports(
        &preamble,
        "qualified Tidepool.Actor.Internal as TidepoolChildEntryActorInternal",
    );
    let preamble = tidepool_runtime::session::insert_preamble_imports(
        &preamble,
        "qualified Tidepool.Agent.Ref as TidepoolChildEntryAgentRef",
    );
    let preamble = tidepool_runtime::session::insert_preamble_imports(
        &preamble,
        "qualified Tidepool.Effects.Core as TidepoolChildEntryCore",
    );
    let preamble = preamble.as_str();
    let effect_stack = "(TidepoolChildEntryCore.ActorKernel ': \
                         TidepoolChildEntryActorInternal.ReadOnlyEffects \
                         TidepoolChildEntryAgentRef.AgentProtocol)";
    let turn_text = format!(
        "TidepoolChildEntry.unboundAgentEntry (T.pack \"{}\") 0",
        escape_haskell_string_literal(label)
    );
    // Exactly one candidate, not `resident_workbench_templates`'s usual four
    // (Effectful/Pure x Display/Opaque): the reconstruction's row is fully
    // closed and known (see `run_unbound_entry_reconstruction`'s doc
    // comment), so it is always effectful, never a value to display — the
    // pure candidates exist for a REPL-style ambiguous user expression,
    // which this is not, and letting the extract try them first/instead
    // would hit the workbench's own deliberate "must typecheck in the
    // current effect row" guard for no reason.
    let templates = [TurnTemplate {
        kind: TemplateSelector::Expr,
        source: assemble_display_expression_module(
            preamble,
            "__result",
            effect_stack,
            "{{TURN}}",
            ExpressionLift::Effectful,
        ),
    }];
    let result = run_turn(TurnRequest {
        turn_text: &turn_text,
        templates: &templates,
        include,
        session_root,
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
        // A one-off reconstruction turn: no memo retention across cells.
        session_id: None,
        retained_imports: &[],
    })
    .map_err(|failure| {
        ActorStartCaptureError::UnboundReconstructionFailed(format!(
            "{:?} (attempted source: {})",
            failure.error,
            failure.attempted_source.as_deref().unwrap_or("<none>")
        ))
    })?;
    let TurnResult::Expr { compiled, .. } = result else {
        return Err(ActorStartCaptureError::UnboundReconstructionFailed(
            "unboundAgentEntry reconstruction did not compile as a bare expression".into(),
        ));
    };
    session
        .run_with_sites("unbound-agent-entry-reconstruction", compiled.code())
        .map_err(|error| ActorStartCaptureError::UnboundReconstructionFailed(error.to_string()))
}

fn materialize_entry_facade<H, O>(
    session: &ResidentSession<H, O>,
    entry: &RootCustody,
) -> Result<MaterializedFacade, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let heads = facade_heads(entry.provenance());
    let scope = session.run_context().lexical_scope;
    validate_head_incarnations(session, scope, entry.provenance(), &heads)?;
    let names: Vec<_> = heads.iter().map(String::as_str).collect();
    let surface = session.exact_exports_in(scope, &names)?;
    let view = session
        .compile_view_in(scope)
        .ok_or(ActorStartCaptureError::NoCompileView)?;
    Ok(surface.materialize(&view)?)
}

fn validate_head_incarnations<H, O>(
    session: &ResidentSession<H, O>,
    scope: tidepool_codegen::scope::ScopeId,
    provenance: &tidepool_runtime::session::ProgramProvenance,
    selected: &BTreeSet<String>,
) -> Result<(), ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let mut rooted: std::collections::BTreeMap<String, BTreeSet<String>> =
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
                    .insert(head.module.clone());
            }
        }
    }

    let visible: std::collections::BTreeMap<_, _> =
        session.current_decl_heads_in(scope).into_iter().collect();
    for (head, modules) in rooted {
        if modules.len() != 1 {
            return Err(ActorStartCaptureError::AmbiguousIncarnation {
                head,
                modules: modules.into_iter().collect(),
            });
        }
        let Some(expected) = modules.into_iter().next() else {
            unreachable!("the rooted module count was validated above");
        };
        let Some(generation) = visible.get(&head) else {
            continue;
        };
        let actual = SessionModule::lib(Generation(*generation)).module_name();
        if actual != expected {
            return Err(ActorStartCaptureError::ShadowDrift {
                head,
                expected,
                actual,
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
            ActorLaunchRoleWire::ActorCodingRole => crate::EffectiveRole::coding(),
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
                    crate::EffectiveRole::coding()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::facade_heads;

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
    fn selected_context_without_unbound_label_is_ineligible() {
        use super::{child_session_eligibility, ForkContext};
        let decision = child_session_eligibility(ForkContext::SelectedContext, None, &[]);
        assert!(!decision.eligible);
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
    fn empty_provenance_requires_no_child_facade() {
        let heads = facade_heads(&tidepool_runtime::session::ProgramProvenance::default());
        assert!(heads.is_empty());
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

    /// Nothing has to retire an errand: `AgentLaunchWith` is captured as
    /// `ParentOwned`, which records a supervisor parent and makes the child a
    /// linked worker that goes with its owner.
    #[test]
    fn a_fresh_launch_is_parent_owned_so_no_retirement_is_authored() {
        assert_eq!(
            super::ActorStartRequest::FRESH_LAUNCH_LIFETIME,
            crate::WorkerLifetime::ParentOwned
        );
    }

    /// A minimal, real (extractor-backed) machine with the actors library on
    /// its include path — everything `run_unbound_entry_reconstruction`
    /// needs and nothing an interactive workbench session also carries.
    fn bare_actors_session() -> (
        tidepool_runtime::session::ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        tempfile::TempDir,
        Vec<std::path::PathBuf>,
        String,
    ) {
        use tidepool_runtime::session::{ModuleEnv, SessionLib};
        tidepool_testing::eval_harness::require_extract();
        let declarations = [tidepool_mcp::notifications_decl()];
        let base_preamble = tidepool_mcp::build_preamble(&declarations, false);
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
        let mut include = effects.include_paths().to_vec();
        include.push(tidepool_testing::eval_harness::prelude_path());
        include.push(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../bridge/haskell/actors"),
        );
        let session_id = tidepool_repr::SessionId((u64::from(std::process::id()) << 16) | 9_001);
        let session_root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("declaration plane");
        (
            tidepool_runtime::session::ResidentSession::unbootstrapped(
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                Some(lib),
            ),
            session_root,
            include,
            base_preamble,
        )
    }

    #[test]
    fn unbound_entry_reconstruction_compiles_and_suspends_at_install_shutdown() {
        use super::run_unbound_entry_reconstruction;
        let (mut session, session_root, include, base_preamble) = bare_actors_session();
        let lexical_scope = session.mint_isolated_scope();
        let resource_scope = tidepool_codegen::suspension::RealmId::fresh();
        session
            .set_actor_execution(
                tidepool_runtime::session::SessionRunContext {
                    lexical_scope,
                    resource_scope,
                    ..tidepool_runtime::session::SessionRunContext::ROOT
                },
                tidepool_effect::EffectRunPolicy::HandleOrSuspend,
                tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .expect("actor execution context");
        let include_refs: Vec<&std::path::Path> = include.iter().map(|p| p.as_path()).collect();
        let outcome = run_unbound_entry_reconstruction(
            &mut session,
            session_root.path(),
            &include_refs,
            &base_preamble,
            "reconstruction-test-label",
        )
        .expect("unbound entry reconstructs and runs to its first suspension");
        assert!(
            matches!(
                outcome,
                tidepool_runtime::session::ResidentOutcome::Suspended { .. }
            ),
            "the reconstructed entry's first send is ActorInstallShutdownWith, \
             which suspends exactly like a captured entry's would: {outcome:?}"
        );
    }
}

/// Static launch inputs after actor authority attenuation. The host resolves its
/// provider defaults and frozen prompts; the actor runtime owns admission.
pub struct WorkerLaunchRequest {
    pub role: crate::EffectiveRole,
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
