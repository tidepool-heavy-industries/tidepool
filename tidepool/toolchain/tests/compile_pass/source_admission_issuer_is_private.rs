use tidepool_toolchain::certified_products::OriginalSourceAdmission;
use tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct;
fn observe(product: &CertifiedRecoveryProduct) -> Option<&OriginalSourceAdmission> {
    product.source_admission()
}
fn main() {}
