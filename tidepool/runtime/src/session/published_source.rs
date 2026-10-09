//! Publication of compiler-issued frozen source originals at one actor scope.

use std::sync::Arc;

use tidepool_codegen::scope::ScopeId;
use tidepool_toolchain::artifacts::PublishedSourceOriginalSelection;
use tidepool_toolchain::declaration_join::ExactDeclarationContext;

use super::admission::RuntimeAdmissionOwner;
use super::{PersistentSession, PublicVisibilitySnapshot, SessionError};

/// A composed original selection awaiting the same installation's settlement.
/// Dropping this token changes neither the public compiler view nor its epoch.
pub struct PendingPublishedSourceOriginals {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    admitted: PublicVisibilitySnapshot,
    view_digest: [u8; 32],
    context: Arc<ExactDeclarationContext>,
    selection: Arc<PublishedSourceOriginalSelection>,
}

static_assertions::assert_not_impl_any!(PendingPublishedSourceOriginals: Clone, Copy);

impl PersistentSession {
    /// Compose before executing an installer. Only an authenticated original
    /// selection can enter this public namespace; conflicting owners refuse.
    pub fn stage_published_source_originals_in(
        &mut self,
        scope: ScopeId,
        selection: Arc<PublishedSourceOriginalSelection>,
    ) -> Result<PendingPublishedSourceOriginals, SessionError> {
        let admitted = self
            .public_visibility_snapshot_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let view_digest = self
            .compile_view_digest_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let base = self.lib().current_exact_context_in(scope);
        let context = match base {
            Some(base) => (*base).clone(),
            None => ExactDeclarationContext::new(&[], &[], Vec::new())?,
        }
        .with_published_source_originals(&selection)?;
        Ok(PendingPublishedSourceOriginals {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            admitted,
            view_digest,
            context: Arc::new(context),
            selection,
        })
    }

    /// Publish only after the installer has settled and before its dispatcher
    /// is exposed. A concurrent public write refuses this entire installation.
    pub fn publish_source_originals(
        &mut self,
        pending: PendingPublishedSourceOriginals,
    ) -> Result<(), SessionError> {
        let scope = pending.admitted.scope;
        if !Arc::ptr_eq(&pending.owner, self.admission_owner())
            || pending.owner_epoch != self.admission_owner().epoch()
            || self.public_visibility_snapshot_in(scope).as_ref() != Some(&pending.admitted)
            || self.compile_view_digest_in(scope) != Some(pending.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let epoch = self.prepare_public_visibility_advance(scope)?;
        let tip = pending.admitted.declaration_tip;
        self.lib_mut()
            .set_published_source_context_in(scope, tip, pending.context);
        self.lib_mut()
            .retain_published_source_selection_in(scope, pending.selection);
        self.commit_public_visibility_advance(scope, epoch);
        Ok(())
    }
}
