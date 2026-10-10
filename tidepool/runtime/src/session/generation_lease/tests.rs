use super::*;

#[test]
fn refill_covers_shortage_large_request_and_final_partial_range() {
    assert_eq!(refill_ceiling(Generation(0), Generation(1)), Generation(64));
    assert_eq!(
        refill_ceiling(Generation(64), Generation(65)),
        Generation(128)
    );
    assert_eq!(
        refill_ceiling(Generation(64), Generation(200)),
        Generation(200)
    );
    assert_eq!(
        refill_ceiling(Generation(u64::MAX - 7), Generation(u64::MAX - 6)),
        Generation(u64::MAX)
    );
}

#[test]
fn confirmed_tail_at_max_is_consumable_then_typed_exhaustion() {
    let lease = GenerationRangeLease::confirmed(Generation(u64::MAX));
    let end = requested_end(Generation(u64::MAX - 1), 1).unwrap();
    assert!(lease.covers(end));
    assert_eq!(
        requested_end(end, 1),
        Err(GenerationExhausted {
            consumed: end,
            requested: 1
        })
    );
    assert_eq!(requested_end(end, 0).unwrap(), end);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 128, ..Default::default() })]
    #[test]
    fn full_u64_arithmetic_matches_independent_wide_integer_model(
        consumed in proptest::prelude::any::<u64>(),
        amount in proptest::prelude::any::<u64>(),
        other in proptest::prelude::any::<u64>(),
    ) {
        let wide_end = u128::from(consumed) + u128::from(amount);
        let actual_end = requested_end(Generation(consumed), amount);
        if wide_end > u128::from(u64::MAX) {
            proptest::prop_assert_eq!(actual_end, Err(GenerationExhausted { consumed: Generation(consumed), requested: amount }));
        } else {
            proptest::prop_assert_eq!(actual_end.unwrap().0, wide_end as u64);
        }
        let ceiling = consumed.min(other);
        let end = consumed.max(other);
        if ceiling < end {
            let expected = (u128::from(ceiling) + 64).max(u128::from(end)).min(u128::from(u64::MAX));
            let actual = refill_ceiling(Generation(ceiling), Generation(end));
            proptest::prop_assert_eq!(u128::from(actual.0), expected);
            proptest::prop_assert!(actual.0 >= end);
        }
    }
}
