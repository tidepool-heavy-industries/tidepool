//! Pure formatters for `ResidentKernelBehavior::live_status_text`'s
//! what-is-live status view: the bindings section, the collectors-section job
//! lines, and the source-drift rows. Each is a pure function of already-owned
//! data (the actor's own workbench execution journal, its command jobs, its
//! observed source drift) — no new tracking, and directly testable without an
//! actor.

use super::*;

/// Exact text spanned by `span` within `source`, using GHC's one-based
/// line/column coordinates (the convention [`CellSourceSpan`] documents).
/// `None` if the span does not fit `source` (e.g. it was recorded against a
/// since-rewritten cell).
fn slice_cell_span(source: &str, span: &CellSourceSpan) -> Option<String> {
    let lines: Vec<&str> = source.split('\n').collect();
    let start_line = span.start_line.checked_sub(1)?;
    let end_line = span.end_line.checked_sub(1)?;
    if start_line > end_line || end_line >= lines.len() {
        return None;
    }
    if start_line == end_line {
        let line: Vec<char> = lines[start_line].chars().collect();
        let start = span.start_column.saturating_sub(1).min(line.len());
        let end = span.end_column.saturating_sub(1).clamp(start, line.len());
        return Some(line[start..end].iter().collect());
    }
    let mut out = String::new();
    let span_lines = &lines[start_line..=end_line];
    let last = span_lines.len() - 1;
    for (offset, raw_line) in span_lines.iter().enumerate() {
        let line: Vec<char> = raw_line.chars().collect();
        if offset == 0 {
            let start = span.start_column.saturating_sub(1).min(line.len());
            out.extend(&line[start..]);
        } else if offset == last {
            let end = span.end_column.saturating_sub(1).min(line.len());
            out.extend(&line[..end]);
        } else {
            out.push_str(raw_line);
        }
        if offset != last {
            out.push('\n');
        }
    }
    Some(out)
}

/// Identifier-shaped tokens in `source` that could name another session
/// binding: a run of letters/digits/`_`/`'` starting lower-case or `_`, the
/// shape a Haskell variable or function name takes. This is a textual scan,
/// not a GHC-verified resolution — [`ResidentKernelBehavior::live_status_text`]
/// uses it only to flag same-session names a binding's source *mentions*
/// that are no longer live, which a reader can then check by hand.
fn same_session_identifiers(source: &str) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut current = String::new();
    for character in source.chars().chain(std::iter::once(' ')) {
        if character.is_alphanumeric() || character == '_' || character == '\'' {
            current.push(character);
            continue;
        }
        if current
            .chars()
            .next()
            .is_some_and(|first| first.is_lowercase() || first == '_')
        {
            names.insert(std::mem::take(&mut current));
        } else {
            current.clear();
        }
    }
    names
}

/// The three source-drift rows of the what-is-live status view: is what is
/// running still what is on disk. Each row is independently `None` when it
/// has not been observed for this actor (see
/// `runtime_observation::ActorSourceDriftObservation`) and renders as
/// "not observed" rather than as clean — an unread row must never look like
/// a checked-and-identical one. A pure formatter, directly testable without
/// an actor.
pub(super) fn render_source_drift_section(drift: &crate::ActorSourceDriftObservation) -> String {
    let layer = match &drift.layer {
        None => "  source layer: not observed".to_owned(),
        Some(layer) if layer.changed_modules.is_empty() => format!(
            "  source layer: active={}@{} disk={}@{} (checked, identical)",
            layer.active_identity,
            layer.active_generation,
            layer.disk_identity,
            layer.disk_generation,
        ),
        Some(layer) => {
            let mut changed = layer.changed_modules.clone();
            changed.sort();
            format!(
                "  source layer: active={}@{} disk={}@{} changed_modules={changed:?}",
                layer.active_identity,
                layer.active_generation,
                layer.disk_identity,
                layer.disk_generation,
            )
        }
    };
    let checkout = match &drift.checkout {
        None => "  checkout: not observed".to_owned(),
        Some(checkout) if checkout.dirty_files.is_empty() => format!(
            "  checkout: head={} (checked, clean); binary_build_revision=unavailable (not recorded by this build)",
            checkout.head,
        ),
        Some(checkout) => {
            let mut dirty = checkout.dirty_files.clone();
            dirty.sort();
            format!(
                "  checkout: head={} dirty_files={dirty:?}; binary_build_revision=unavailable (not recorded by this build)",
                checkout.head,
            )
        }
    };
    let frozen = match &drift.frozen {
        None => "  frozen workspace: not observed".to_owned(),
        Some(frozen) if frozen.changed_modules.is_empty() => {
            "  frozen workspace: (checked, identical)".to_owned()
        }
        Some(frozen) => {
            let mut changed = frozen.changed_modules.clone();
            changed.sort();
            format!("  frozen workspace: changed_modules={changed:?}")
        }
    };
    format!("{layer}\n{checkout}\n{frozen}")
}

/// One collectors-section line of the what-is-live status view: a job, its
/// owner, the actors observing its completion ("collectors", in
/// `Tidepool.Actor` usage), and whether it has finished. A pure formatter so
/// the finished/running distinction is directly testable without an actor.
pub(super) fn render_job_line(job: &crate::command_jobs::CommandJobSnapshot) -> String {
    format!(
        "  - job {} owner={}@{} collectors={:?} finished={}",
        job.id,
        job.owner.id.0,
        job.owner.incarnation.0,
        job.observers
            .iter()
            .map(|observer| format!("{}@{}", observer.id.0, observer.incarnation.0))
            .collect::<Vec<_>>(),
        job.finished,
    )
}

/// The execution that installed `name`, the exact source text of the item
/// that installed it (sliced from that execution's retained raw cell source
/// via the span the item recorded), and how many other declarations shared
/// that item. `None` when no retained, settled execution's reply installed
/// `name` with both a span and a raw cell source still available (a hosted
/// tool call, or an item the compiler never attributed a span to, leaves
/// nothing to slice).
fn defining_execution(
    name: &str,
    executions: &[(
        WorkbenchExecutionId,
        &WorkbenchRequest,
        &crate::KernelWorkbenchReply,
    )],
) -> Option<(WorkbenchExecutionId, String, usize)> {
    executions.iter().find_map(|(execution, request, reply)| {
        let response = reply.as_ref().ok()?;
        response.items.iter().find_map(|item| {
            if !item
                .installed_bindings
                .iter()
                .any(|installed| installed == name)
            {
                return None;
            }
            let span = item.span.as_ref()?;
            let source = request.cell_source()?;
            let text = slice_cell_span(source, span)?;
            Some((execution.clone(), text, item.source_items.len()))
        })
    })
}

/// The bindings section of the what-is-live status view: for each binding,
/// its defining generation, the execution id and exact source of the cell
/// that defined it (via [`defining_execution`]), and any same-session name
/// its source mentions ([`same_session_identifiers`]) that this actor's
/// workbench has bound at some point but that is not currently live. A pure
/// function of `bindings` (from `ResidentSession::workbench_bindings_in`)
/// and `executions` (this actor's own replay journal via
/// `WorkbenchExecutions::terminal_entries`) — no new tracking, and directly
/// testable without an actor.
pub(super) fn render_bindings_section(
    bindings: &[tidepool_runtime::session::WorkbenchBinding],
    executions: &[(
        WorkbenchExecutionId,
        &WorkbenchRequest,
        &crate::KernelWorkbenchReply,
    )],
) -> String {
    if bindings.is_empty() {
        return "  (no persistent bindings)".to_owned();
    }
    let mut journal_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (_, _, reply) in executions {
        if let Ok(response) = reply {
            for item in &response.items {
                for name in &item.installed_bindings {
                    journal_names.insert(name.as_str());
                }
            }
        }
    }
    let live_names: std::collections::HashSet<&str> = bindings
        .iter()
        .map(|binding| binding.name.as_str())
        .collect();

    let mut lines = bindings
        .iter()
        .map(|binding| {
            let generation = binding
                .defining_generation()
                .map_or_else(|| "unknown".to_owned(), |generation| generation.to_string());
            let (execution, source, group_size) =
                match defining_execution(&binding.name, executions) {
                    Some((execution, source, group_size)) => {
                        (execution.to_string(), source, group_size)
                    }
                    None => ("unavailable".to_owned(), "unavailable".to_owned(), 0),
                };
            let unresolved = if source == "unavailable" {
                Vec::new()
            } else {
                let mut unresolved = same_session_identifiers(&source)
                    .into_iter()
                    .filter(|name| {
                        *name != binding.name
                            && journal_names.contains(name.as_str())
                            && !live_names.contains(name.as_str())
                    })
                    .collect::<Vec<_>>();
                unresolved.sort();
                unresolved
            };
            let group = if group_size > 1 {
                format!(
                    " (shares its defining cell item with {} other declaration(s))",
                    group_size - 1
                )
            } else {
                String::new()
            };
            format!(
                "  - {} [{}] gen={generation} exec={execution} source={source:?}{group} unresolved={unresolved:?}",
                binding.name,
                binding.kind.label(),
            )
        })
        .collect::<Vec<_>>();
    lines.sort();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_jobs::CommandJobSnapshot;
    use crate::{ActorId, Incarnation};
    use tidepool_runtime::session::WorkbenchBinding;

    /// A single-item, single-binding committed reply naming `binding` and
    /// spanning `source` in full, paired with the request that retains
    /// `source` as its raw cell text — the minimal fixture
    /// `render_bindings_section`'s tests build against.
    fn committed_execution(
        digest: u8,
        source: &str,
        binding: &str,
    ) -> (
        WorkbenchExecutionId,
        WorkbenchRequest,
        crate::KernelWorkbenchReply,
    ) {
        let execution = WorkbenchExecutionId::from_digest([digest; 16]);
        let request =
            WorkbenchRequest::from_cell_input(source).with_execution_id(execution.clone());
        let end_column = source.trim_end_matches('\n').chars().count() + 1;
        let response = WorkbenchResponse {
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: vec![WorkbenchItemReceipt {
                diagnostics: Vec::new(),
                index: 0,
                kind: None,
                span: Some(CellSourceSpan {
                    start_line: 1,
                    start_column: 1,
                    end_line: 1,
                    end_column,
                }),
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Committed,
                output: String::new(),
                warnings: Vec::new(),
                installed_bindings: vec![binding.to_owned()],
                operations: Vec::new(),
                terminal_transfer: None,
                failure_layer: None,
            }],
            next_index: 1,
            total: 1,
        };
        (execution, request, Ok(response))
    }

    #[test]
    fn slice_cell_span_extracts_exact_text_and_rejects_an_out_of_range_span() {
        let source = "x = 5\ny = 6\n";
        let first = CellSourceSpan {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 6,
        };
        assert_eq!(slice_cell_span(source, &first).as_deref(), Some("x = 5"));
        let second = CellSourceSpan {
            start_line: 2,
            start_column: 1,
            end_line: 2,
            end_column: 6,
        };
        assert_eq!(slice_cell_span(source, &second).as_deref(), Some("y = 6"));
        let stale = CellSourceSpan {
            start_line: 5,
            start_column: 1,
            end_line: 5,
            end_column: 2,
        };
        assert_eq!(slice_cell_span(source, &stale), None);
    }

    #[test]
    fn same_session_identifiers_keeps_only_lower_case_leading_tokens() {
        let names = same_session_identifiers("f x = g x + Y 3 + _hidden");
        assert!(names.contains("f"));
        assert!(names.contains("x"));
        assert!(names.contains("g"));
        assert!(names.contains("_hidden"));
        assert!(!names.contains("Y"), "{names:?}");
        assert!(!names.contains("3"), "{names:?}");
    }

    #[test]
    fn defining_execution_finds_the_exact_source_that_installed_a_binding() {
        let (execution, request, reply) = committed_execution(9, "x = 5", "x");
        let executions = vec![(execution.clone(), &request, &reply)];
        let (found, source, group_size) =
            defining_execution("x", &executions).expect("x was installed with a recorded span");
        assert_eq!(found, execution);
        assert_eq!(source, "x = 5");
        assert_eq!(group_size, 0);
        assert_eq!(defining_execution("never_bound", &executions), None);
    }

    #[test]
    fn bindings_section_is_absent_when_there_are_no_bindings() {
        assert_eq!(
            render_bindings_section(&[], &[]),
            "  (no persistent bindings)"
        );
    }

    #[test]
    fn bindings_section_shows_generation_source_and_execution_for_a_matched_binding() {
        let (execution, request, reply) = committed_execution(5, "answer = 42", "answer");
        let executions = vec![(execution.clone(), &request, &reply)];
        let binding = WorkbenchBinding::declaration("answer".into(), "answer".into())
            .with_generation(Some(1));
        let text = render_bindings_section(std::slice::from_ref(&binding), &executions);
        assert!(text.contains("gen=1"), "{text}");
        assert!(text.contains(&format!("exec={execution}")), "{text}");
        assert!(text.contains("source=\"answer = 42\""), "{text}");

        // A binding the journal has no record of (a materialized value
        // mounted directly, or a journal entry that aged out) renders
        // honestly as unavailable rather than a guess.
        let unrecorded = WorkbenchBinding::materialized("mystery".into(), Some("Int".into()))
            .with_generation(Some(3));
        let text = render_bindings_section(std::slice::from_ref(&unrecorded), &[]);
        assert!(text.contains("gen=3"), "{text}");
        assert!(text.contains("exec=unavailable"), "{text}");
        assert!(text.contains("source=\"unavailable\""), "{text}");
    }

    #[test]
    fn bindings_section_flags_a_same_session_name_that_is_no_longer_live() {
        let helper = committed_execution(6, "helper = 41", "helper");
        let total = committed_execution(7, "total = helper + 1", "total");
        let executions = vec![
            (helper.0.clone(), &helper.1, &helper.2),
            (total.0.clone(), &total.1, &total.2),
        ];
        // Only `total` is still live: `helper` was retracted or shadowed
        // away since its cell ran, but this actor's own journal still
        // remembers it was once a session binding.
        let binding = WorkbenchBinding::materialized("total".into(), None).with_generation(Some(2));
        let text = render_bindings_section(std::slice::from_ref(&binding), &executions);
        assert!(text.contains("unresolved=[\"helper\"]"), "{text}");
    }

    #[test]
    fn job_line_renders_a_finished_job_differently_from_a_running_one() {
        let owner = ActorRef {
            id: ActorId(1),
            incarnation: Incarnation(1),
        };
        let collector = ActorRef {
            id: ActorId(2),
            incarnation: Incarnation(1),
        };
        let running = CommandJobSnapshot {
            id: "job-a".into(),
            owner,
            observers: vec![collector],
            finished: false,
        };
        let finished = CommandJobSnapshot {
            finished: true,
            ..running.clone()
        };
        let running_line = render_job_line(&running);
        let finished_line = render_job_line(&finished);
        assert_ne!(running_line, finished_line);
        assert!(running_line.contains("finished=false"), "{running_line}");
        assert!(finished_line.contains("finished=true"), "{finished_line}");
        assert!(
            finished_line.contains("collectors=[\"2@1\"]"),
            "{finished_line}"
        );
    }

    #[test]
    fn source_drift_section_marks_every_unobserved_row_as_not_observed() {
        // No row has been published for this actor yet: the honest render is
        // "not observed" for all three, never "clean" or a guessed value.
        let text = render_source_drift_section(&crate::ActorSourceDriftObservation::default());
        assert!(text.contains("source layer: not observed"), "{text}");
        assert!(text.contains("checkout: not observed"), "{text}");
        assert!(text.contains("frozen workspace: not observed"), "{text}");
    }

    #[test]
    fn source_drift_section_names_a_changed_module_in_the_layer_row() {
        let drift = crate::ActorSourceDriftObservation {
            layer: Some(crate::SourceLayerDrift {
                active_identity: "rev-a".into(),
                active_generation: 3,
                disk_identity: "rev-b".into(),
                disk_generation: 0,
                changed_modules: vec!["Project.Work".into()],
            }),
            ..Default::default()
        };
        let text = render_source_drift_section(&drift);
        assert!(text.contains("active=rev-a@3"), "{text}");
        assert!(text.contains("disk=rev-b@0"), "{text}");
        assert!(
            text.contains("changed_modules=[\"Project.Work\"]"),
            "{text}"
        );
        assert!(!text.contains("checked, identical"), "{text}");
    }

    #[test]
    fn source_drift_section_reports_identical_revisions_as_checked_not_skipped() {
        let drift = crate::ActorSourceDriftObservation {
            layer: Some(crate::SourceLayerDrift {
                active_identity: "rev-a".into(),
                active_generation: 2,
                disk_identity: "rev-a".into(),
                disk_generation: 2,
                changed_modules: Vec::new(),
            }),
            ..Default::default()
        };
        let text = render_source_drift_section(&drift);
        assert!(
            text.contains("source layer: active=rev-a@2 disk=rev-a@2 (checked, identical)"),
            "{text}"
        );
    }

    #[test]
    fn source_drift_section_lists_a_dirty_checkouts_files() {
        let drift = crate::ActorSourceDriftObservation {
            checkout: Some(crate::CheckoutGitDrift {
                head: "abc123".into(),
                dirty_files: vec!["src/lib.rs".into(), "Cargo.toml".into()],
            }),
            ..Default::default()
        };
        let text = render_source_drift_section(&drift);
        assert!(text.contains("checkout: head=abc123"), "{text}");
        assert!(
            text.contains("dirty_files=[\"Cargo.toml\", \"src/lib.rs\"]"),
            "{text}"
        );
        // The binary's build revision is never recorded, so this says so
        // rather than pairing the head with a guessed value.
        assert!(text.contains("binary_build_revision=unavailable"), "{text}");
    }

    #[test]
    fn source_drift_section_reports_a_clean_checkout_as_checked() {
        let drift = crate::ActorSourceDriftObservation {
            checkout: Some(crate::CheckoutGitDrift {
                head: "abc123".into(),
                dirty_files: Vec::new(),
            }),
            ..Default::default()
        };
        let text = render_source_drift_section(&drift);
        assert!(
            text.contains("checkout: head=abc123 (checked, clean)"),
            "{text}"
        );
    }

    #[test]
    fn source_drift_section_names_frozen_modules_that_differ_from_disk() {
        let drift = crate::ActorSourceDriftObservation {
            frozen: Some(crate::FrozenSourceDrift {
                changed_modules: vec!["Project.Types".into()],
            }),
            ..Default::default()
        };
        let text = render_source_drift_section(&drift);
        assert!(
            text.contains("frozen workspace: changed_modules=[\"Project.Types\"]"),
            "{text}"
        );
    }
}
