// The removed turn.rs consumer mutation: relabel an issued request's digest.
use std::sync::Arc;

use tidepool_toolchain::checked_cell::{AdmissionDigest, CellRecipe, CheckedCellRequest};

fn main() {
    let recipe = CellRecipe::new("cell", "template", Vec::new(), Vec::new()).unwrap();
    let mut request = CheckedCellRequest::issue(
        Arc::new(recipe),
        AdmissionDigest::finalize([7; 32]).unwrap(),
        Vec::new(),
    )
    .unwrap();
    request.admission = AdmissionDigest::finalize([9; 32]).unwrap();
}
