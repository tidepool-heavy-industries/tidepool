//! Durable attachment belongs to the existing journal, Store and retained run
//! lease. This adapter carries their authority across the attachment barrier.

use super::HostIncarnationLease;

pub(super) struct EmbeddedStartupRecovery {
    pub(super) lease: Arc<HostIncarnationLease>,
    pub(super) journal: Arc<ActorRecoveryJournal>,
    pub(super) store: Arc<harness::store::Store>,
    pub(super) root_binding_path: PathBuf,
    pub(super) manifest: Option<(ActorRef, exomonad_actor::RootStartupManifestPin)>,
}
impl EmbeddedStartupRecovery {
    /// Observe each owner independently before admitting a successor. Runtime
    /// validates manifest contents; journal admission validates chain membership.
    pub(super) fn observe_manifest(
        &mut self,
        library: &tidepool_runtime::session::SessionLib,
        run_root: &Path,
        accepted_source: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.lease.owns_run(run_root)? {
            return Err("startup binding lacks its exact run lease".into());
        }
        let records = self.journal.validated_records()?;
        if let Some(head) = super::latest_durable_root_application(&records)? {
            let application = head
                .application
                .as_ref()
                .ok_or("root startup application is absent")?;
            if application.accepted_source.as_deref() != accepted_source
                || application.binding_path != self.root_binding_path
            {
                return Err("root startup changed its accepted source or binding path".into());
            }
        }
        let path = run_root.join("root-declarations.json");
        if !path.try_exists()? {
            if super::root_startup_chain(&records)?.iter().any(|record| {
                record
                    .startup
                    .as_ref()
                    .is_some_and(|intent| intent.manifest.is_some())
                    || record
                        .application
                        .as_ref()
                        .is_some_and(|application| application.conversation.is_some())
            }) {
                return Err("root startup lost its retained public manifest".into());
            }
            return Ok(());
        }
        for record in super::root_startup_chain(&records)? {
            let owner = tidepool_runtime::session::RecoveryPublicOwner::new(
                &super::root_declaration_recovery::root_path(),
                record.admission.actor.incarnation.0,
            )
            .ok_or("root startup owner has no incarnation")?;
            if library.validate_recovered_public_owner(&owner)? {
                self.manifest = Some((
                    record.admission.actor,
                    exomonad_actor::RootStartupManifestPin::capture_for_owner(&path, &owner)?,
                ));
                return Ok(());
            }
        }
        Err("root manifest owner is outside the exact startup chain".into())
    }

    pub(super) fn intent(
        &self,
        run_root: &Path,
        actor: ActorRef,
        accepted_source: Option<String>,
        bootstrap_identity: String,
    ) -> Result<exomonad_actor::RootStartupIntent, Box<dyn std::error::Error>> {
        let records = self.journal.validated_records()?;
        let head = super::latest_durable_root_application(&records)?;
        let mut store_predecessor = None;
        for record in super::root_startup_chain(&records)? {
            let identity = host_identity(run_root, "/root", record.admission.actor);
            if self.store.embedded_binding_matches(&identity)? {
                store_predecessor = Some(conversation(&identity));
                break;
            }
        }
        if store_predecessor.is_none()
            && self
                .store
                .agent(&harness::model::AgentPath("/root".into()))?
                .is_some()
        {
            return Err("root Store owner is outside the exact startup chain".into());
        }
        if store_predecessor.is_some() && self.manifest.is_none() {
            return Err("root Store binding has no retained public manifest".into());
        }
        Ok(exomonad_actor::RootStartupIntent {
            bootstrap_identity,
            predecessor: head.map(|record| record.admission.actor),
            manifest_predecessor: self.manifest.as_ref().map(|(actor, _)| *actor),
            manifest: self.manifest.as_ref().map(|(_, pin)| pin.clone()),
            store_predecessor,
            binding_path: self.root_binding_path.clone(),
            accepted_source,
            conversation: conversation(&host_identity(run_root, "/root", actor)),
        })
    }
}
use exomonad_actor::{
    ActorRecoveryJournal, ActorRef, ApplicationConversation, DurableRootSuccessorAdmission,
};
use harness::embedding::{BindingInitialAuthority, BindingSuccessorAuthority, HostIdentity};
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

/// Initial binding uses the actual admitted intent and retained journal under
/// the Store transaction. Readback alone cannot authorize this transition.
pub(super) struct EmbeddedBindingInitialAuthority {
    pub(super) lease: Arc<HostIncarnationLease>,
    pub(super) journal: Arc<ActorRecoveryJournal>,
    pub(super) run_root: PathBuf,
    pub(super) actor: ActorRef,
    pub(super) intent: exomonad_actor::RootStartupIntent,
}
impl BindingInitialAuthority for EmbeddedBindingInitialAuthority {
    fn validate_initial_binding(&self, identity: &HostIdentity) -> Result<bool, String> {
        if !self
            .lease
            .owns_run(&self.run_root)
            .map_err(|error| error.to_string())?
            || self.intent.store_predecessor.is_some()
            || self.intent.conversation != conversation(identity)
        {
            return Ok(false);
        }
        let records = self
            .journal
            .validated_records()
            .map_err(|error| error.to_string())?;
        let head =
            super::latest_durable_root_application(&records).map_err(|error| error.to_string())?;
        Ok(head.is_some_and(|record| {
            record.admission.actor == self.actor
                && record.terminal.is_none()
                && record.startup.as_ref() == Some(&self.intent)
                && record
                    .application
                    .as_ref()
                    .is_some_and(|application| application.conversation.is_none())
        }))
    }
}

pub(super) fn identity_from_conversation(
    value: &ApplicationConversation,
) -> Result<HostIdentity, String> {
    match value {
        ApplicationConversation::Embedded {
            run,
            agent_path,
            incarnation,
        } => Ok(HostIdentity {
            run: run.clone(),
            actor: harness::model::AgentPath(agent_path.clone()),
            incarnation: incarnation.clone(),
        }),
        _ => Err("root startup requires its exact Embedded conversation".into()),
    }
}
