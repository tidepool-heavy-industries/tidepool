use proptest::prelude::*;
use tidepool_repr::*;
use tidepool_repr::serial::{read_cbor, write_cbor, read_metadata, write_metadata};
use tidepool_testing::gen::arb_core_expr;

/// Round-trip property for CoreExpr: from_cbor(to_cbor(expr)) == expr
proptest! {
    #[test]
    fn cbor_round_trip(expr in arb_core_expr()) {
        let bytes = write_cbor(&expr).expect("write_cbor failed");
        let recovered = read_cbor(&bytes).expect("read_cbor failed");
        prop_assert_eq!(expr, recovered);
    }
}

/// CBOR serialization is deterministic: to_cbor(expr) == to_cbor(expr)
proptest! {
    #[test]
    fn cbor_deterministic(expr in arb_core_expr()) {
        let bytes1 = write_cbor(&expr).expect("write_cbor failed (1)");
        let bytes2 = write_cbor(&expr).expect("write_cbor failed (2)");
        prop_assert_eq!(bytes1, bytes2);
    }
}

/// Serialized form is never empty for any non-trivial expr
proptest! {
    #[test]
    fn cbor_non_empty(expr in arb_core_expr()) {
        let bytes = write_cbor(&expr).expect("write_cbor failed");
        prop_assert!(!bytes.is_empty());
    }
}

/// Strategy for SrcBang
fn arb_src_bang() -> impl Strategy<Value = SrcBang> {
    prop_oneof![
        Just(SrcBang::NoSrcBang),
        Just(SrcBang::SrcBang),
        Just(SrcBang::SrcUnpack),
    ]
}

/// Strategy for DataCon
fn arb_data_con() -> impl Strategy<Value = DataCon> {
    (
        any::<u64>().prop_map(DataConId),
        prop::string::string_regex("[a-zA-Z0-9_]{1,20}").unwrap(),
        any::<u32>(),
        any::<u32>(),
        prop::collection::vec(arb_src_bang(), 0..10),
    )
        .prop_map(|(id, name, tag, rep_arity, field_bangs)| DataCon {
            id,
            name,
            tag,
            rep_arity,
            field_bangs,
        })
}

/// Strategy for DataConTable
fn arb_data_con_table() -> impl Strategy<Value = DataConTable> {
    prop::collection::vec(arb_data_con(), 0..20).prop_map(|dcs| {
        let mut table = DataConTable::new();
        let mut seen_names = std::collections::HashSet::new();
        let mut seen_ids = std::collections::HashSet::new();
        for mut dc in dcs {
            // Ensure unique IDs and names for reliable round-trip equality.
            // DataConTable::insert overwrites, but we want to avoid collisions
            // that would make the result smaller than the input, or change names.
            if seen_ids.contains(&dc.id) {
                continue;
            }

            if seen_names.contains(&dc.name) {
                // If name is seen, make it unique using the ID
                dc.name = format!("{}_{}", dc.name, dc.id.0);
                // If the new name is ALSO seen (extremely unlikely with u64 ID but possible),
                // just skip it to maintain invariants simply.
                if seen_names.contains(&dc.name) {
                    continue;
                }
            }

            seen_ids.insert(dc.id);
            seen_names.insert(dc.name.clone());
            table.insert(dc);
        }
        table
    })
}

/// Round-trip property for DataConTable
proptest! {
    #[test]
    fn cbor_round_trip_data_con_table(table in arb_data_con_table()) {
        let bytes = write_metadata(&table).expect("write_metadata failed");
        let recovered = read_metadata(&bytes).expect("read_metadata failed");
        prop_assert_eq!(table, recovered);
    }
}

/// Strategy for Literal
fn arb_literal() -> impl Strategy<Value = Literal> {
    prop_oneof![
        any::<i64>().prop_map(Literal::LitInt),
        any::<u64>().prop_map(Literal::LitWord),
        any::<char>().prop_map(Literal::LitChar),
        prop::collection::vec(any::<u8>(), 0..100).prop_map(Literal::LitString),
        any::<u64>().prop_map(Literal::LitFloat),
        any::<u64>().prop_map(Literal::LitDouble),
    ]
}

/// Literal individual round-trip
proptest! {
    #[test]
    fn literal_round_trip(lit in arb_literal()) {
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Lit(lit)],
        };
        let bytes = write_cbor(&expr).expect("write_cbor failed");
        let recovered = read_cbor(&bytes).expect("read_cbor failed");
        prop_assert_eq!(expr, recovered);
    }
}

/// Deeply nested expressions round-trip correctly.
/// arb_core_expr already supports depth, but let's try to force some depth if possible.
/// Actually, arb_core_expr has a depth limit of 3 in tidepool-testing.
/// We'll define a simpler generator for deep nesting of just App or Lam.

fn gen_deep_expr(depth: usize) -> RecursiveTree<CoreFrame<usize>> {
    let mut builder = TreeBuilder::new();
    let mut current = builder.push(CoreFrame::Var(VarId(0)));
    for i in 1..depth {
        current = builder.push(CoreFrame::Lam {
            binder: VarId(i as u64),
            body: current,
        });
    }
    builder.build()
}

#[test]
fn nested_expr_round_trip() {
    for depth in [5, 10, 20, 50] {
        let expr = gen_deep_expr(depth);
        let bytes = write_cbor(&expr).expect("write_cbor failed");
        let recovered = read_cbor(&bytes).expect("read_cbor failed");
        assert_eq!(expr, recovered, "failed at depth {}", depth);
    }
}
