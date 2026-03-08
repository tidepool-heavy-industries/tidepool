#[test]
#[ignore] // Debug utility — requires manual setup of /tmp/test_mcp_context_cbor/
fn inspect_letrec_map() {
    let bytes = std::fs::read("/tmp/test_mcp_context_cbor/testResult.cbor").unwrap();
    let expr = tidepool_repr::serial::read_cbor(&bytes).unwrap();
    eprintln!("\n=== Tree nodes ({} total) ===", expr.nodes.len());
    for (i, node) in expr.nodes.iter().enumerate() {
        eprintln!("[{:3}] {:?}", i, node);
    }
    eprintln!("\nRoot: {}", expr.nodes.len() - 1);
    eprintln!("\n=== Pretty ===");
    eprintln!("{}", tidepool_repr::pretty::pretty_print(&expr));
}

#[test]
#[ignore]
fn inspect_repro_fail() {
    let bytes = std::fs::read("/tmp/repro_fail_cbor/result.cbor").unwrap();
    let expr = tidepool_repr::serial::read_cbor(&bytes).unwrap();
    eprintln!("\n=== Tree nodes ({} total) ===", expr.nodes.len());
    for (i, node) in expr.nodes.iter().enumerate() {
        eprintln!("[{:3}] {:?}", i, node);
    }
    eprintln!("\nRoot: {}", expr.nodes.len() - 1);
    eprintln!("\n=== Pretty ===");
    eprintln!("{}", tidepool_repr::pretty::pretty_print(&expr));
}
