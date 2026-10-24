use tidepool_toolchain::certified_products::OriginalSourceAdmission;
fn needs_admission(_: &OriginalSourceAdmission) {}
fn reject(canonical_source_sha256: &[u8; 32]) {
    needs_admission(canonical_source_sha256);
}
fn main() {}
