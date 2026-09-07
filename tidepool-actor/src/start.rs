//! Capture of one public Haskell `startActor` suspension.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::collections::BTreeSet;
use tidepool_bridge::{BridgeError, FromCore};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_repr::{DataConTable, Generation, SessionModule};
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentHole, ResidentSession, RootCustody,
};

use crate::generated::actor::ActorReq;
use crate::ActorDescriptor;
#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromCore)]
pub enum ForkEffort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromCore)]
pub enum ForkContext {
    InheritedContext,
    SelectedContext,
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
    pub model: Option<String>,
    pub context: ForkContext,
    pub fork_budget: Option<(i64, i64)>,
    pub session_id: tidepool_repr::SessionId,
    pub parent_actor: crate::ActorRef,
}

#[derive(tidepool_bridge_derive::FromCore)]
pub enum ActorEffectProfileWire {
    ActorReadWriteProfile,
    ActorReadOnlyProfile,
}

#[derive(tidepool_bridge_derive::FromCore)]
pub enum ActorLaunchRoleWire {
    ActorRootRole,
    ActorResearchRole,
    ActorCodingRole,
    ActorScaffoldingRole,
    ActorIntegrationRole,
    ActorInheritedRole,
}

#[derive(tidepool_bridge_derive::FromCore)]
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
    EffectNotifications,
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
            ActorEffectKeyWire::EffectNotifications => Self::Notifications,
        }
    }
}

/// One parked parent continuation paired with exclusive custody of its child
/// entry. Compiler provenance travels with the rooted entry itself.
pub struct ResidentActorStart {
    pub(crate) parent_hole: ResidentHole,
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
}

impl ResidentActorStart {
    /// Decode and claim a newly suspended start request while the resident
    /// machine is checked out. The entry root is born in the unpublished
    /// child's realm so parent cleanup cannot revoke a successfully accepted
    /// child computation.
    pub fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: &Value,
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
                context: ForkContext::SelectedContext,
                fork_budget: None,
                session_id,
                parent_actor,
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
            context,
            fork_budget,
            session_id,
            parent_actor,
        } = request;
        if model
            .as_ref()
            .is_some_and(|model| model.is_empty() || model.chars().any(char::is_whitespace))
        {
            return Err(ActorStartCaptureError::InvalidModel);
        }
        let context_fork = fork_group.is_some() && context == ForkContext::InheritedContext;
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let profile = match profile {
            ActorEffectProfileWire::ActorReadWriteProfile => crate::ActorEffectProfile::ReadWrite,
            ActorEffectProfileWire::ActorReadOnlyProfile => crate::ActorEffectProfile::ReadOnly,
        };
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
        .with_fork_budget(fork_budget)
        .with_supervisor_parent(parent_actor)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades([&facade]));
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
                    maximum_active_children: 0,
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
    fn empty_provenance_requires_no_child_facade() {
        let heads = facade_heads(&tidepool_runtime::session::ProgramProvenance::default());
        assert!(heads.is_empty());
    }
}
