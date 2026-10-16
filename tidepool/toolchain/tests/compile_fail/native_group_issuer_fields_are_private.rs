use tidepool_toolchain::artifact_inventory::{ArtifactId, ArtifactSelectionId, NativeGroupIssuer};
fn issuer(artifact: ArtifactId, selection: ArtifactSelectionId) -> NativeGroupIssuer {
    NativeGroupIssuer {
        artifact,
        selection,
    }
}
fn main() {}
