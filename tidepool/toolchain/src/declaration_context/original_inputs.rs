//! Original content custody is independent of a receiving request's roles.

use std::collections::{BTreeMap, BTreeSet};
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
pub(crate) struct OriginalInputPart {
    pub kind: OriginalInputKind,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub transport: crate::owned_input_arena::OwnedInputSlice,
}

#[derive(Default)]
pub(crate) struct OriginalInputArenaTable(BTreeMap<PathBuf, (usize, u64)>);

impl OriginalInputArenaTable {
    fn location(&mut self, slice: &crate::owned_input_arena::OwnedInputSlice) -> Result<Value, CompileError> {
        let next = self.0.len();
        if next >= 4096 { return Err(super::failure("original input arena table exceeds bound")); }
        let (index, extent) = self.0.entry(slice.endpoint().to_owned()).or_insert((next, slice.arena_len()));
        if *extent != slice.arena_len() { return Err(super::failure("conflicting original input arena extent")); }
        Ok(Value::Array(vec![Value::Integer((*index as u64).into()), Value::Integer(slice.offset().into())]))
    }

    pub(crate) fn acquisition(self, images: Vec<Value>) -> Result<Value, CompileError> {
        let mut descriptors = vec![Value::Null; self.0.len()];
        for (endpoint, (index, extent)) in self.0 {
            descriptors[index] = Value::Array(vec![super::path_value(&endpoint)?, Value::Integer(extent.into())]);
        }
        Ok(Value::Array(vec![super::text("continue-originals"), Value::Array(descriptors), Value::Array(images)]))
    }
}

pub(crate) fn entry_payloads(entry: &crate::artifact_inventory::ArtifactEntry) -> Vec<(OriginalInputKind, &[u8])> {
    use crate::artifact_inventory::ArtifactPayload;
    let (interface, packages, canonical) = match &entry.payload {
        ArtifactPayload::Original(product) => (product.interface_bytes(), product.package_imports_bytes(), product.module_interface()),
        ArtifactPayload::Canonical(interface) => (interface.interface_bytes(), interface.package_imports_bytes(), Some(interface)),
        ArtifactPayload::Interface(interface, _) => (interface.interface_bytes(), interface.package_imports_bytes(), None),
    };
    let mut parts = vec![(OriginalInputKind::Interface, interface), (OriginalInputKind::Packages, packages)];
    if let Some(canonical) = canonical {
        parts.push((OriginalInputKind::Certificate, canonical.certificate_bytes()));
        if let Some(core) = canonical.core_bytes() { parts.push((OriginalInputKind::Core, core)); }
    }
    if let ArtifactPayload::Original(product) = &entry.payload {
        parts.push((OriginalInputKind::Native, product.product_bytes()));
        parts.push((OriginalInputKind::Census, product.certification_bytes()));
    }
    parts
}

pub(crate) fn encode_image(
    producer: &str,
    unit: &str,
    module: &str,
    mut parts: Vec<OriginalInputPart>,
    origins: Option<&OwnedOriginalInputOrigins>,
    arenas: &mut OriginalInputArenaTable,
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
        if !part.path.is_absolute() || part.bytes == 0 || part.bytes > part.kind.byte_limit()
            || part.bytes != part.transport.len() || part.sha256 != hex(&part.transport.sha256()) {
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
                        arenas.location(&part.transport)?,
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
            let mut builder = crate::owned_input_arena::OwnedInputArenaBuilder::new(1024).unwrap();
            let pending = builder.append(&bytes).unwrap();
            let arena = builder.finish().unwrap();
            let transport = arena.issue_slice(pending).unwrap();
            let mut arenas = OriginalInputArenaTable::default();
            let part = |path: &str| OriginalInputPart {kind:OriginalInputKind::Interface,
                path:PathBuf::from(path),sha256:seal.clone(),bytes:bytes.len() as u64,transport:transport.clone()};
            let image = encode_image(&producer,"home",&module,vec![part("/original.hi")],None,&mut arenas).unwrap();
            let relocated = encode_image(&producer,"home",&module,vec![part(&format!("/receiving-{alias}.hi"))],None,&mut arenas).unwrap();
            let identity = |image: &Value| image.as_array().unwrap()[3].clone();
            prop_assert_eq!(identity(&image),identity(&relocated));
            prop_assert_ne!(&image.as_array().unwrap()[4],&relocated.as_array().unwrap()[4]);
            let other_owner = encode_image(&producer,"other-home",&module,vec![part("/original.hi")],None,&mut arenas).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_owner));
            let other_producer = encode_image(&"08".repeat(32),"home",&module,vec![part("/original.hi")],None,&mut arenas).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_producer));
            let mut changed = part("/original.hi");
            changed.bytes += 1;
            prop_assert!(encode_image(&producer,"home",&module,vec![changed],None,&mut arenas).is_err());
            let mut changed = part("/original.hi");
            changed.sha256 = "ff".repeat(32);
            prop_assert!(encode_image(&producer,"home",&module,vec![changed],None,&mut arenas).is_err());
            let mut changed = part("/original.hi");
            changed.kind = OriginalInputKind::Core;
            let other_kind = encode_image(&producer,"home",&module,vec![changed],None,&mut arenas).unwrap();
            prop_assert_ne!(identity(&image),identity(&other_kind));
        }
    }
}
