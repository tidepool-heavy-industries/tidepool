use tidepool_toolchain::artifact_inventory::{ArtifactId, ArtifactSelectionId, NativeGroupIssuer};
fn identity(issuer: NativeGroupIssuer) -> (ArtifactId, ArtifactSelectionId) {
    (issuer.artifact(), issuer.selection())
}
fn main() {}
