//! Journal histories; hosted acceptance separately exercises execution and
//! provider-issued references. These data fixtures grant no runtime authority.

use crate::initial_context::{
    ContextSection, InitialContextEvent, InitialContextIdentity, InitialContextObservation,
    InitialContextPublication, InitialContextState, RetainedContextSeed,
};
use crate::{
    ActorDescriptor, ActorId, ActorPlacement, ActorRecoveryJournal, ActorRef,
    ApplicationConversation, Incarnation,
};
use proptest::prelude::*;
use tidepool_atomic_write::DirectoryAnchor;
use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
use tidepool_repr::SessionId;
use tidepool_runtime::session::WorkbenchExecutionId;

#[derive(Clone, Debug)]
enum Step {
    Begin,
    Skip(bool),
    Retain,
    Publish(bool),
    Fail(bool),
    Inspect,
    Reopen,
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            Just(Step::Begin),
            any::<bool>().prop_map(Step::Skip),
            Just(Step::Retain),
            any::<bool>().prop_map(Step::Publish),
            any::<bool>().prop_map(Step::Fail),
            Just(Step::Inspect),
            Just(Step::Reopen),
        ],
        0..48,
    )
}

#[derive(Clone, Debug)]
enum Facts {
    None,
    Skipped(bool),
    Begun,
    Returned,
    Published,
    Failed(bool),
}

struct History {
    directory: tempfile::TempDir,
    anchor: DirectoryAnchor,
    journal: ActorRecoveryJournal,
    actor: ActorRef,
    caller: Option<String>,
    result: RetainedContextSeed,
    identity: InitialContextIdentity,
    publication: InitialContextPublication,
    facts: Facts,
}

impl History {
    fn new(caller: Option<String>, output: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl").unwrap();
        let actor = ActorRef::first(ActorId(7));
        journal
            .admit(
                actor,
                &ActorDescriptor::new(
                    "initial-context-journal",
                    ActorPlacement {
                        session: SessionId(1),
                        lexical_scope: ScopeId(1),
                        resource_scope: RealmId(1),
                    },
                ),
                &[],
            )
            .unwrap();
        let identity = InitialContextIdentity {
            operation: WorkbenchExecutionId::from_digest([1; 16]),
            actor,
            spec_install: 19,
            spec_revision: Some("source-revision".into()),
            spec_entry: Some("AgentSpec.agentSpec".into()),
            spec_source: "compiled-source-identity".into(),
        };
        let result = RetainedContextSeed {
            sections: if output {
                vec![ContextSection {
                    label: "memory".into(),
                    source: Some("memory.txt".into()),
                    body: "runtime-observation".into(),
                }]
            } else {
                vec![]
            },
            rendered: if output {
                "rendered-observation".into()
            } else {
                String::new()
            },
        };
        let publication = InitialContextPublication {
            conversation: ApplicationConversation::Embedded {
                run: "run".into(),
                agent_path: "/root".into(),
                incarnation: Incarnation::FIRST.0.to_string(),
            },
            request: "publication-request".into(),
            caller_reference: caller.as_ref().map(|_| "caller-reference".into()),
            authored_reference: output.then(|| "authored-reference".into()),
        };
        Self {
            directory,
            anchor,
            journal,
            actor,
            caller,
            result,
            identity,
            publication,
            facts: Facts::None,
        }
    }

    fn expected(&self) -> Option<InitialContextObservation> {
        let (identity, state, result, publication, receipts) = match self.facts {
            Facts::None => return None,
            Facts::Skipped(captured) => (
                None,
                if captured {
                    InitialContextState::Captured
                } else {
                    InitialContextState::AbsentHook
                },
                None,
                None,
                serde_json::Value::Null,
            ),
            Facts::Begun => (
                Some(self.identity.clone()),
                InitialContextState::Preparing,
                None,
                None,
                serde_json::Value::Null,
            ),
            Facts::Returned => (
                Some(self.identity.clone()),
                InitialContextState::ResultRetained,
                Some(self.result.clone()),
                None,
                serde_json::json!(["preparation-receipt"]),
            ),
            Facts::Published => (
                Some(self.identity.clone()),
                InitialContextState::Published,
                Some(self.result.clone()),
                Some(self.publication.clone()),
                serde_json::json!(["preparation-receipt"]),
            ),
            Facts::Failed(cancelled) => (
                Some(self.identity.clone()),
                if cancelled {
                    InitialContextState::Cancelled
                } else {
                    InitialContextState::Failed {
                        detail: "preparation failed".into(),
                    }
                },
                None,
                None,
                serde_json::json!(["partial-effect-receipt"]),
            ),
        };
        Some(InitialContextObservation {
            identity,
            caller_seed: self.caller.clone(),
            state,
            result,
            publication,
            receipts,
        })
    }

    fn run(&mut self, step: &Step) -> Result<(), TestCaseError> {
        let before = std::fs::read(self.directory.path().join("actors.jsonl")).unwrap();
        let (event, accepted, next, changes) = match step {
            Step::Inspect => {
                prop_assert_eq!(
                    self.journal.initial_context(self.actor).unwrap(),
                    self.expected()
                );
                return Ok(());
            }
            Step::Reopen => {
                self.journal =
                    ActorRecoveryJournal::open_existing(&self.anchor, "actors.jsonl").unwrap();
                prop_assert_eq!(
                    self.journal.initial_context(self.actor).unwrap(),
                    self.expected()
                );
                prop_assert_eq!(
                    std::fs::read(self.directory.path().join("actors.jsonl")).unwrap(),
                    before
                );
                return Ok(());
            }
            Step::Begin => (
                InitialContextEvent::Begin {
                    identity: self.identity.clone(),
                    caller_seed: self.caller.clone(),
                },
                matches!(self.facts, Facts::None),
                Facts::Begun,
                true,
            ),
            Step::Skip(captured) => (
                InitialContextEvent::Skip {
                    captured: *captured,
                    caller_seed: self.caller.clone(),
                },
                matches!(self.facts, Facts::None),
                Facts::Skipped(*captured),
                true,
            ),
            Step::Retain => (
                InitialContextEvent::Retain {
                    result: self.result.clone(),
                    receipts: serde_json::json!(["preparation-receipt"]),
                },
                matches!(self.facts, Facts::Begun),
                Facts::Returned,
                true,
            ),
            Step::Fail(cancelled) => (
                InitialContextEvent::Fail {
                    detail: "preparation failed".into(),
                    cancelled: *cancelled,
                    receipts: serde_json::json!(["partial-effect-receipt"]),
                },
                matches!(self.facts, Facts::Begun),
                Facts::Failed(*cancelled),
                true,
            ),
            Step::Publish(valid) => {
                let mut publication = self.publication.clone();
                if !valid {
                    publication.authored_reference = if publication.authored_reference.is_some() {
                        None
                    } else {
                        Some("unexpected-reference".into())
                    };
                }
                let accepted = *valid && matches!(self.facts, Facts::Returned | Facts::Published);
                (
                    InitialContextEvent::Publish { publication },
                    accepted,
                    Facts::Published,
                    !matches!(self.facts, Facts::Published),
                )
            }
        };
        let actual = self.journal.record_initial_context(self.actor, event);
        prop_assert_eq!(
            actual.is_ok(),
            accepted,
            "step={:?}; facts={:?}",
            step,
            self.facts
        );
        if accepted {
            self.facts = next;
        }
        prop_assert_eq!(
            self.journal.initial_context(self.actor).unwrap(),
            self.expected()
        );
        if !accepted || !changes {
            prop_assert_eq!(
                std::fs::read(self.directory.path().join("actors.jsonl")).unwrap(),
                before
            );
        }
        Ok(())
    }
}

proptest! {
    #[test]
    fn generated_initial_context_journal_histories_preserve_accepted_facts(
        caller in prop_oneof![Just(None), Just(Some(String::new())), Just(Some("caller-seed".into()))],
        output in any::<bool>(),
        history in steps(),
    ) {
        let mut actual = History::new(caller, output);
        for step in history { actual.run(&step)?; }
    }
}

#[test]
fn retained_result_reopens_and_publication_retry_appends_nothing() {
    for caller in [None, Some(String::new()), Some("caller-seed".into())] {
        for output in [false, true] {
            let mut actual = History::new(caller.clone(), output);
            for step in [
                Step::Begin,
                Step::Retain,
                Step::Reopen,
                Step::Publish(true),
                Step::Reopen,
                Step::Publish(true),
                Step::Retain,
                Step::Begin,
                Step::Inspect,
            ] {
                actual.run(&step).unwrap();
            }
        }
    }
}

#[test]
fn failure_and_cancellation_do_not_become_empty_success_on_reopen() {
    for cancelled in [false, true] {
        let mut actual = History::new(None, false);
        for step in [
            Step::Begin,
            Step::Fail(cancelled),
            Step::Reopen,
            Step::Retain,
            Step::Publish(true),
            Step::Begin,
            Step::Inspect,
        ] {
            actual.run(&step).unwrap();
        }
    }
}
