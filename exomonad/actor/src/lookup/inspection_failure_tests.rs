//! Bounded operation histories through the production lookup consumer.
use super::*;
use proptest::prelude::*;
use std::cell::RefCell;

#[derive(Clone, Copy, Debug)]
enum Failure {
    Source,
    Input,
    Worker,
    Cancelled,
    Io,
    Contract,
    Protocol,
    Message,
    WrongLength,
}

const FAILURES: [Failure; 9] = [
    Failure::Source,
    Failure::Input,
    Failure::Worker,
    Failure::Cancelled,
    Failure::Io,
    Failure::Contract,
    Failure::Protocol,
    Failure::Message,
    Failure::WrongLength,
];

fn fail(kind: Failure) -> Result<Vec<InspectionResult>, LookupInspectionError> {
    use tidepool_runtime::CompileError;
    let diagnostics = || {
        vec![tidepool_toolchain::diag::ExtractDiag {
            span: None,
            severity: tidepool_toolchain::diag::DiagnosticSeverity::Error,
            message: "same failure detail".into(),
        }]
    };
    Err(match kind {
        Failure::Source => {
            LookupInspectionError::Compiler(CompileError::Diagnostics(diagnostics()))
        }
        Failure::Input => {
            LookupInspectionError::Compiler(CompileError::InputRejected(diagnostics()))
        }
        Failure::Worker => {
            LookupInspectionError::Compiler(CompileError::WorkerFailure(diagnostics()))
        }
        Failure::Cancelled => LookupInspectionError::Compiler(CompileError::Io(
            std::io::Error::new(std::io::ErrorKind::Interrupted, "same failure detail"),
        )),
        Failure::Io => LookupInspectionError::Compiler(CompileError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "same failure detail",
        ))),
        Failure::Contract => LookupInspectionError::Compiler(CompileError::ExtractFailed(
            "same failure detail".into(),
        )),
        Failure::Protocol => LookupInspectionError::Compiler(CompileError::MalformedDiagnostics(
            "same failure detail".into(),
        )),
        Failure::Message => {
            LookupInspectionError::Message("Haskell compilation failed: same failure detail".into())
        }
        Failure::WrongLength => return Ok(vec![]),
    })
}

fn request(names: usize, references: usize, discover: bool) -> LookupRequest {
    LookupRequest {
        queries: (0..names)
            .map(|index| format!("q{index}"))
            .chain(std::iter::once("doc workbench".into()))
            .collect(),
        discover,
        expected_view: None,
        candidate_limit: 128,
        references: (0..references)
            .map(|index| LookupReference {
                module: format!("Project.Ref{index}"),
                name: "name".into(),
                namespace: LookupNamespace::Value,
            })
            .collect(),
    }
}

fn answer(query: &InspectionQuery) -> InspectionResult {
    match query {
        InspectionQuery::Info(name) => InspectionResult::NotFound {
            query: name.clone(),
        },
        InspectionQuery::Browse { module, expanded } => InspectionResult::Browse {
            module: module.clone(),
            expanded: *expanded,
            entries: vec![],
        },
        InspectionQuery::ScopeBrowse => InspectionResult::Browse {
            module: String::new(),
            expanded: true,
            entries: vec![],
        },
        other => panic!("unexpected fixture query: {other:?}"),
    }
}

fn run_history(
    names: usize,
    references: usize,
    failure: Failure,
    position: usize,
    discover: bool,
    initial_failure: bool,
) -> Result<(), TestCaseError> {
    let parts = names + references;
    let position = position % parts;
    let submitted = RefCell::new(Vec::new());
    let batch = execute(
        request(names, references, discover),
        "view".into(),
        "",
        &[],
        &[],
        crate::UsagePointerTable::default(),
        |queries| {
            let ordinal = submitted.borrow().len();
            submitted.borrow_mut().push(queries.to_vec());
            if ordinal == 0 {
                return fail(if initial_failure {
                    failure
                } else {
                    Failure::Source
                });
            }
            if ordinal == position + 1 {
                return fail(failure);
            }
            Ok(queries.iter().map(answer).collect())
        },
    );
    let source = matches!(failure, Failure::Source);
    let partitions_run = parts > 1 && (!initial_failure || source);
    // The oracle counts permitted submissions from the authored history, rather
    // than using the production failure classifier or partition implementation.
    let isolated_count = if !partitions_run {
        0
    } else if source {
        parts
    } else {
        position + 1
    };
    let missing_primary = partitions_run && source && (names > 1 || position >= names);
    let discovery_count = usize::from(discover && missing_primary);
    let expected_calls = 1 + isolated_count + discovery_count;
    prop_assert_eq!(
        submitted.borrow().len(),
        expected_calls,
        "names={} refs={} failure={:?} position={} discovery={} initial={}",
        names,
        references,
        failure,
        position,
        discover,
        initial_failure
    );
    prop_assert_eq!(submitted.borrow()[0].len(), parts);
    if partitions_run {
        for (index, queries) in submitted
            .borrow()
            .iter()
            .skip(1)
            .take(isolated_count)
            .enumerate()
        {
            prop_assert_eq!(queries.len(), 1);
            let expected = if index < names {
                InspectionQuery::Info(format!("q{index}"))
            } else {
                InspectionQuery::Browse {
                    module: format!("Project.Ref{}", index - names),
                    expanded: true,
                }
            };
            prop_assert_eq!(&queries[0], &expected);
        }
    }
    prop_assert_eq!(batch.results.len(), names + references + 1);
    prop_assert!(
        matches!(batch.results[names].outcome, LookupOutcome::Found(_, false)),
        "local documentation must survive inspection failure"
    );
    for index in 0..parts {
        let result_index = index + usize::from(index >= names);
        let rejected = !partitions_run
            || if source {
                index == position
            } else {
                index >= position
            };
        if rejected {
            prop_assert!(
                matches!(
                    batch.results[result_index].outcome,
                    LookupOutcome::Rejected(_)
                ),
                "result={} history={:?}",
                result_index,
                failure
            );
        } else {
            prop_assert!(
                matches!(
                    batch.results[result_index].outcome,
                    LookupOutcome::Missing(_, _)
                ),
                "result={} history={:?}",
                result_index,
                failure
            );
        }
    }
    let unavailable = if initial_failure {
        !source
    } else {
        partitions_run && !source
    };
    prop_assert_eq!(batch.issue.is_some(), unavailable);
    prop_assert!(batch.candidates.is_empty());
    Ok(())
}

#[test]
fn persistent_worker_failure_submits_one_batch_instead_of_query_retries() {
    for names in [1, 2, 8] {
        let calls = std::cell::Cell::new(0);
        let batch = execute(
            request(names, 2, true),
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| {
                calls.set(calls.get() + 1);
                fail(Failure::Worker)
            },
        );
        assert_eq!(calls.get(), 1);
        assert!(batch.issue.is_some());
        assert_eq!(batch.results.len(), names + 3);
        assert!(batch
            .results
            .iter()
            .enumerate()
            .all(|(index, result)| if index == names {
                matches!(result.outcome, LookupOutcome::Found(_, false))
            } else {
                matches!(result.outcome, LookupOutcome::Rejected(_))
            }));
    }
}

#[test]
fn typed_failure_histories_bound_submissions_and_preserve_local_results() {
    let mut histories = 0;
    for names in [1, 2, 8] {
        for references in [0, 2] {
            for failure in FAILURES {
                for position in [0, names + references - 1] {
                    for discover in [false, true] {
                        run_history(names, references, failure, position, discover, false).unwrap();
                        histories += 1;
                        if !matches!(failure, Failure::Source) {
                            run_history(names, references, failure, position, discover, true)
                                .unwrap();
                            histories += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(histories, 408);
    eprintln!("lookup_failure_histories: histories={histories} failure_partitions=9 primary_sizes=3 reference_sizes=2 discovery_partitions=2");
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    config.cases = 256;
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn generated_failure_histories_match_submission_and_result_oracle(
        names in 1usize..9, references in 0usize..4, kind in 0usize..FAILURES.len(),
        position in 0usize..12, discover in any::<bool>(), initial in any::<bool>(),
    ) {
        // Initial source rejection is the decomposition trigger; all other
        // initial failures must remain one submission regardless of batch size.
        run_history(names, references, FAILURES[kind], position, discover, initial)?;
    }
}

#[test]
fn malformed_discovery_preserves_primary_answer_without_retry() {
    let submitted = RefCell::new(Vec::new());
    let batch = execute(
        request(2, 0, true),
        "view".into(),
        "",
        &[],
        &[],
        crate::UsagePointerTable::default(),
        |queries| {
            let ordinal = submitted.borrow().len();
            submitted.borrow_mut().push(queries.to_vec());
            if ordinal == 0 {
                Ok(queries.iter().map(answer).collect())
            } else {
                Ok(vec![])
            }
        },
    );
    assert_eq!(submitted.borrow().len(), 2);
    assert!(batch.issue.is_some());
    assert!(batch.results[..2]
        .iter()
        .all(|result| matches!(result.outcome, LookupOutcome::Missing(_, _))));
    assert!(matches!(
        batch.results[2].outcome,
        LookupOutcome::Found(_, false)
    ));
}
