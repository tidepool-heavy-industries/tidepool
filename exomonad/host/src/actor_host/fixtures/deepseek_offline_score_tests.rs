#[path = "score.rs"]
mod score;

#[test]
fn empty_input_has_zero_score() {
    assert_eq!(score::score(&[]), 0);
}

#[test]
fn mixed_signs_preserve_their_contribution() {
    assert_eq!(score::score(&[19, -7, 30]), 42);
}

#[test]
fn an_all_negative_score_stays_negative() {
    assert_eq!(score::score(&[-17, -25]), -42);
}
