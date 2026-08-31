//! Exact-source promotion of one live Haskell actor definition.

use std::collections::BTreeSet;

use tidepool_bridge::{FromCore, ToCore};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentHole, ResidentOutcome, ResidentSession,
};

use crate::generated::actor::ActorReq;

#[derive(Debug, thiserror::Error)]
pub enum ActorPromotionError {
    #[error("actor promotion decoder received a non-promotion request")]
    UnexpectedRequest,
    #[error(transparent)]
    Decode(#[from] tidepool_bridge::BridgeError),
    #[error("actor promotion suspended without compiler provenance")]
    MissingProvenance,
    #[error(transparent)]
    ExactExports(#[from] tidepool_runtime::session::ExactExportError),
    #[error(transparent)]
    Facade(#[from] tidepool_runtime::session::ExactFacadeError),
    #[error("actor promotion has no live declaration plane")]
    NoCompileView,
    #[error(transparent)]
    Resident(#[from] tidepool_runtime::session::ResidentError),
}

/// Seal and resume the private promotion step inside one `startActor` call
/// while the resident machine is checked out. The returned facade is the
/// complete receipt: it is content-addressed and owns no live roots or
/// registry entry.
pub fn promote_checked_out<H, O>(
    session: &mut ResidentSession<H, O>,
    hole: ResidentHole,
    request: &Value,
) -> Result<(ResidentOutcome, MaterializedFacade), ActorPromotionError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let ActorReq::ActorPromoteWith = ActorReq::from_value(request, session.data_con_table())?
    else {
        return Err(ActorPromotionError::UnexpectedRequest);
    };
    let provenance = session
        .parked_program_provenance(&hole)
        .ok_or(ActorPromotionError::MissingProvenance)?;
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
    let scope = session.run_context().lexical_scope;
    let names: Vec<_> = heads.iter().map(String::as_str).collect();
    let surface = session.exact_exports_in(scope, &names)?;
    let view = session
        .compile_view_in(scope)
        .ok_or(ActorPromotionError::NoCompileView)?;
    let facade = surface.materialize(&view)?;
    let receipt = facade.identity().digest().to_string();
    let answer = receipt.to_value(session.data_con_table())?;
    let outcome = session.resume(hole, answer)?;
    Ok((outcome, facade))
}
