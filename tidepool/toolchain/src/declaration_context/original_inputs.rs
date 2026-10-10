//! Original content custody is independent of a receiving request's roles.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::CompileError;
use ciborium::value::Value;

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

/// Receiving aliases come from the retained writer. Protected origins come
/// only from authenticated acquisition; disposable producer scratch is absent.
pub(super) struct OriginalInputPart {
    pub kind: OriginalInputKind,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

pub(super) fn encode_image(
    producer: &str,
    unit: &str,
    module: &str,
    mut parts: Vec<OriginalInputPart>,
    origins: Option<&OwnedOriginalInputOrigins>,
) -> Result<Value, CompileError> {
    use super::{failure, hex, path_value, sha256, text};
    parts.sort_by(|a, b| (a.kind, &a.sha256).cmp(&(b.kind, &b.sha256)));
    if parts.is_empty()
        || parts
            .windows(2)
            .any(|pair| (pair[0].kind, &pair[0].sha256) == (pair[1].kind, &pair[1].sha256))
    {
        return Err(failure("ambiguous original input image parts"));
    }
    for part in &parts {
        if !part.path.is_absolute() || part.bytes == 0 || part.bytes > part.kind.byte_limit() {
            return Err(failure("invalid original input image part"));
        }
    }
    let identity = Value::Array(vec![
        text("TPORIGINALINPUT1"),
        text(producer),
        text(unit),
        text(module),
        Value::Array(
            parts
                .iter()
                .map(|part| {
                    Value::Array(vec![
                        text(part.kind.wire_tag()),
                        text(&part.sha256),
                        Value::Integer(part.bytes.into()),
                    ])
                })
                .collect(),
        ),
    ]);
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(&identity, &mut encoded).map_err(failure)?;
    Ok(Value::Array(vec![
        text(producer),
        text(unit),
        text(module),
        text(sha256(&encoded)),
        Value::Array(
            parts
                .into_iter()
                .map(|part| {
                    let protected = origins
                        .into_iter()
                        .flat_map(OwnedOriginalInputOrigins::origins)
                        .filter(|origin| {
                            origin.kind == part.kind
                                && hex(&origin.sha256) == part.sha256
                                && origin.bytes == part.bytes
                        })
                        .map(|origin| path_value(&origin.path))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Value::Array(vec![
                        text(part.kind.wire_tag()),
                        path_value(&part.path)?,
                        text(part.sha256),
                        Value::Integer(part.bytes.into()),
                        Value::Array(protected),
                    ]))
                })
                .collect::<Result<Vec<_>, CompileError>>()?,
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
        #[test]
        fn image_identity_tracks_content_and_owner_independently_of_receiving_paths(
            bytes in proptest::collection::vec(any::<u8>(),1..128),
            module in "[A-Z][A-Za-z]{0,12}",
            alias in "[a-z]{1,12}",
        ) {
            let seal = super::super::sha256(&bytes);
            let producer = "07".repeat(32);
            let part = |path: &str| OriginalInputPart {kind:OriginalInputKind::Interface,
                path:PathBuf::from(path),sha256:seal.clone(),bytes:bytes.len() as u64};
            let image = encode_image(&producer,"home",&module,vec![part("/original.hi")],None).unwrap();
            let relocated = encode_image(&producer,"home",&module,vec![part(&format!("/receiving-{alias}.hi"))],None).unwrap();
            let identity = |image: &Value| image.as_array().unwrap()[3].clone();
            prop_assert_eq!(identity(&image),identity(&relocated));
            prop_assert_ne!(&image.as_array().unwrap()[4],&relocated.as_array().unwrap()[4]);
            let other_owner = encode_image(&producer,"other-home",&module,vec![part("/original.hi")],None).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_owner));
            let other_producer = encode_image(&"08".repeat(32),"home",&module,vec![part("/original.hi")],None).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_producer));
            let mut changed = part("/original.hi");
            changed.bytes += 1;
            let other_length = encode_image(&producer,"home",&module,vec![changed],None).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_length));
            let mut changed = part("/original.hi");
            changed.sha256 = "ff".repeat(32);
            let other_content = encode_image(&producer,"home",&module,vec![changed],None).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_content));
            let mut changed = part("/original.hi");
            changed.kind = OriginalInputKind::Core;
            let other_kind = encode_image(&producer,"home",&module,vec![changed],None).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_kind));
        }
    }
}
