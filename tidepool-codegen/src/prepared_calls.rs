//! Call/application planning for the prepared execution language.
//!
//! This module contains no emitter or heap policy. It turns the checked semantic
//! signature into an immutable plan which the native backend can lower through
//! the one `EntryAbi` owner.

use tidepool_repr::execution_schema::{RuntimeRep, Signature, ValueId};

use crate::entry_abi::EntryAbi;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CallPlanError {
    #[error("argument {index} has representation {actual:?}, expected {expected:?}")]
    Representation {
        index: usize,
        expected: RuntimeRep,
        actual: RuntimeRep,
    },
    #[error("PAP prefix exceeds target semantic arity")]
    PrefixOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationKind {
    Partial { remaining_semantic: usize },
    Exact,
    Oversaturated { pending_semantic: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallPlan {
    kind: ApplicationKind,
    consumed_semantic: usize,
    consumed_physical: Vec<usize>,
    pending_root_arguments: Vec<usize>,
}

impl CallPlan {
    pub fn kind(&self) -> &ApplicationKind {
        &self.kind
    }

    pub fn consumed_semantic(&self) -> usize {
        self.consumed_semantic
    }

    pub fn consumed_physical(&self) -> &[usize] {
        &self.consumed_physical
    }

    /// Semantic indices in the oversaturated suffix which must be installed in
    /// stable root slots before entry/forcing/allocation can collect.
    pub fn pending_root_arguments(&self) -> &[usize] {
        &self.pending_root_arguments
    }
}

pub fn plan_application(
    abi: &EntryAbi,
    supplied: &[RuntimeRep],
) -> Result<CallPlan, CallPlanError> {
    let expected = abi.semantic_arguments();
    for (index, (actual, wanted)) in supplied.iter().zip(expected).enumerate() {
        if actual != wanted {
            return Err(CallPlanError::Representation {
                index,
                expected: *wanted,
                actual: *actual,
            });
        }
    }

    let consumed_semantic = supplied.len().min(expected.len());
    let consumed_physical = expected
        .iter()
        .take(consumed_semantic)
        .enumerate()
        .filter_map(|(index, rep)| (*rep != RuntimeRep::Void).then_some(index))
        .collect();
    let pending_root_arguments = supplied
        .iter()
        .enumerate()
        .skip(expected.len())
        .filter_map(|(index, rep)| {
            matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef).then_some(index)
        })
        .collect();
    let kind = match supplied.len().cmp(&expected.len()) {
        std::cmp::Ordering::Less => ApplicationKind::Partial {
            remaining_semantic: expected.len() - supplied.len(),
        },
        std::cmp::Ordering::Equal => ApplicationKind::Exact,
        std::cmp::Ordering::Greater => ApplicationKind::Oversaturated {
            pending_semantic: supplied.len() - expected.len(),
        },
    };
    Ok(CallPlan {
        kind,
        consumed_semantic,
        consumed_physical,
        pending_root_arguments,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PapArgument {
    pub rep: RuntimeRep,
    pub bits: u128,
}

/// An immutable flat PAP prefix. `Void` arguments occupy semantic prefix
/// positions but no physical field; extending returns a new value and never
/// chains or mutates an existing PAP.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlatPap {
    target: ValueId,
    signature: Signature,
    prefix: Vec<PapArgument>,
}

impl FlatPap {
    pub fn new(target: ValueId, signature: Signature) -> Self {
        Self {
            target,
            signature,
            prefix: Vec::new(),
        }
    }

    pub fn target(&self) -> ValueId {
        self.target
    }

    pub fn prefix(&self) -> &[PapArgument] {
        &self.prefix
    }

    pub fn extend(&self, arguments: &[PapArgument]) -> Result<Self, CallPlanError> {
        let new_len = self
            .prefix
            .len()
            .checked_add(arguments.len())
            .ok_or(CallPlanError::PrefixOverflow)?;
        if new_len > self.signature.arguments.len() {
            return Err(CallPlanError::PrefixOverflow);
        }
        for (relative, argument) in arguments.iter().enumerate() {
            let index = self.prefix.len() + relative;
            let expected = self.signature.arguments[index];
            if argument.rep != expected {
                return Err(CallPlanError::Representation {
                    index,
                    expected,
                    actual: argument.rep,
                });
            }
        }
        let mut prefix = self.prefix.clone();
        prefix.extend_from_slice(arguments);
        Ok(Self {
            target: self.target,
            signature: self.signature.clone(),
            prefix,
        })
    }

    pub fn physical_prefix(&self) -> impl Iterator<Item = &PapArgument> {
        self.prefix
            .iter()
            .filter(|argument| argument.rep != RuntimeRep::Void)
    }

    pub fn remaining_semantic(&self) -> usize {
        self.signature.arguments.len() - self.prefix.len()
    }
}
