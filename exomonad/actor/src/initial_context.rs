//! Actor-owned preparation and publication facts. The resident continuation
//! and effect journal remain with the existing workbench execution owner.

use crate::{ActorRef, ApplicationConversation};
use serde::{Deserialize, Serialize};
use tidepool_runtime::session::WorkbenchExecutionId;

pub(crate) const INITIAL_CONTEXT_SLOT: &str = "initialContext";
pub(crate) const INITIAL_CONTEXT_ENTRY: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialContextIdentity {
    pub operation: WorkbenchExecutionId,
    pub actor: ActorRef,
    pub spec_install: u64,
    pub spec_revision: Option<String>,
    pub spec_entry: Option<String>,
    pub spec_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSection {
    pub label: String,
    pub source: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedContextSeed {
    pub sections: Vec<ContextSection>,
    pub rendered: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialContextPublication {
    pub conversation: ApplicationConversation,
    pub request: String,
    pub caller_reference: Option<String>,
    pub authored_reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InitialContextState {
    AbsentHook,
    Captured,
    Preparing,
    ResultRetained,
    Published,
    Failed { detail: String },
    Cancelled,
    RecoveryRequired { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialContextObservation {
    pub identity: Option<InitialContextIdentity>,
    pub caller_seed: Option<String>,
    pub state: InitialContextState,
    pub result: Option<RetainedContextSeed>,
    pub publication: Option<InitialContextPublication>,
    pub receipts: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum InitialContextEvent {
    Begin {
        identity: InitialContextIdentity,
        caller_seed: Option<String>,
    },
    Skip {
        captured: bool,
        caller_seed: Option<String>,
    },
    Retain {
        result: RetainedContextSeed,
        receipts: serde_json::Value,
    },
    Publish {
        publication: InitialContextPublication,
    },
    Fail {
        detail: String,
        cancelled: bool,
        receipts: serde_json::Value,
    },
    Uncertain {
        detail: String,
        receipts: serde_json::Value,
    },
}

/// Validate before appending. Replaying uses the same transition owner.
pub(crate) fn apply_event(
    current: &mut Option<InitialContextObservation>,
    event: InitialContextEvent,
) -> std::io::Result<()> {
    let refuse =
        || std::io::Error::other("initial context transition differs from retained lifecycle");
    match event {
        InitialContextEvent::Begin {
            identity,
            caller_seed,
        } if current.is_none() => {
            *current = Some(InitialContextObservation {
                identity: Some(identity),
                caller_seed,
                state: InitialContextState::Preparing,
                result: None,
                publication: None,
                receipts: serde_json::Value::Null,
            });
        }
        InitialContextEvent::Skip {
            captured,
            caller_seed,
        } if current.is_none() => {
            *current = Some(InitialContextObservation {
                identity: None,
                caller_seed,
                state: if captured {
                    InitialContextState::Captured
                } else {
                    InitialContextState::AbsentHook
                },
                result: None,
                publication: None,
                receipts: serde_json::Value::Null,
            });
        }
        InitialContextEvent::Retain { result, receipts } => {
            let current = current.as_mut().ok_or_else(refuse)?;
            if current.state != InitialContextState::Preparing {
                return Err(refuse());
            }
            current.result = Some(result);
            current.receipts = receipts;
            current.state = InitialContextState::ResultRetained;
        }
        InitialContextEvent::Publish { publication } => {
            let current = current.as_mut().ok_or_else(refuse)?;
            if current.state == InitialContextState::Published
                && current.publication.as_ref() == Some(&publication)
            {
                return Ok(());
            }
            if current.state != InitialContextState::ResultRetained {
                return Err(refuse());
            }
            let result = current.result.as_ref().ok_or_else(refuse)?;
            if result.rendered.is_empty() != publication.authored_reference.is_none()
                || current.caller_seed.is_some() != publication.caller_reference.is_some()
            {
                return Err(refuse());
            }
            current.publication = Some(publication);
            current.state = InitialContextState::Published;
        }
        InitialContextEvent::Fail {
            detail,
            cancelled,
            receipts,
        } => {
            let current = current.as_mut().ok_or_else(refuse)?;
            if current.state != InitialContextState::Preparing {
                return Err(refuse());
            }
            current.receipts = receipts;
            current.state = if cancelled {
                InitialContextState::Cancelled
            } else {
                InitialContextState::Failed { detail }
            };
        }
        InitialContextEvent::Uncertain { detail, receipts } => {
            let current = current.as_mut().ok_or_else(refuse)?;
            if current.state != InitialContextState::Preparing {
                return Err(refuse());
            }
            current.receipts = receipts;
            current.state = InitialContextState::RecoveryRequired { detail };
        }
        _ => return Err(refuse()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_result_cannot_be_replaced_or_prepared_again() {
        let mut state = None;
        let identity = InitialContextIdentity {
            operation: WorkbenchExecutionId::from_digest([1; 16]),
            actor: ActorRef {
                id: crate::ActorId(1),
                incarnation: crate::Incarnation::FIRST,
            },
            spec_install: 1,
            spec_revision: None,
            spec_entry: None,
            spec_source: "source".into(),
        };
        let begin = InitialContextEvent::Begin {
            identity,
            caller_seed: Some(String::new()),
        };
        apply_event(&mut state, begin.clone()).unwrap();
        let retained = InitialContextEvent::Retain {
            result: RetainedContextSeed {
                sections: vec![],
                rendered: String::new(),
            },
            receipts: serde_json::json!([]),
        };
        apply_event(&mut state, retained.clone()).unwrap();
        let before = state.clone();
        assert!(apply_event(&mut state, begin).is_err());
        assert!(apply_event(&mut state, retained).is_err());
        assert_eq!(state, before);
        let publication = InitialContextPublication {
            conversation: ApplicationConversation::Embedded {
                run: "run".into(),
                agent_path: "/root".into(),
                incarnation: "1".into(),
            },
            request: "request".into(),
            caller_reference: Some("caller".into()),
            authored_reference: None,
        };
        apply_event(
            &mut state,
            InitialContextEvent::Publish {
                publication: publication.clone(),
            },
        )
        .unwrap();
        let published = state.clone();
        apply_event(&mut state, InitialContextEvent::Publish { publication }).unwrap();
        assert_eq!(state, published);
    }
}
