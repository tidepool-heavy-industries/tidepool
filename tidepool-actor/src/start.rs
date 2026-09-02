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

/// One parked parent continuation paired with exclusive custody of its child
/// entry. Compiler provenance travels with the rooted entry itself.
pub struct ResidentActorStart {
    descriptor: ActorDescriptor,
    parent_hole: ResidentHole,
    entry: RootCustody,
    launch_worktrees: Vec<String>,
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
    #[error("actor start carried unknown effect profile {0}")]
    UnknownProfile(i64),
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
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorReq::ActorStartWith(
            label,
            _entry_projection,
            profile,
            launch_worktrees,
            explicit_exports,
        ) = ActorReq::from_value(request, table)?
        else {
            return Err(ActorStartCaptureError::UnexpectedRequest);
        };
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let profile = match profile {
            0 => crate::ActorEffectProfile::ReadWrite,
            1 => crate::ActorEffectProfile::ReadOnly,
            other => return Err(ActorStartCaptureError::UnknownProfile(other)),
        };
        let facade = materialize_entry_facade(session, &entry, &explicit_exports)?;
        let lexical_scope = session.mint_isolated_scope();
        let descriptor = ActorDescriptor::new(
            label,
            crate::ActorPlacement {
                session: session_id,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_profile(profile)
        .with_source_imports(crate::ActorSourceImports::from_exact_facades([&facade]));
        Ok(Self {
            descriptor,
            parent_hole,
            entry,
            launch_worktrees,
        })
    }

    /// Consume the capture into the exact parent obligation and child entry.
    pub fn into_parts(self) -> (ActorDescriptor, ResidentHole, RootCustody, Vec<String>) {
        (
            self.descriptor,
            self.parent_hole,
            self.entry,
            self.launch_worktrees,
        )
    }
}

fn materialize_entry_facade<H, O>(
    session: &ResidentSession<H, O>,
    entry: &RootCustody,
    explicit_exports: &[String],
) -> Result<MaterializedFacade, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let heads = facade_heads(entry.provenance(), explicit_exports);
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

fn facade_heads(
    provenance: &tidepool_runtime::session::ProgramProvenance,
    explicit_exports: &[String],
) -> BTreeSet<String> {
    let mut heads: BTreeSet<_> = explicit_exports.iter().cloned().collect();
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
    use super::facade_heads;

    #[test]
    fn explicit_facade_heads_are_deduplicated_and_sorted() {
        let heads = facade_heads(
            &tidepool_runtime::session::ProgramProvenance::default(),
            &["Policy".into(), "helper".into(), "Policy".into()],
        );
        assert_eq!(heads.into_iter().collect::<Vec<_>>(), ["Policy", "helper"]);
    }
}
