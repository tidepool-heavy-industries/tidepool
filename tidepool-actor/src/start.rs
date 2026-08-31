//! Capture of one public Haskell `startActor` suspension.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::sync::Arc;

use tidepool_bridge::{BridgeError, FromCore};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{OutputSink, ResidentHole, ResidentSession, RootCustody};
use tidepool_runtime::YieldSite;

use crate::generated::actor::ActorReq;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorStartRequest {
    pub label: String,
}

/// One parked parent continuation paired with exclusive custody of its child
/// entry and the GHC metadata belonging to that compiled entry.
pub struct ResidentActorStart {
    request: ActorStartRequest,
    parent_hole: ResidentHole,
    entry: RootCustody,
    yield_sites: Arc<[YieldSite]>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorStartCaptureError {
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("actor start decoder received a non-start request")]
    UnexpectedRequest,
    #[error("actor start suspended without its child entry live payload")]
    MissingEntry,
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
        yield_sites: Vec<YieldSite>,
        child_realm: RealmId,
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorReq::ActorStartWith(label, _entry_projection) =
            ActorReq::from_value(request, table)?
        else {
            return Err(ActorStartCaptureError::UnexpectedRequest);
        };
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        Ok(Self {
            request: ActorStartRequest { label },
            parent_hole,
            entry,
            yield_sites: yield_sites.into(),
        })
    }

    #[must_use]
    pub fn request(&self) -> &ActorStartRequest {
        &self.request
    }

    /// Consume the capture into the exact parent obligation, child entry, and
    /// compiler-owned typed-yield metadata required by startup orchestration.
    pub fn into_parts(self) -> (ResidentHole, RootCustody, Arc<[YieldSite]>) {
        (self.parent_hole, self.entry, self.yield_sites)
    }
}
