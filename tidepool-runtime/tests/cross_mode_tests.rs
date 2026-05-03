//! Sanity tests for the cross-mode test harness.

mod cross_mode_harness;

use cross_mode_harness::{CrossModeFixture, assert_cross_mode_structurally_equivalent, assert_cross_mode_pure_equivalent};

#[test]
fn harness_pure_value_roundtrips() {
    let fixture = CrossModeFixture {
        single: r#"
module Test where
fortyTwo :: Int
fortyTwo = 42
"#.to_string(),
        split: vec![
            ("Helper.hs".to_string(), r#"
module Helper where
fortyTwo :: Int
fortyTwo = 42
"#.to_string()),
        ],
        target: "fortyTwo",
    };

    // Assert structural equivalence (Core trees modulo IDs)
    assert_cross_mode_structurally_equivalent(&fixture);

    // Assert runtime equivalence
    assert_cross_mode_pure_equivalent(&fixture);
}

#[test]
fn harness_detects_obvious_divergence() {
    let fixture = CrossModeFixture {
        single: r#"
module M where
x = 1 :: Int
"#.to_string(),
        split: vec![
            ("Main.hs".to_string(), r#"
module Test where
x = 2 :: Int
"#.to_string()),
        ],
        target: "x",
    };

    // Runtime equivalence should fail because 1 != 2
    let result = std::panic::catch_unwind(|| {
        assert_cross_mode_pure_equivalent(&fixture);
    });

    assert!(result.is_err(), "harness should have detected value divergence");
}
