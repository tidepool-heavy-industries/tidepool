use std::sync::Arc;

use tidepool_toolchain::checked_cell::{AdmissionDigest, CellRecipe, CheckedCellRequest};

fn main() {
    let recipe = CellRecipe::new("cell", "template", Vec::new(), Vec::new()).unwrap();
    let request = CheckedCellRequest::issue(
        Arc::new(recipe),
        AdmissionDigest::finalize([7; 32]).unwrap(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(request.admission_digest(), [7; 32]);
    assert_eq!(request.recipe().cell_source(), "cell");
}
