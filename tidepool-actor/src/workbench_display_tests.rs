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

#[test]
fn bounds_output_bytes_with_unicode_and_both_ends() {
    let input = format!("first line\n{}last line\n", "λ-output\n".repeat(10_000));
    for budget in [0, 1, 2, 159, 192, 256, 1024, 65_536] {
        let output = super::bounded_output(&input, budget);
        assert!(
            output.len() <= budget,
            "budget={budget}, length={}",
            output.len()
        );
        if budget >= 256 {
            assert!(output.starts_with("first line\n"));
            assert!(output.ends_with("last line\n"));
            assert!(output.contains("bytes not displayed"));
        }
    }
    assert_eq!(super::bounded_output("λ\n", 3), "λ\n");
}

#[test]
fn bounded_output_shares_an_aggregate_allowance() {
    let mut remaining = 65_536;
    let mut rendered = String::new();
    for _ in 0..8 {
        let text = super::bounded_output(&"some output\n".repeat(2000), remaining);
        remaining -= text.len();
        rendered.push_str(&text);
    }
    assert!(rendered.len() <= 65_536);
    assert_eq!(rendered.len() + remaining, 65_536);
}

#[test]
fn command_pages_explain_gaps_without_merging_stream_order() {
    use tidepool_bridge_effects::{CommandPage, CommandStream};
    let page = |text: &str, start, end| CommandPage {
        text: text.into(),
        start,
        end,
        available_end: 100,
        retained_start: 0,
        lost_bytes: 0,
        finished: true,
        lossy: false,
        leading_fragment: false,
        trailing_fragment: false,
    };
    let rendered = super::command_pages(&[
        (CommandStream::Stdout, page("first\n", 0, 6)),
        (CommandStream::Stdout, page("last\n", 95, 100)),
        (CommandStream::Stderr, page("error\n", 0, 6)),
    ]);
    assert!(rendered.contains("89 retained bytes between displayed pages"));
    assert_eq!(rendered.matches("between displayed pages").count(), 1);
    assert!(rendered.contains("stdout · bytes 95–100 of 100"));
    assert!(rendered.contains("stderr · bytes 0–6 of 100"));
}
