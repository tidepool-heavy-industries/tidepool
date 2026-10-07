//! Branch-local readiness and immutable decisions owned by a watch.

use super::{ReplyError, RequestId, ResponseFailure};

const MAX_NODES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node<D> {
    Ready,
    Leaf(D),
    All(usize, usize),
    Either(usize, usize),
}

/// References are topological: each edge points to an earlier node. Every
/// node is reachable from the root; unused leaves cannot acquire authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan<D> {
    pub nodes: Vec<Node<D>>,
    pub root: usize,
}

impl<D> Plan<D> {
    pub fn checked(nodes: Vec<Node<D>>, root: usize) -> Result<Self, ReplyError> {
        if nodes.is_empty() || nodes.len() > MAX_NODES || root >= nodes.len() {
            return Err(ReplyError::InvalidReadiness);
        }
        for (index, node) in nodes.iter().enumerate() {
            if let Node::All(left, right) | Node::Either(left, right) = node {
                if *left >= index || *right >= index {
                    return Err(ReplyError::InvalidReadiness);
                }
            }
        }
        let mut reachable = vec![false; nodes.len()];
        let mut stack = vec![root];
        while let Some(index) = stack.pop() {
            if std::mem::replace(&mut reachable[index], true) {
                continue;
            }
            if let Node::All(left, right) | Node::Either(left, right) = &nodes[index] {
                stack.extend([*left, *right]);
            }
        }
        if reachable.iter().any(|reachable| !reachable) {
            return Err(ReplyError::InvalidReadiness);
        }
        Ok(Self { nodes, root })
    }

    pub fn leaves(&self) -> impl Iterator<Item = (usize, &D)> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| match node {
                Node::Leaf(dependency) => Some((index, dependency)),
                _ => None,
            })
    }

    pub fn try_map<E, F>(self, mut map: F) -> Result<Plan<E>, ReplyError>
    where
        F: FnMut(D) -> Result<E, ReplyError>,
    {
        let mut nodes = Vec::with_capacity(self.nodes.len());
        for node in self.nodes {
            nodes.push(match node {
                Node::Ready => Node::Ready,
                Node::Leaf(dependency) => Node::Leaf(map(dependency)?),
                Node::All(left, right) => Node::All(left, right),
                Node::Either(left, right) => Node::Either(left, right),
            });
        }
        Ok(Plan {
            nodes,
            root: self.root,
        })
    }
}

/// A decision contains only selected leaves and selected sum branches.
/// Successful values remain in Haskell cells; progress snapshots stay owned
/// by the watch. Node indices bind this decision to its admitted plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decision {
    pub leaves: Vec<(usize, Option<ResponseFailure>)>,
    /// true selects the left child.
    pub choices: Vec<(usize, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeafState {
    Pending,
    Ready,
    /// A settlement projection captures an unavailable outcome as a value.
    SettledFailure(ResponseFailure),
    Failed(RequestId, ResponseFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Pending,
    Ready,
    SettledFailure(ResponseFailure),
    Failed(RequestId, ResponseFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ready(Decision),
    Failed(RequestId, ResponseFailure),
}

pub(crate) struct Evaluation {
    states: Vec<State>,
    choices: Vec<Option<bool>>,
}

impl Evaluation {
    pub fn new<D>(plan: &Plan<D>) -> Self {
        Self {
            states: vec![State::Pending; plan.nodes.len()],
            choices: vec![None; plan.nodes.len()],
        }
    }

    /// Called under the request-state lock for every transition. Terminal
    /// nodes never change; nested choices therefore latch before their parent
    /// finishes, and later release cannot revoke a captured fact.
    pub fn advance<D>(
        &mut self,
        plan: &Plan<D>,
        mut leaf: impl FnMut(usize, &D) -> LeafState,
    ) -> Option<Outcome> {
        for (index, node) in plan.nodes.iter().enumerate() {
            if self.states[index] != State::Pending {
                continue;
            }
            self.states[index] = match node {
                Node::Ready => State::Ready,
                Node::Leaf(dependency) => match leaf(index, dependency) {
                    LeafState::Pending => State::Pending,
                    LeafState::Ready => State::Ready,
                    LeafState::SettledFailure(failure) => State::SettledFailure(failure),
                    LeafState::Failed(request, failure) => State::Failed(request, failure),
                },
                Node::All(left, right) => match (&self.states[*left], &self.states[*right]) {
                    (State::Failed(request, failure), _) | (_, State::Failed(request, failure)) => {
                        State::Failed(*request, failure.clone())
                    }
                    (State::Pending, _) | (_, State::Pending) => State::Pending,
                    _ => State::Ready,
                },
                Node::Either(left, right) => {
                    let selected = if self.states[*left] != State::Pending {
                        Some(true)
                    } else if self.states[*right] != State::Pending {
                        Some(false)
                    } else {
                        None
                    };
                    self.choices[index] = selected;
                    match selected {
                        Some(left_selected) => {
                            match &self.states[if left_selected { *left } else { *right }] {
                                State::Failed(request, failure) => {
                                    State::Failed(*request, failure.clone())
                                }
                                _ => State::Ready,
                            }
                        }
                        None => State::Pending,
                    }
                }
            };
        }
        match &self.states[plan.root] {
            State::Pending => None,
            State::Failed(request, failure) => Some(Outcome::Failed(*request, failure.clone())),
            _ => Some(Outcome::Ready(self.decision(plan))),
        }
    }

    fn decision<D>(&self, plan: &Plan<D>) -> Decision {
        let mut decision = Decision::default();
        let mut visited = vec![false; plan.nodes.len()];
        let mut stack = vec![plan.root];
        while let Some(index) = stack.pop() {
            if std::mem::replace(&mut visited[index], true) {
                continue;
            }
            match &plan.nodes[index] {
                Node::Ready => {}
                Node::Leaf(_) => decision.leaves.push((
                    index,
                    match &self.states[index] {
                        State::SettledFailure(failure) => Some(failure.clone()),
                        State::Ready => None,
                        _ => unreachable!("successful decision contains an unsettled leaf"),
                    },
                )),
                Node::All(left, right) => {
                    stack.push(*right);
                    stack.push(*left);
                }
                Node::Either(left, right) => {
                    let choice = self.choices[index].expect("terminal choice has a selection");
                    decision.choices.push((index, choice));
                    stack.push(if choice { *left } else { *right });
                }
            }
        }
        decision
    }
}

#[cfg(test)]
mod tests;
