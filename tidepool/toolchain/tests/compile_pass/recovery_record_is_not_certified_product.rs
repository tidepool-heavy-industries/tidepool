use std::path::Path;
use tidepool_toolchain::recovery_artifacts::{
    materialize_certified_products, CertifiedRecoveryProduct,
};
fn accept(root: &Path, product: CertifiedRecoveryProduct) {
    let _ = materialize_certified_products(root, [1; 32], &[product]);
}
fn main() {}
