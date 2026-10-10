//! Original content custody is independent of a receiving request's roles.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::CompileError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum OriginalInputKind {
    Interface,
    Packages,
    Certificate,
    Core,
    Native,
    Census,
    Graph,
}

impl OriginalInputKind {
    pub(crate) fn wire_tag(self) -> &'static str {
        match self {
            Self::Interface => "iface",
            Self::Packages => "packages",
            Self::Certificate => "certificate",
            Self::Core => "core",
            Self::Native => "native",
            Self::Census => "census",
            Self::Graph => "graph",
        }
    }

    pub(crate) fn byte_limit(self) -> u64 {
        match self {
            Self::Packages | Self::Certificate => 4 * 1024 * 1024,
            Self::Graph | Self::Native => 64 * 1024 * 1024,
            _ => 32 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OriginalInputOrigin {
    pub kind: OriginalInputKind,
    pub path: PathBuf,
    pub sha256: [u8; 32],
    pub bytes: u64,
}

/// Issued from authenticated acquisition paths, never reconstructed from a
/// later inventory or a materialized filename. The payload owner keeps this
/// fact through detached selections; receiving offers select their own aliases.
#[derive(Clone, Debug)]
pub(crate) struct OwnedOriginalInputOrigins(Arc<[OriginalInputOrigin]>);

impl OwnedOriginalInputOrigins {
    pub(crate) fn from_authenticated_acquisition(
        origins: Vec<OriginalInputOrigin>,
    ) -> Result<Self, CompileError> {
        let mut seen = BTreeSet::new();
        for origin in &origins {
            if !origin.path.is_absolute()
                || origin.bytes == 0
                || origin.bytes > origin.kind.byte_limit()
                || !seen.insert((origin.kind, origin.path.clone()))
            {
                return Err(CompileError::ExtractFailed(
                    "invalid acquired original input provenance".into(),
                ));
            }
        }
        Ok(Self(origins.into()))
    }

    pub(crate) fn origins(&self) -> &[OriginalInputOrigin] {
        &self.0
    }
}
