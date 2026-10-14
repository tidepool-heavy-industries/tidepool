use super::{bounded_activation_text, declaration_worth_showing, ACTIVATION_INPUT_LIMIT};

#[test]
fn library_reply_type_declaration_is_not_worth_showing() {
    // Text's own module (data.text) is neither a configured workspace
    // module nor a session-declared one: the model already knows the
    // type from "reply type Text" and gains nothing from its internal
    // Array/Int representation.
    assert!(!declaration_worth_showing(
        "Data.Text",
        &["Project.Shell".to_owned()],
    ));
}

#[test]
fn workspace_reply_type_declaration_is_worth_showing() {
    assert!(declaration_worth_showing(
        "Project.Shell",
        &["Project.Shell".to_owned()],
    ));
}

#[test]
fn session_declared_reply_type_declaration_is_worth_showing() {
    // Every session declaration lives under generation-versioned
    // `Tidepool.Session.Lib.G<n>`/`Tidepool.Session.Val.G<n>` modules,
    // never listed in `workspace_modules`.
    assert!(declaration_worth_showing("Tidepool.Session.Lib.G3", &[]));
}

#[test]
fn preview_bounds_preserve_unicode_and_mark_omission() {
    assert_eq!(
        bounded_activation_text("".into(), 4, false, "display sessionInput"),
        ""
    );
    assert_eq!(
        bounded_activation_text("a\nλ".into(), 4, false, "display sessionInput"),
        "a\nλ"
    );
    assert_eq!(
        bounded_activation_text("aλz".into(), 2, true, "display sessionInput"),
        "a\n<additional detail omitted past the 2 bytes cap; expand with `display sessionInput`>"
    );
    assert_eq!(
        bounded_activation_text("data R".into(), 4, false, "lookup R"),
        "data\n<additional detail omitted past the 4 bytes cap; expand with `lookup R`>"
    );
}

#[test]
fn activation_renders_a_whole_task_and_names_the_cap_past_it() {
    let task = format!(
        "Task {{taskName = \"wave/lane\", planPath = \"plans/x.md\", \
         taskSource = GitOid \"3454b78\", obligation = \"{}\", rationale = \"r\", \
         ownedPaths = [\"src/a.rs\"], acceptance = \"tests pass\", acceptedDecisions = []}}",
        "o".repeat(4 * 1024)
    );
    assert_eq!(
        bounded_activation_text(
            task.clone(),
            ACTIVATION_INPUT_LIMIT,
            false,
            "display sessionInput"
        ),
        task
    );
    let oversized = "x".repeat(ACTIVATION_INPUT_LIMIT + 1);
    let rendered = bounded_activation_text(
        oversized,
        ACTIVATION_INPUT_LIMIT,
        false,
        "display sessionInput",
    );
    assert!(rendered.starts_with(&"x".repeat(ACTIVATION_INPUT_LIMIT)));
    assert!(rendered.ends_with(
        "\n<additional detail omitted past the 32 KiB cap; expand with `display sessionInput`>"
    ));
}
