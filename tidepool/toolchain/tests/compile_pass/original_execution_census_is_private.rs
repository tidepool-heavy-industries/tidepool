use tidepool_toolchain::declaration_join::OriginalExecutionEvidence;
fn inspect(output: &OriginalExecutionEvidence) {
    let _ = output.missing_owners();
}
fn main() {}
