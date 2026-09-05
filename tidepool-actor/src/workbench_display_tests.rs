use super::layout;

#[test]
fn lays_out_nested_values_without_touching_literals() {
    let literal = r#""a,b {c} \"quoted\" \\ suffix""#;
    let input = format!("Result {{ evidence = [{literal}, {literal}, {literal}], state = Ready }}");
    let output = layout(&input);
    assert!(output.contains('\n'));
    assert_eq!(output.matches(literal).count(), 3);
    assert_eq!(
        output.split_whitespace().collect::<String>(),
        input.split_whitespace().collect::<String>()
    );
}

#[test]
fn preserves_short_custom_and_ambiguous_output() {
    for input in [
        "Just (Right 3)".to_owned(),
        format!("custom\n{}", "x".repeat(120)),
        format!("[{}, broken", "x".repeat(120)),
        format!("[{}, \"unclosed]", "x".repeat(120)),
    ] {
        assert_eq!(layout(&input), input);
    }
}
