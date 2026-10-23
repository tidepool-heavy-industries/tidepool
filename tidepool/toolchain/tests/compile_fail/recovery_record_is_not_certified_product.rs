use std::path::Path;
use tidepool_toolchain::recovery_artifacts::{materialize_certified_products, RecoveryArtifactRef};
fn reject(root: &Path, record: RecoveryArtifactRef) {
    let _ = materialize_certified_products(root, [1; 32], &[record]);
}
fn main() {}
