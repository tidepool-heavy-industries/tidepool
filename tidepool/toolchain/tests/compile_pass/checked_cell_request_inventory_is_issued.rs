use std::sync::Arc;

use tidepool_toolchain::checked_cell::{AdmissionDigest, CellRecipe, CheckedCellRequest};

fn main() {
    let recipe = CellRecipe::new("cell", "template", Vec::new(), Vec::new()).unwrap();
    let request = CheckedCellRequest::issue(
        Arc::new(recipe),
        AdmissionDigest::finalize([7; 32]).unwrap(),
        vec!["Tidepool.Session.Val.G1".to_string()],
    )
    .unwrap();
    assert_eq!(
        request.reserved_declaration_modules(),
        ["Tidepool.Session.Val.G1"]
    );
}
