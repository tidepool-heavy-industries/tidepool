//! Nonexecuting recovery graph fixtures, enabled only by a downstream dev-dependency.
//! Canonical encoding, native binding and materialization stay with their production
//! owners. These structural bytes do not claim GHC interface or Core semantics.

use super::*;
use ciborium::value::Value;
use tidepool_repr::execution_schema::ModuleVersion;

/// Materialize one empty native Lib product with its canonical interface companion.
/// An optional real package input lets tests exercise shared reads and later tamper
/// without editing or resealing any of the published artifact representations.
pub fn materialize_empty_native(
    root: &Path,
    external_package: Option<&Path>,
) -> Result<RecoveryArtifactRef, RecoveryArtifactError> {
    let producer = [0x11; 32];
    let bytes = b"interface".to_vec();
    let interface_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    let roots = external_package
        .map(|path| {
            let package_bytes = fs::read(path)?;
            Ok::<_, RecoveryArtifactError>(Value::Array(vec![
                Value::Text("base-unit".into()),
                Value::Text("Data.Base".into()),
                Value::Text(path.to_string_lossy().into_owned()),
                Value::Text(hex(&Sha256::digest(&package_bytes).into())),
            ]))
        })
        .transpose()?
        .into_iter()
        .collect();
    let packages = Value::Array(vec![
        Value::Text("TPPKGROOTS".into()),
        Value::Text("2".into()),
        Value::Array(vec![
            Value::Text("main".into()),
            Value::Text("Lib".into()),
            Value::Text(hex(&interface_sha256)),
        ]),
        Value::Array(roots),
        Value::Array(vec![]),
    ]);
    let mut package_bytes = Vec::new();
    ciborium::ser::into_writer(&packages, &mut package_bytes)
        .map_err(|_| RecoveryArtifactError::InvalidReference)?;
    let canonical = crate::certified_products::fixture_interface_bytes(
        producer,
        "main",
        "Lib",
        bytes.clone(),
        package_bytes.clone(),
    );
    let product = Value::Array(vec![
        Value::Text("TPMOD".into()),
        Value::Integer(1.into()),
        Value::Array(vec![Value::Array(vec![
            Value::Text("main".into()),
            Value::Text("Lib".into()),
            Value::Bytes(bytes.clone()),
            Value::Array(vec![]),
        ])]),
    ]);
    let mut product_bytes = Vec::new();
    ciborium::ser::into_writer(&product, &mut product_bytes)
        .map_err(|_| RecoveryArtifactError::InvalidReference)?;
    let owner = CachedHomeOwner {
        unit: "main".into(),
        module: "Lib".into(),
        module_version: ModuleVersion([0x22; 32]),
        skinny_iface_sha256: interface_sha256,
        product_sha256: Sha256::digest(&product_bytes).into(),
    };
    let certificate = crate::certified_products::encode_home_certification_with_module(
        &owner,
        &[],
        &BTreeMap::new(),
        canonical.requirements(),
        Sha256::digest(canonical.certificate_bytes()).into(),
    )
    .map_err(|_| RecoveryArtifactError::InvalidReference)?;
    let product = CertifiedRecoveryProduct::from_certification(
        owner,
        bytes,
        product_bytes,
        package_bytes,
        certificate,
    )
    .with_module_interface(canonical)?;
    materialize_certified_products(root, producer, &[product])?
        .into_iter()
        .next()
        .ok_or(RecoveryArtifactError::InvalidReference)
}
