//! Durable attachment belongs to the existing journal, Store and retained run
//! lease. This adapter carries their authority across the attachment barrier.

use super::HostIncarnationLease;

pub(super) struct EmbeddedStartupRecovery {
    pub(super) lease: Arc<HostIncarnationLease>,
    pub(super) journal: Arc<ActorRecoveryJournal>,
    pub(super) store: Arc<harness::store::Store>,
    pub(super) root_binding_path: PathBuf,
}
impl EmbeddedStartupRecovery {
    /// Finish only the latest prepared binding's missing journal row. The
    /// persisted owner is checked without allocating a scope or live actor.
    pub(super) fn reconcile(
        &self,
        library: &tidepool_runtime::session::SessionLib,
        run_root: &Path,
        accepted_source: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.lease.owns_run(run_root)? {
            return Err("startup binding lacks its exact run lease".into());
        }
        let records = self.journal.validated_records()?;
        let Some(latest) = super::latest_durable_root_application(&records)? else {
            return Ok(());
        };
        let application = latest
            .application
            .as_ref()
            .ok_or("latest root has no prepared application")?;
        if application.conversation.is_some() {
            return Ok(());
        }
        let Some(ApplicationConversation::Embedded {
            run,
            agent_path,
            incarnation,
        }) = &application.intended_conversation
        else {
            return Err("latest root application has no exact prepared Embedded intent".into());
        };
        if latest.terminal.is_some()
            || application.accepted_source.as_deref() != accepted_source
            || application.binding_path != self.root_binding_path
            || run != &super::runtime_namespace(run_root)
            || agent_path != "/root"
            || incarnation != &latest.admission.actor.incarnation.0.to_string()
        {
            return Err(
                "latest prepared Embedded root differs from startup source/application".into(),
            );
        }
        let owner_path = tidepool_repr::ActorPath::parse(
            latest
                .admission
                .actor_path
                .as_deref()
                .ok_or("latest root lacks canonical owner path")?,
        )?;
        let owner = tidepool_runtime::session::RecoveryPublicOwner::new(
            &owner_path,
            latest.admission.actor.incarnation.0,
        )
        .ok_or("latest root lacks persisted owner incarnation")?;
        if !library.validate_recovered_public_owner(&owner)? {
            return Err("latest prepared root does not own protected manifest".into());
        }
        let identity = HostIdentity {
            run: run.clone(),
            actor: harness::model::AgentPath(agent_path.clone()),
            incarnation: incarnation.clone(),
        };
        if !self.store.embedded_binding_matches(&identity)? {
            return Err("latest prepared root has no exact successor Store binding".into());
        }
        self.journal
            .bind_application_conversation(latest.admission.actor, conversation(&identity))?;
        Ok(())
    }
}
use exomonad_actor::{
    ActorRecoveryJournal, ActorRef, ApplicationConversation, DurableRootSuccessorAdmission,
};
use harness::embedding::{BindingSuccessorAuthority, HostIdentity};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn conversation(identity: &HostIdentity) -> ApplicationConversation {
    ApplicationConversation::Embedded {
        run: identity.run.clone(),
        agent_path: identity.actor.0.clone(),
        incarnation: identity.incarnation.clone(),
    }
}

pub(super) struct EmbeddedApplicationRecovery {
    pub(super) lease: Arc<HostIncarnationLease>,
    pub(super) journal: Arc<ActorRecoveryJournal>,
    pub(super) run_root: PathBuf,
    pub(super) root: ActorRef,
    pub(super) root_binding_path: PathBuf,
    pub(super) accepted_source: Option<String>,
    pub(super) recovered_root: bool,
}
impl EmbeddedApplicationRecovery {
    pub(super) fn prepare(&self, actor: ActorRef, identity: &HostIdentity) -> Result<(), String> {
        if !self
            .lease
            .owns_run(&self.run_root)
            .map_err(|error| error.to_string())?
            || identity.run != super::runtime_namespace(&self.run_root)
            || identity.incarnation != actor.incarnation.0.to_string()
        {
            return Err("embedded application does not belong to its retained run/actor".into());
        }
        let binding_path = if actor == self.root {
            self.root_binding_path.clone()
        } else {
            self.run_root
                .join(format!("{}-{}", actor.id.0, actor.incarnation.0))
                .join("binding.json")
        };
        self.journal
            .prepare_application_with_intent(
                actor,
                binding_path,
                self.accepted_source.clone(),
                Some(conversation(identity)),
            )
            .map_err(|error| error.to_string())
    }
    pub(super) fn bind(
        &self,
        actor: ActorRef,
        identity: &HostIdentity,
        store: &harness::store::Store,
    ) -> Result<(), String> {
        self.prepare(actor, identity)?;
        if !store
            .embedded_binding_matches(identity)
            .map_err(|error| error.to_string())?
        {
            return Err("embedded application has no exact durable Store binding".into());
        }
        self.journal
            .bind_application_conversation(actor, conversation(identity))
            .map_err(|error| error.to_string())
    }
}

pub(super) struct EmbeddedBindingSuccessorAuthority {
    pub(super) lease: Arc<HostIncarnationLease>,
    pub(super) run_root: PathBuf,
    pub(super) admission: Arc<DurableRootSuccessorAdmission>,
}
impl BindingSuccessorAuthority for EmbeddedBindingSuccessorAuthority {
    fn validate_successor(
        &self,
        predecessor: &HostIdentity,
        successor: &HostIdentity,
    ) -> Result<bool, String> {
        Ok(self
            .lease
            .owns_run(&self.run_root)
            .map_err(|error| error.to_string())?
            && predecessor.run == super::runtime_namespace(&self.run_root)
            && self
                .admission
                .validate_embedded_binding(
                    &self.run_root,
                    &conversation(predecessor),
                    &conversation(successor),
                )
                .map_err(|error| error.to_string())?)
    }
}

pub(super) fn host_identity(run_root: &Path, path: &str, actor: ActorRef) -> HostIdentity {
    HostIdentity {
        run: super::runtime_namespace(run_root),
        actor: harness::model::AgentPath(path.into()),
        incarnation: actor.incarnation.0.to_string(),
    }
}
