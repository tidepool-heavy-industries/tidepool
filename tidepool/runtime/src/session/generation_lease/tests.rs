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
