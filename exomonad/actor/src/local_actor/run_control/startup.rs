//! Owner-issued permission for an exact root startup journal admission.

use super::DirectoryError;
use crate::kernel::lifecycle::{LifecycleHandle, SuccessorDrainReceipt};
use crate::recovery::JournalBootRegistration;
use crate::{ActorRecoveryJournal, ActorRef, RootStartupIntent};
use std::sync::Arc;

/// Qualified initial replay prefix issued while consuming the host boot lease.
pub struct RootStartupRecoveryAuthority {
    journal: Arc<ActorRecoveryJournal>,
    boot: JournalBootRegistration,
}

impl RootStartupRecoveryAuthority {
    pub(super) fn from_boot(
        journal: Arc<ActorRecoveryJournal>,
        boot: JournalBootRegistration,
    ) -> Result<Self, DirectoryError> {
        if !boot.matches(&journal) {
            return Err(DirectoryError::Unavailable);
        }
        Ok(Self { journal, boot })
    }

    async fn verify(
        &self,
        predecessor: ActorRef,
        journal: &ActorRecoveryJournal,
    ) -> Result<(), DirectoryError> {
        if !self.boot.qualifies(predecessor, journal) {
            return Err(DirectoryError::Unauthorized { actor: predecessor });
        }
        let run = self.boot.run_authority();
        let root = self.boot.run_root().to_owned();
        let owns = tokio::task::spawn_blocking(move || run.owns_run(&root))
            .await
            .map_err(|_| DirectoryError::Unavailable)?
            .map_err(|error| {
                tracing::warn!(actor = %predecessor, kind = ?error.kind(), %error,
                    "root startup run authority could not be observed");
                DirectoryError::Unavailable
            })?;
        if !owns {
            return Err(DirectoryError::Unauthorized { actor: predecessor });
        }
        Ok(())
    }
}

pub(crate) enum StartupPredecessorAuthority {
    Initial,
    Live(LifecycleHandle),
    Recovered(Arc<RootStartupRecoveryAuthority>),
}

enum StartupPredecessorSettlement {
    Initial,
    Live(SuccessorDrainReceipt),
    Recovered(Arc<RootStartupRecoveryAuthority>),
}

/// Only the tracked root reservation producer obtains this permission.
/// It retains the exact intent and issuing journal until append acknowledgement.
pub(crate) struct RootStartupAdmission {
    actor: ActorRef,
    intent: RootStartupIntent,
    journal: Arc<ActorRecoveryJournal>,
    predecessor: StartupPredecessorSettlement,
}

impl RootStartupAdmission {
    pub(crate) fn validate_journal_admission(
        &self,
        actor: ActorRef,
        startup: &RootStartupIntent,
        journal: &ActorRecoveryJournal,
    ) -> Result<(), DirectoryError> {
        if actor != self.actor || startup != &self.intent || !self.journal.same_owner(journal) {
            return Err(DirectoryError::Unauthorized { actor });
        }
        match (&self.predecessor, startup.predecessor) {
            (StartupPredecessorSettlement::Initial, None) => Ok(()),
            (StartupPredecessorSettlement::Live(receipt), Some(predecessor))
                if receipt.predecessor() == predecessor
                    && receipt.confirms_resident_retirement() =>
            {
                Ok(())
            }
            (StartupPredecessorSettlement::Recovered(authority), Some(_))
                if authority.journal.same_owner(journal) =>
            {
                Ok(())
            }
            _ => Err(DirectoryError::Unauthorized { actor }),
        }
    }
}

/// Called by the tracked reservation producer, never by RunControl's handler.
/// Closing the predecessor stays processable while its actual work drains.
pub(super) async fn authorize(
    actor: ActorRef,
    intent: RootStartupIntent,
    journal: Arc<ActorRecoveryJournal>,
    predecessor: StartupPredecessorAuthority,
) -> Result<RootStartupAdmission, DirectoryError> {
    let unauthorized = || DirectoryError::Unauthorized { actor };
    let settlement = match (intent.predecessor, predecessor) {
        (None, StartupPredecessorAuthority::Initial) => StartupPredecessorSettlement::Initial,
        (Some(expected), StartupPredecessorAuthority::Live(owner))
            if owner.identity() == expected =>
        {
            let receipt = owner.retire_for_successor().await.map_err(|error| {
                tracing::warn!(actor = %actor, predecessor = %expected, %error,
                        "root startup predecessor did not confirm retirement");
                DirectoryError::Unavailable
            })?;
            if receipt.predecessor() != expected || !receipt.confirms_resident_retirement() {
                return Err(unauthorized());
            }
            StartupPredecessorSettlement::Live(receipt)
        }
        (Some(expected), StartupPredecessorAuthority::Recovered(authority)) => {
            authority.verify(expected, &journal).await?;
            StartupPredecessorSettlement::Recovered(authority)
        }
        _ => return Err(unauthorized()),
    };
    Ok(RootStartupAdmission {
        actor,
        intent,
        journal,
        predecessor: settlement,
    })
}
