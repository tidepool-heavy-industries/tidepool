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

/// One caller's last rendered summary roster: each row's rendered text,
/// with ages normalized out ([`without_ages`]), by its row key (label and
/// exact incarnation), and when it was rendered. The actor keeps exactly
/// one, replaced on every summary-family status call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RosterSnapshot {
    pub(super) rendered_at_unix_ms: u64,
    pub(super) rows: std::collections::BTreeMap<String, String>,
}

impl RosterSnapshot {
    pub(super) fn new(rendered_at_unix_ms: u64, rows: &[(String, String)]) -> Self {
        Self {
            rendered_at_unix_ms,
            rows: rows
                .iter()
                .map(|(key, row)| (key.clone(), without_ages(row)))
                .collect(),
        }
    }
}

/// A rendered row with every age token (`3m`, `40s`, `2h`, `1d`: digits
/// then one unit letter, standing alone) replaced by `<age>`, so a row whose
/// only change is elapsed time compares equal. Printed rows keep their ages.
fn without_ages(row: &str) -> String {
    let bytes = row.as_bytes();
    let standalone = |index: Option<&u8>| index.is_none_or(|byte| !byte.is_ascii_alphanumeric());
    let mut out = String::with_capacity(row.len());
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        if bytes[index].is_ascii_digit() && (start == 0 || standalone(bytes.get(start - 1))) {
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if matches!(bytes.get(end), Some(b's' | b'm' | b'h' | b'd'))
                && standalone(bytes.get(end + 1))
            {
                out.push_str("<age>");
                index = end + 1;
                continue;
            }
            out.push_str(&row[start..end]);
            index = end;
            continue;
        }
        let character = row[index..].chars().next().unwrap_or_default();
        out.push(character);
        index += character.len_utf8().max(1);
    }
    out
}

/// The `changed` roster: rows whose rendered text, ages normalized out,
/// differs from `previous` (new rows included) in `current`'s order, then
/// rows `previous` listed that `current` no longer does, then one
/// `N unchanged since <time>` line. `current` is `(row key, rendered row)`.
pub(super) fn render_roster_changes(
    previous: &RosterSnapshot,
    current: &[(String, String)],
) -> String {
    let mut lines = Vec::new();
    let mut unchanged = 0usize;
    for (key, row) in current {
        if previous.rows.get(key) == Some(&without_ages(row)) {
            unchanged += 1;
        } else {
            lines.push(row.clone());
        }
    }
    let listed = current
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<std::collections::HashSet<_>>();
    lines.extend(
        previous
            .rows
            .keys()
            .filter(|key| !listed.contains(key.as_str()))
            .map(|key| format!("  - {key} no longer listed")),
    );
    lines.push(format!(
        "  {unchanged} unchanged since {}",
        crate::runtime_observation::render_clock(previous.rendered_at_unix_ms)
    ));
    lines.join("\n")
}

/// The revision identities one actor works against, each already observed
/// by the host; `None` means not observed (or, for `assignment_base`, that
/// the current request carries none).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RevisionIdentities {
    /// HEAD of the run's workspace repository, as the root's checkout poll
    /// last observed it.
    pub(super) operator_checkout: Option<String>,
    /// This actor's own assigned checkout, as its checkout poll last
    /// observed it.
    pub(super) checked: Option<crate::CheckoutGitDrift>,
    /// `taskSource` of the current request's session input.
    pub(super) assignment_base: Option<String>,
    /// Each live descendant's label and last observed checked head.
    pub(super) children: Vec<(String, Option<String>)>,
    /// This actor's installed source layer against its on-disk capture.
    pub(super) layer: Option<crate::SourceLayerDrift>,
}

fn short_oid(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

fn short_identity(identity: &str) -> &str {
    identity.get(..12).unwrap_or(identity)
}

/// The `revisions` section: which revision each identity names, in the
/// delivery line's `key=value; key=value` style, and what a source reload
/// would publish. Unobserved values say so; nothing is inferred.
pub(super) fn render_revisions_section(revisions: &RevisionIdentities) -> String {
    let operator = revisions
        .operator_checkout
        .as_deref()
        .map_or("unobserved", short_oid);
    let checked = revisions.checked.as_ref().map_or_else(
        || "unobserved".to_owned(),
        |checkout| {
            let state = if checkout.dirty_files.is_empty() {
                "clean".to_owned()
            } else {
                format!("dirty={}", checkout.dirty_files.len())
            };
            format!("{} {state}", short_oid(&checkout.head))
        },
    );
    let base = revisions
        .assignment_base
        .as_deref()
        .map_or("none", short_oid);
    let (layer, publish) = match &revisions.layer {
        None => (
            "source_layer=unobserved".to_owned(),
            "unknown (source layer unobserved)".to_owned(),
        ),
        Some(drift) => (
            format!(
                "source_layer={}@{}; disk={}@{}; drifted={}",
                short_identity(&drift.active_identity),
                drift.active_generation,
                short_identity(&drift.disk_identity),
                drift.disk_generation,
                if drift.changed_modules.is_empty() {
                    "no"
                } else {
                    "yes"
                },
            ),
            if drift.changed_modules.is_empty() {
                "nothing".to_owned()
            } else {
                drift.changed_modules.join(", ")
            },
        ),
    };
    let mut lines = vec![
        "revisions:".to_owned(),
        format!("  operator_checkout={operator}; checked_head={checked}; assignment_base={base}"),
        format!("  {layer}"),
        format!("  a reload would publish: {publish}"),
    ];
    lines.extend(revisions.children.iter().map(|(label, head)| {
        format!(
            "  descendant {label:?} checked_head={}",
            head.as_deref().map_or("unobserved", short_oid)
        )
    }));
    lines.join("\n")
}

/// The Git OID a session input's rendered `taskSource` field names: the
/// first 40-hex-digit run within a short distance after the field name,
/// whatever wrapper the rendering puts around it. `None` when the input has
/// no such field.
pub(super) fn assignment_base_from_input(rendered_input: &str) -> Option<String> {
    const SEARCH_BYTES: usize = 64;
    let start = rendered_input.find("taskSource")? + "taskSource".len();
    let bytes = rendered_input.get(start..)?.as_bytes();
    let mut run = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if byte.is_ascii_hexdigit() {
            run += 1;
            let boundary = bytes
                .get(index + 1)
                .is_none_or(|next| !next.is_ascii_hexdigit());
            if run == 40 && boundary {
                let oid = &bytes[index + 1 - 40..=index];
                return Some(String::from_utf8_lossy(oid).to_ascii_lowercase());
            }
        } else if index >= SEARCH_BYTES {
            return None;
        } else {
            run = 0;
        }
    }
    None
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
    fn roster_row(label: &str, id: u64, delivery: &str) -> (String, String) {
        (
            format!("{label:?} ({id}@1)"),
            format!(
                "  - {label:?} ({id}@1) supervisor=1@1 role=Coding state=running\n    {label} req=0 pending; provider=idle; inbox=open; last_message=none; next={delivery}"
            ),
        )
    }

    #[test]
    fn status_roster_changes_list_only_rows_whose_rendered_text_changed() {
        let first = vec![
            roster_row("worker-a", 2, "await-event"),
            roster_row("worker-b", 3, "await-event"),
        ];
        // 12:00:00Z
        let snapshot = RosterSnapshot::new(43_200_000, &first);

        // Nothing changed: no rows, only the unchanged line.
        let unchanged = render_roster_changes(&snapshot, &first);
        assert_eq!(unchanged, "  2 unchanged since 12:00:00Z");

        // One child's rendered line changed: exactly that row renders.
        let second = vec![
            roster_row("worker-a", 2, "await-event"),
            roster_row("worker-b", 3, "await-cell"),
        ];
        let changed = render_roster_changes(&snapshot, &second);
        assert_eq!(
            changed,
            format!("{}\n  1 unchanged since 12:00:00Z", second[1].1)
        );
        assert!(!changed.contains("\"worker-a\""), "{changed}");

        // A row that is no longer listed is named, not silently dropped.
        let gone = render_roster_changes(&snapshot, &first[..1]);
        assert_eq!(
            gone,
            "  - \"worker-b\" (3@1) no longer listed\n  1 unchanged since 12:00:00Z"
        );
    }

    #[test]
    fn status_roster_changes_ignore_a_row_whose_only_change_is_an_age() {
        let row = |age: &str, fence: &str| {
            (
                "\"worker-a\" (2@1)".to_owned(),
                format!(
                    "  - \"worker-a\" (2@1) state=running\n    worker-a req=0 pending; provider=idle {age}; inbox=fenced(turn, {fence}); last_message=ref3 presented@11:58:00Z; next=await-event"
                ),
            )
        };
        let snapshot = RosterSnapshot::new(43_200_000, &[row("3m", "40s")]);
        assert_eq!(
            render_roster_changes(&snapshot, &[row("4m", "1m")]),
            "  1 unchanged since 12:00:00Z"
        );
        // A non-age change on the same row still renders it, ages as printed.
        let (key, moved) = row("4m", "1m");
        let moved = moved.replace("await-event", "await-cell");
        assert_eq!(
            render_roster_changes(&snapshot, &[(key, moved.clone())]),
            format!("{moved}\n  0 unchanged since 12:00:00Z")
        );
    }

    #[test]
    fn status_revisions_section_renders_every_identity() {
        let revisions = RevisionIdentities {
            operator_checkout: Some("1884c03c8aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            checked: Some(crate::CheckoutGitDrift {
                head: "9a1b2c3dbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                dirty_files: vec!["src/lib.rs".into(), "Cargo.toml".into()],
            }),
            assignment_base: Some("c427057eccccccccccccccccccccccccccccccccc".into()),
            children: vec![
                (
                    "worker-a".into(),
                    Some("77aa001ddddddddddddddddddddddddddddddddd".into()),
                ),
                ("worker-b".into(), None),
            ],
            layer: Some(crate::SourceLayerDrift {
                active_identity: "5f0e2d4c3b2a19180706".into(),
                active_generation: 3,
                disk_identity: "e1d2c3b4a5968778695a".into(),
                disk_generation: 0,
                changed_modules: vec!["Project.Shell".into(), "Project.Work".into()],
            }),
        };
        assert_eq!(
            render_revisions_section(&revisions),
            "revisions:\n  operator_checkout=1884c03; checked_head=9a1b2c3 dirty=2; assignment_base=c427057\n  source_layer=5f0e2d4c3b2a@3; disk=e1d2c3b4a596@0; drifted=yes\n  a reload would publish: Project.Shell, Project.Work\n  descendant \"worker-a\" checked_head=77aa001\n  descendant \"worker-b\" checked_head=unobserved"
        );

        // Nothing observed: every identity says so, nothing is inferred.
        assert_eq!(
            render_revisions_section(&RevisionIdentities::default()),
            "revisions:\n  operator_checkout=unobserved; checked_head=unobserved; assignment_base=none\n  source_layer=unobserved\n  a reload would publish: unknown (source layer unobserved)"
        );
    }

    #[test]
    fn status_assignment_base_reads_task_source_from_the_rendered_input() {
        let oid = "c427057e0123456789abcdef0123456789abcdef";
        for rendered in [
            format!("Task {{ taskGroup = \"work\", planPath = \"p\", taskSource = GitOid \"{oid}\", obligation = \"x\" }}"),
            format!("Task {{taskSource = GitOid {{unGitOid = \"{oid}\"}}}}"),
            format!("{{\"taskSource\":\"{oid}\"}}"),
        ] {
            assert_eq!(
                assignment_base_from_input(&rendered).as_deref(),
                Some(oid),
                "{rendered}"
            );
        }
        assert_eq!(
            assignment_base_from_input("CommitReview { commit = \"abc\" }"),
            None
        );
        // A longer hex run (a content digest) is not a Git OID.
        let digest = "a".repeat(64);
        assert_eq!(
            assignment_base_from_input(&format!("taskSource = {digest}")),
            None
        );
    }
}
