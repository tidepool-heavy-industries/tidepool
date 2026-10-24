use tidepool_toolchain::certified_products::OriginalSourceAdmission;
use tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct;
fn needs_admission(_: &OriginalSourceAdmission) {}
fn observe(product: &CertifiedRecoveryProduct) {
    if let Some(admission) = product.source_admission() {
        needs_admission(admission);
    }
}
fn main() {}
