//! Durable notebook generations share the run recovery transaction owner.
use super::{RecoveryPublicOwner, ScopeId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NotebookGenerationId(NonZeroU64);
impl NotebookGenerationId {
    pub const INITIAL: Self = Self(NonZeroU64::new(1).unwrap());
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub(crate) fn next(self) -> Result<Self, NotebookGenerationError> {
        self.get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or(NotebookGenerationError::Exhausted)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NotebookGenerationError {
    #[error("notebook generation counter exhausted")]
    Exhausted,
    #[error("generation artifact digest differs from its exact bytes")]
    DigestMismatch,
}
/// Issuers serialize their complete typed identity; recovery revalidates that
/// identity through its source/installer owner before constructing live leases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationArtifactReference {
    bytes: Vec<u8>,
    sha256: [u8; 32],
}
impl GenerationArtifactReference {
    pub fn capture(bytes: Vec<u8>) -> Self {
        Self {
            sha256: Sha256::digest(&bytes).into(),
            bytes,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn validate(&self) -> Result<(), NotebookGenerationError> {
        if <[u8; 32]>::from(Sha256::digest(&self.bytes)) == self.sha256 {
            Ok(())
        } else {
            Err(NotebookGenerationError::DigestMismatch)
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposedPromptSnapshot {
    instructions: String,
    sha256: [u8; 32],
}
impl ComposedPromptSnapshot {
    pub fn capture(instructions: String) -> Self {
        Self {
            sha256: Sha256::digest(instructions.as_bytes()).into(),
            instructions,
        }
    }
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn validate(&self) -> Result<(), NotebookGenerationError> {
        if <[u8; 32]>::from(Sha256::digest(self.instructions.as_bytes())) == self.sha256 {
            Ok(())
        } else {
            Err(NotebookGenerationError::DigestMismatch)
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableGenerationReferences {
    pub source: GenerationArtifactReference,
    pub installation: GenerationArtifactReference,
    pub prompts: ComposedPromptSnapshot,
}
impl DurableGenerationReferences {
    pub fn validate(&self) -> Result<(), NotebookGenerationError> {
        self.source.validate()?;
        self.installation.validate()?;
        self.prompts.validate()
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveNotebookGeneration {
    pub owner: RecoveryPublicOwner,
    pub references: DurableGenerationReferences,
}
impl ActiveNotebookGeneration {
    pub fn generation(&self) -> NotebookGenerationId {
        self.owner.notebook_generation()
    }
}
/// Prepared under one session view; only commit can publish its issued owner.
pub struct PreparedNotebookGeneration {
    pub(crate) staged: super::recovery::StagedRecoveryManifest,
    pub(crate) scope: ScopeId,
    pub(crate) epoch: u64,
    pub(crate) snapshot: super::PublicVisibilitySnapshot,
    pub(crate) next: ActiveNotebookGeneration,
    pub(crate) expected: Option<ActiveNotebookGeneration>,
}
impl PreparedNotebookGeneration {
    pub fn owner(&self) -> &RecoveryPublicOwner {
        &self.next.owner
    }
    pub fn generation(&self) -> NotebookGenerationId {
        self.next.generation()
    }
}
