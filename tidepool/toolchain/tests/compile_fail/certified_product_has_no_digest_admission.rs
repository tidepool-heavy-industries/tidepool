use tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct;
fn reject(product: CertifiedRecoveryProduct) {
    let _ = product.with_source_sha256([1; 32]);
}
fn main() {}
