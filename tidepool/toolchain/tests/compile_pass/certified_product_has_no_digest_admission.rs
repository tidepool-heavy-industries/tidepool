use tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct;
fn observe(product: &CertifiedRecoveryProduct) -> Option<[u8; 32]> {
    product.source_sha256()
}
fn main() {}
