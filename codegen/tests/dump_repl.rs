use core_repr::serial::read::read_cbor;
use core_repr::{CoreFrame, RecursiveTree};

#[test]
fn dump_letrec_structure() {
    let data = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../examples/tide/target/tidepool-cbor/Repl/repl.cbor"
    ))
    .unwrap();
    let expr = read_cbor(&data).unwrap();

    for (i, node) in expr.nodes.iter().enumerate() {
        if let CoreFrame::LetRec { bindings, body: _ } = node {
            eprintln!("LetRec at node {} with {} bindings:", i, bindings.len());
            for (binder, rhs_idx) in bindings {
                let kind = match &expr.nodes[*rhs_idx] {
                    CoreFrame::Lam { .. } => "Lam",
                    CoreFrame::Con { tag, fields } => {
                        &format!("Con(tag={}, {} fields)", tag, fields.len())
                    }
                    CoreFrame::App { .. } => "App",
                    CoreFrame::Case { .. } => "Case",
                    CoreFrame::Var(_) => "Var",
                    other => &format!("{:?}", std::mem::discriminant(other)),
                };
                eprintln!("  {:?} = {} (node {})", binder, kind, rhs_idx);
            }
        }
    }
}
