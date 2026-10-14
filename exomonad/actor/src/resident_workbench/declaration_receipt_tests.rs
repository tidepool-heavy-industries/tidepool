use super::declaration_receipt;

#[test]
fn import_and_pragma_only_cells_have_truthful_receipts() {
    assert_eq!(
        declaration_receipt(&[], true, 7),
        "accepted cell prologue at generation 7"
    );
    assert_eq!(
        declaration_receipt(&[], false, 7),
        "committed declaration at generation 7"
    );
    assert_eq!(
        declaration_receipt(&["watchdogProbe".to_owned()], false, 7),
        "defined watchdogProbe at generation 7"
    );
}
