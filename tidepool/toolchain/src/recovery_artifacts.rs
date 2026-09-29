//! Run-owned retention of exact compiler artifacts. The compile cache can be
//! regenerated; recovery manifests refer only to this fsynced closure.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::CachedHomeOwner;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryArtifactRef {
    pub toolchain_identity_sha256: [u8; 32],
    pub unit: String,
    pub module: String,
    pub module_version: [u8; 32],
    pub skinny_iface_sha256: [u8; 32],
    pub product_sha256: [u8; 32],
    pub interface_path: PathBuf,
    pub product_path: PathBuf,
}

pub struct RecoveryArtifactInput<'a> {
    pub owner: &'a CachedHomeOwner,
    pub interface_source: &'a Path,
    pub product_source: &'a Path,
}

#[derive(Debug)]
pub struct VerifiedRecoveryArtifact {
    pub reference: RecoveryArtifactRef,
    pub interface_path: PathBuf,
    pub product_path: PathBuf,
    pub interface_bytes: Vec<u8>,
    pub product_bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryArtifactError {
    #[error("invalid recovery artifact reference")]
    InvalidReference,
    #[error("recovery artifact unavailable: {0}")]
    Unavailable(PathBuf),
    #[error("recovery artifact checksum mismatch: {0}")]
    DigestMismatch(PathBuf),
    #[error("recovery artifact I/O: {0}")]
    Io(#[from] io::Error),
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn checked_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn read_checked(path: &Path, expected: &[u8; 32]) -> Result<Vec<u8>, RecoveryArtifactError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RecoveryArtifactError::Unavailable(path.to_path_buf()));
        }
        Err(error) => return Err(error.into()),
    };
    if Sha256::digest(&bytes).as_slice() != expected {
        return Err(RecoveryArtifactError::DigestMismatch(path.to_path_buf()));
    }
    Ok(bytes)
}

fn durable_copy(path: &Path, bytes: &[u8], digest: &[u8; 32]) -> Result<(), RecoveryArtifactError> {
    if path.exists() && read_checked(path, digest).is_ok() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or(RecoveryArtifactError::InvalidReference)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    read_checked(path, digest)?;
    Ok(())
}

/// Materialize complete checksum-verified pairs before a recovery manifest is
/// staged. Paths in the returned refs are relative to the supplied run root.
pub fn materialize_recovery_closure(
    recovery_root: &Path,
    toolchain_identity_sha256: [u8; 32],
    artifacts: &[RecoveryArtifactInput<'_>],
) -> Result<Vec<RecoveryArtifactRef>, RecoveryArtifactError> {
    if toolchain_identity_sha256 == [0; 32] {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let owned = recovery_root.join("artifacts");
    fs::create_dir_all(&owned)?;
    File::open(recovery_root)?.sync_all()?;
    let mut refs = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let owner = artifact.owner;
        if owner.unit.is_empty() || owner.module.is_empty() {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        let interface = read_checked(artifact.interface_source, &owner.skinny_iface_sha256)?;
        let product = read_checked(artifact.product_source, &owner.product_sha256)?;
        let interface_path =
            PathBuf::from("artifacts").join(format!("{}.hi", hex(&owner.skinny_iface_sha256)));
        let product_path = PathBuf::from("artifacts")
            .join(format!("{}.products.cbor", hex(&owner.product_sha256)));
        durable_copy(
            &recovery_root.join(&interface_path),
            &interface,
            &owner.skinny_iface_sha256,
        )?;
        durable_copy(
            &recovery_root.join(&product_path),
            &product,
            &owner.product_sha256,
        )?;
        refs.push(RecoveryArtifactRef {
            toolchain_identity_sha256,
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: owner.module_version.0,
            skinny_iface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
            interface_path,
            product_path,
        });
    }
    File::open(&owned)?.sync_all()?;
    Ok(refs)
}

/// Verify path confinement and both immutable bytes before using a durable
/// recovery ref. The captured bytes let a caller avoid a second unbound read.
pub fn verify_materialized_ref(
    recovery_root: &Path,
    reference: &RecoveryArtifactRef,
) -> Result<VerifiedRecoveryArtifact, RecoveryArtifactError> {
    if reference.toolchain_identity_sha256 == [0; 32]
        || reference.unit.is_empty()
        || reference.module.is_empty()
        || !checked_relative(&reference.interface_path)
        || !checked_relative(&reference.product_path)
    {
        return Err(RecoveryArtifactError::InvalidReference);
    }
    let canonical_root = fs::canonicalize(recovery_root)?;
    let resolve = |relative: &Path| -> Result<PathBuf, RecoveryArtifactError> {
        let candidate = recovery_root.join(relative);
        let canonical = match fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(RecoveryArtifactError::Unavailable(candidate));
            }
            Err(error) => return Err(error.into()),
        };
        if !canonical.starts_with(&canonical_root) {
            return Err(RecoveryArtifactError::InvalidReference);
        }
        Ok(canonical)
    };
    let interface_path = resolve(&reference.interface_path)?;
    let product_path = resolve(&reference.product_path)?;
    let interface_bytes = read_checked(&interface_path, &reference.skinny_iface_sha256)?;
    let product_bytes = read_checked(&product_path, &reference.product_sha256)?;
    Ok(VerifiedRecoveryArtifact {
        reference: reference.clone(),
        interface_path,
        product_path,
        interface_bytes,
        product_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::ModuleVersion;

    #[test]
    fn materialized_pair_is_confined_and_checksum_verified() {
        let run = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("A.hi");
        let product = source.path().join("products.cbor");
        fs::write(&iface, b"iface").unwrap();
        fs::write(&product, b"product").unwrap();
        let owner = CachedHomeOwner {
            unit: "home".into(),
            module: "A".into(),
            module_version: ModuleVersion([3; 32]),
            skinny_iface_sha256: Sha256::digest(b"iface").into(),
            product_sha256: Sha256::digest(b"product").into(),
        };
        let refs = materialize_recovery_closure(
            run.path(),
            [1; 32],
            &[RecoveryArtifactInput {
                owner: &owner,
                interface_source: &iface,
                product_source: &product,
            }],
        )
        .unwrap();
        let verified = verify_materialized_ref(run.path(), &refs[0]).unwrap();
        assert_eq!(verified.interface_bytes, b"iface");
        let mut escaped = refs[0].clone();
        escaped.product_path = PathBuf::from("../outside");
        assert!(matches!(
            verify_materialized_ref(run.path(), &escaped),
            Err(RecoveryArtifactError::InvalidReference)
        ));
        fs::write(&verified.product_path, b"tampered").unwrap();
        assert!(matches!(
            verify_materialized_ref(run.path(), &refs[0]),
            Err(RecoveryArtifactError::DigestMismatch(_))
        ));
    }
}
