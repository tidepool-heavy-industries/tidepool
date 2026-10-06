//! Exact artifact-backed declaration recovery and its status report.

use super::SessionError;
use std::path::Path;

#[path = "newrecovery_v2.rs"]
mod newrecovery_v2;
pub(crate) use newrecovery_v2::*;
pub use newrecovery_v2::{RecoveryPublicOwner, RecoveryRefusal as RecoveryFormatRefusal};

pub(crate) fn graph_error(path: &Path, error: RecoveryError) -> SessionError {
    match &error.kind {
        RecoveryErrorKind::Format(refusal) => SessionError::RecoveryFormatRefused {
            path: path.to_path_buf(),
            refusal: *refusal,
        },
        RecoveryErrorKind::InventoryAccounting(cause) => SessionError::RecoveryInventoryRefused {
            path: path.to_path_buf(),
            cause: cause.clone(),
        },
        RecoveryErrorKind::Manifest => SessionError::RecoveryManifest {
            path: path.to_path_buf(),
            detail: error.to_string(),
        },
    }
}

/// Declaration evidence restored under its original module identity.
/// Native execution still requires each demanded original binding lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredDeclaration {
    pub generation: u64,
    pub module: String,
}

/// A durable binding winner whose native value belonged to the prior machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnavailableBinding {
    pub name: String,
    pub session: u64,
    pub variable: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationRecoveryReport {
    pub source_session: u64,
    pub successor_session: u64,
    pub restored: Vec<RestoredDeclaration>,
    pub unavailable_bindings: Vec<UnavailableBinding>,
    pub durability_unconfirmed: bool,
}
