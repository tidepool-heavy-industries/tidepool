//! Original source paths selected together by native catalog production.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    absolute, io, reject_source_aliases, require_immutable_roots, ModulePackageError, RootPolicy,
};

/// The ordered native runtime source roles. Order determines import shadowing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSourceRole {
    StableEffects,
    Stdlib,
    Actors,
    Jev,
}

impl NativeSourceRole {
    pub const ORDERED: [Self; 4] = [Self::StableEffects, Self::Stdlib, Self::Actors, Self::Jev];

    pub fn relative_root(self) -> &'static str {
        match self {
            Self::StableEffects => "effects",
            Self::Stdlib => "lib",
            Self::Actors => "actors",
            Self::Jev => "jev/core",
        }
    }
}

/// Serialized source provenance; admission remains the catalog owner's job.
/// This value does not establish Nix registration or retention.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCatalogSourceSelection {
    pub snapshot_root: PathBuf,
    pub roles: [NativeSourceRole; 4],
    pub source_files: Vec<(PathBuf, String)>,
}

impl NativeCatalogSourceSelection {
    pub(crate) fn capture(snapshot_root: &Path) -> Result<Self, ModulePackageError> {
        Self::capture_under(snapshot_root, RootPolicy::NixStore)
    }

    pub(super) fn capture_under(
        snapshot_root: &Path,
        policy: RootPolicy,
    ) -> Result<Self, ModulePackageError> {
        require_immutable_roots(policy, snapshot_root)?;
        if !snapshot_root.is_dir() || absolute(snapshot_root).as_deref() != Some(snapshot_root) {
            return Err(ModulePackageError::RootMoved);
        }
        reject_source_aliases(snapshot_root)?;
        for role in NativeSourceRole::ORDERED {
            let root = snapshot_root.join(role.relative_root());
            if !root.is_dir() || absolute(&root).as_ref() != Some(&root) {
                return Err(ModulePackageError::Format("native source role directory"));
            }
        }
        if !snapshot_root.join("TidepoolCatalog.hs").is_file() {
            return Err(ModulePackageError::Format("native catalog probe"));
        }
        Ok(Self {
            snapshot_root: snapshot_root.to_owned(),
            roles: NativeSourceRole::ORDERED,
            source_files: crate::cache::source_root_manifest(snapshot_root)
                .map_err(|error| io(&error.path, error.source))?,
        })
    }

    pub(super) fn validate_under(&self, policy: RootPolicy) -> Result<(), ModulePackageError> {
        if self.roles != NativeSourceRole::ORDERED {
            return Err(ModulePackageError::Format("native source role order"));
        }
        let current = Self::capture_under(&self.snapshot_root, policy)?;
        if current.source_files != self.source_files {
            return Err(ModulePackageError::SourceChanged);
        }
        Ok(())
    }

    pub fn root(&self, role: NativeSourceRole) -> PathBuf {
        self.snapshot_root.join(role.relative_root())
    }

    pub fn include_roots(&self) -> Vec<PathBuf> {
        self.roles.iter().map(|role| self.root(*role)).collect()
    }

    pub fn contains_source(&self, path: &Path) -> bool {
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            && self
                .include_roots()
                .iter()
                .any(|root| path.starts_with(root))
    }
}
