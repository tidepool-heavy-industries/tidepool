//! Source effect handler: reloading a run's own workspace Haskell source.
//!
//! The work itself — re-reading the declared source roots, checking the
//! affected module graph, and publishing a revision — belongs to whoever owns
//! the run's compiler and its frozen workspace, which is the composition root.
//! This handler is the boundary: it decodes the request, calls the installed
//! service, and renders the typed answer. It deliberately holds no paths and
//! no compiler of its own, so there is exactly one place that decides what a
//! run's source roots are.

use std::sync::Arc;

use tidepool_bridge_effects::{SrReloadOutcome, SrRevision, SrStatus};

// SourceReq, SourceError, DescribeEffect and the EffectHandler dispatch are
// GENERATED from the `tidepool-protocol` schema.
pub use crate::generated::source::{SourceError, SourceReq};

/// What a run must be able to do for `Source` to answer.
///
/// Implemented by the Exomonad composition root, which owns the frozen workspace,
/// every actor's live source layer, and the driver compile that decides
/// whether a candidate revision typechecks.
///
/// Every verb names the CALLER. A source layer is per checkout, so "which
/// layer" is not a property of the run: it is a property of the actor asking,
/// and the caller's principal is issued by the kernel rather than spelled by
/// the program. The service answers each actor about exactly the layer that
/// actor's own cells compile against.
pub trait SourceReloadService: Send + Sync {
    /// Capture `caller`'s own source roots, check the affected module graph,
    /// and publish it into `caller`'s own layer if — and only if — all of it
    /// compiles. `also_check` names additional modules to pull into the
    /// checked graph.
    ///
    /// A rejected candidate is an `Ok(SrReloadOutcome::ReloadRejected(..))`,
    /// not an `Err`: a failed typecheck is an expected result a program
    /// handles. `Err` is for a caller with no layer of its own to publish, or
    /// roots that could not be read.
    fn reload(
        &self,
        caller: tidepool_repr::PrincipalId,
        also_check: &[String],
    ) -> Result<SrReloadOutcome, SourceError>;

    /// The revision `caller`'s later cells compile against, and the revision
    /// its own source roots hold right now.
    fn status(&self, caller: tidepool_repr::PrincipalId) -> Result<SrStatus, SourceError>;
}

/// The `Source` effect's handler.
///
/// Absent a service — a run with no workspace, and every test stack that never
/// installs one — every verb answers `SourceUnavailable` rather than silently
/// succeeding against nothing.
pub struct SourceHandler {
    service: Option<Arc<dyn SourceReloadService>>,
}

impl SourceHandler {
    #[must_use]
    pub fn new(service: Arc<dyn SourceReloadService>) -> Self {
        Self {
            service: Some(service),
        }
    }

    /// A handler for a run with no workspace source to reload.
    #[must_use]
    pub fn unavailable() -> Self {
        Self { service: None }
    }

    fn service(&self) -> Result<&Arc<dyn SourceReloadService>, SourceError> {
        self.service.as_ref().ok_or_else(|| {
            SourceError::SourceUnavailable(
                "this run has no workspace source roots to reload".into(),
            )
        })
    }

    pub(crate) fn source_reload(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        also_check: Vec<String>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond(
            self.service()
                .and_then(|service| service.reload(cx.principal(), &also_check)),
        )
    }

    pub(crate) fn source_status(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond(
            self.service()
                .and_then(|service| service.status(cx.principal())),
        )
    }
}

/// The wire shape of one captured revision. One constructor, so every caller
/// spells a revision the same way.
#[must_use]
pub fn revision_to_wire(
    identity: &str,
    generation: u64,
    modules: &[(String, String)],
) -> SrRevision {
    SrRevision {
        identity: identity.to_owned(),
        generation: i64::try_from(generation).unwrap_or(i64::MAX),
        modules: modules
            .iter()
            .map(|(name, digest)| tidepool_bridge_effects::SrModule {
                name: name.clone(),
                digest: digest.clone(),
            })
            .collect(),
    }
}
