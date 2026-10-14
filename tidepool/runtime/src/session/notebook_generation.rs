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
    pub fn next(self) -> Result<Self, NotebookGenerationError> {
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
#[serde(try_from = "EncodedGenerationArtifactReference")]
pub struct GenerationArtifactReference {
    bytes: Vec<u8>,
    sha256: [u8; 32],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedGenerationArtifactReference {
    bytes: Vec<u8>,
    sha256: [u8; 32],
}
impl TryFrom<EncodedGenerationArtifactReference> for GenerationArtifactReference {
    type Error = NotebookGenerationError;
    fn try_from(encoded: EncodedGenerationArtifactReference) -> Result<Self, Self::Error> {
        let value = Self {
            bytes: encoded.bytes,
            sha256: encoded.sha256,
        };
        value.validate()?;
        Ok(value)
    }
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
#[serde(try_from = "EncodedComposedPromptSnapshot")]
pub struct ComposedPromptSnapshot {
    instructions: String,
    sha256: [u8; 32],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedComposedPromptSnapshot {
    instructions: String,
    sha256: [u8; 32],
}
impl TryFrom<EncodedComposedPromptSnapshot> for ComposedPromptSnapshot {
    type Error = NotebookGenerationError;
    fn try_from(encoded: EncodedComposedPromptSnapshot) -> Result<Self, Self::Error> {
        let value = Self {
            instructions: encoded.instructions,
            sha256: encoded.sha256,
        };
        value.validate()?;
        Ok(value)
    }
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
pub(crate) struct PreparedGenerationSource {
    pub(crate) selection:
        std::sync::Arc<tidepool_toolchain::artifacts::PublishedSourceOriginalSelection>,
    pub(crate) context:
        std::sync::Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
}

/// Prepared under one session view; only commit can publish its issued owner.
pub struct PreparedNotebookGeneration {
    pub(crate) owner_stamp: super::OwnerStamp,
    pub(crate) domain: super::RecoveryDomainId,
    pub(crate) database_id: [u8; 16],
    pub(crate) staged: super::recovery::StagedRecoveryManifest,
    pub(crate) scope: ScopeId,
    pub(crate) epoch: u64,
    pub(crate) snapshot: super::PublicVisibilitySnapshot,
    pub(crate) next: ActiveNotebookGeneration,
    pub(crate) expected: Option<ActiveNotebookGeneration>,
    pub(crate) source: Option<PreparedGenerationSource>,
}
impl PreparedNotebookGeneration {
    pub fn selection(&self) -> ActiveNotebookGeneration {
        self.next.clone()
    }
    pub fn owner(&self) -> &RecoveryPublicOwner {
        &self.next.owner
    }
    pub fn generation(&self) -> NotebookGenerationId {
        self.next.generation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_ids_and_authenticated_artifacts_refuse_invalid_evidence() {
        assert_eq!(NotebookGenerationId::INITIAL.get(), 1);
        assert!(serde_json::from_str::<NotebookGenerationId>("0").is_err());
        let last: NotebookGenerationId = serde_json::from_str(&u64::MAX.to_string()).unwrap();
        assert_eq!(last.next(), Err(NotebookGenerationError::Exhausted));
        let prompt = ComposedPromptSnapshot::capture("λ\nexact instructions".into());
        let bytes = serde_json::to_vec(&prompt).unwrap();
        let decoded: ComposedPromptSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, prompt);
        decoded.validate().unwrap();
        let mut forged = serde_json::to_value(&prompt).unwrap();
        forged["instructions"] = serde_json::json!("changed");
        assert!(serde_json::from_value::<ComposedPromptSnapshot>(forged).is_err());
        let mut source = GenerationArtifactReference::capture(vec![0, 255, 7]);
        source.bytes.push(8);
        assert_eq!(
            source.validate(),
            Err(NotebookGenerationError::DigestMismatch)
        );
    }
}
