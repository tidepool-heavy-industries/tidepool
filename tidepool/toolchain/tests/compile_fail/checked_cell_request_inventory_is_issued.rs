// A consumer cannot relabel the original declaration-owner inventory either.
use std::sync::Arc;

use tidepool_toolchain::checked_cell::{AdmissionDigest, CellRecipe, CheckedCellRequest};

fn main() {
    let recipe = CellRecipe::new("cell", "template", Vec::new(), Vec::new()).unwrap();
    let mut request = CheckedCellRequest::issue(
        Arc::new(recipe),
        AdmissionDigest::finalize([7; 32]).unwrap(),
        vec!["Tidepool.Session.Val.G1".to_string()],
    )
    .unwrap();
    request.reserved_declaration_modules = Arc::from(["Tidepool.Session.Val.G2".to_string()]);
}
