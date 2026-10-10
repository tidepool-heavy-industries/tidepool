use super::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Expr {
    Ready,
    Leaf(u8, bool),
    All(Box<Expr>, Box<Expr>),
    Either(Box<Expr>, Box<Expr>),
}

fn expressions() -> impl Strategy<Value = Expr> {
    prop_oneof![
        Just(Expr::Ready),
        (0u8..4, any::<bool>()).prop_map(|(key, settled)| Expr::Leaf(key, settled))
    ]
    .prop_recursive(4, 48, 2, |child| {
        prop_oneof![
            (child.clone(), child.clone())
                .prop_map(|(left, right)| Expr::All(Box::new(left), Box::new(right))),
            (child.clone(), child)
                .prop_map(|(left, right)| Expr::Either(Box::new(left), Box::new(right))),
        ]
    })
}

#[derive(Clone)]
enum RefResult {
    Success(Decision),
    Failure(RequestId, ResponseFailure),
}
struct Reference {
    index: usize,
    kind: RefKind,
    captured: Option<RefResult>,
}
enum RefKind {
    Ready,
    Leaf(u8, bool),
    All(Box<Reference>, Box<Reference>),
    Either(Box<Reference>, Box<Reference>),
}

fn construct(expr: &Expr, nodes: &mut Vec<Node<(u8, bool)>>) -> Reference {
    let (kind, node) = match expr {
        Expr::Ready => (RefKind::Ready, Node::Ready),
        Expr::Leaf(key, settled) => (RefKind::Leaf(*key, *settled), Node::Leaf((*key, *settled))),
        Expr::All(left, right) => {
            let left = construct(left, nodes);
            let right = construct(right, nodes);
            let node = Node::All(left.index, right.index);
            (RefKind::All(Box::new(left), Box::new(right)), node)
        }
        Expr::Either(left, right) => {
            let left = construct(left, nodes);
            let right = construct(right, nodes);
            let node = Node::Either(left.index, right.index);
            (RefKind::Either(Box::new(left), Box::new(right)), node)
        }
    };
    let index = nodes.len();
    nodes.push(node);
    Reference {
        index,
        kind,
        captured: None,
    }
}

/// Independent recursive observation of primary facts. Each node keeps its
/// first terminal observation; no production plan evaluation or node state is
/// used to construct expected values.
impl Reference {
    fn observe(&mut self, facts: &[Option<Result<(), ResponseFailure>>]) -> Option<RefResult> {
        if let Some(captured) = &self.captured {
            return Some(captured.clone());
        }
        let outcome = match &mut self.kind {
            RefKind::Ready => Some(RefResult::Success(Decision::default())),
            RefKind::Leaf(key, settled) => match &facts[*key as usize] {
                None => None,
                Some(Ok(())) => Some(RefResult::Success(Decision {
                    leaves: vec![(self.index, None)],
                    choices: vec![],
                })),
                Some(Err(failure)) if *settled => Some(RefResult::Success(Decision {
                    leaves: vec![(self.index, Some(failure.clone()))],
                    choices: vec![],
                })),
                Some(Err(failure)) => Some(RefResult::Failure(
                    RequestId(*key as u64 + 1),
                    failure.clone(),
                )),
            },
            RefKind::All(left, right) => {
                let left = left.observe(facts);
                let right = right.observe(facts);
                match (left, right) {
                    (Some(RefResult::Failure(id, error)), _)
                    | (_, Some(RefResult::Failure(id, error))) => {
                        Some(RefResult::Failure(id, error))
                    }
                    (Some(RefResult::Success(mut left)), Some(RefResult::Success(right))) => {
                        left.leaves.extend(right.leaves);
                        left.choices.extend(right.choices);
                        Some(RefResult::Success(left))
                    }
                    _ => None,
                }
            }
            RefKind::Either(left, right) => {
                let left = left.observe(facts);
                let right = right.observe(facts);
                let selected = if let Some(left) = left {
                    Some((true, left))
                } else {
                    right.map(|right| (false, right))
                };
                selected.map(|(choice, value)| match value {
                    RefResult::Failure(id, error) => RefResult::Failure(id, error),
                    RefResult::Success(mut decision) => {
                        decision.choices.insert(0, (self.index, choice));
                        RefResult::Success(decision)
                    }
                })
            }
        };
        self.captured = outcome.clone();
        outcome
    }
}

fn outcome(value: Option<RefResult>) -> Option<Outcome> {
    value.map(|value| match value {
        RefResult::Success(decision) => Outcome::Ready(decision),
        RefResult::Failure(id, error) => Outcome::Failed(id, error),
    })
}
fn observe(
    plan: &Plan<(u8, bool)>,
    evaluation: &mut Evaluation,
    facts: &[Option<Result<(), ResponseFailure>>],
) -> Option<Outcome> {
    evaluation.advance(plan, |_, &(key, settled)| match &facts[key as usize] {
        None => LeafState::Pending,
        Some(Ok(())) => LeafState::Ready,
        Some(Err(error)) if settled => LeafState::SettledFailure(error.clone()),
        Some(Err(error)) => LeafState::Failed(RequestId(key as u64 + 1), error.clone()),
    })
}
fn publish(facts: &mut [Option<Result<(), ResponseFailure>>], key: u8, operation: u8) {
    let fact = &mut facts[key as usize];
    if operation == 2 {
        *fact = Some(Err(ResponseFailure::Released));
    } else if fact.is_none() {
        *fact = Some(if operation == 0 {
            Ok(())
        } else {
            Err(ResponseFailure::Cancelled)
        });
    }
}

fn delayed_registry_history(
    order: [usize; 3],
    failed: [bool; 3],
    allow_failure: [bool; 3],
    inspect: [bool; 3],
) {
    use crate::request::{
        ReadinessDependency, RequestRegistry, WatchObservation, WatchRequirement,
    };
    use crate::{ActorId, ActorRef};

    let registry = RequestRegistry::default();
    let owner = ActorRef::first(ActorId(1));
    let targets = [2, 3, 4].map(|id| ActorRef::first(ActorId(id)));
    let requests = targets.map(|target| {
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        request
    });
    let expression = Expr::All(
        Box::new(Expr::Either(
            Box::new(Expr::Leaf(0, allow_failure[0])),
            Box::new(Expr::Leaf(1, allow_failure[1])),
        )),
        Box::new(Expr::Leaf(2, allow_failure[2])),
    );
    let mut nodes = Vec::new();
    let mut reference = construct(&expression, &mut nodes);
    let nodes = nodes
        .into_iter()
        .map(|node| match node {
            Node::Ready => Node::Ready,
            Node::Leaf((key, settled)) => Node::Leaf(ReadinessDependency::Request(
                requests[key as usize],
                WatchRequirement::Response {
                    allow_failure: settled,
                },
            )),
            Node::All(left, right) => Node::All(left, right),
            Node::Either(left, right) => Node::Either(left, right),
        })
        .collect();
    let watch = registry
        .register_watch_plan(
            owner,
            "delayed history".into(),
            Plan::checked(nodes, reference.index).unwrap(),
        )
        .unwrap()
        .0;
    let mut facts = vec![None; 3];
    let _ = reference.observe(&facts);
    let check = |expected: Option<RefResult>| match (
        outcome(expected),
        registry.observe_watch(owner, watch).unwrap(),
    ) {
        (None, WatchObservation::Pending(_)) => {}
        (Some(Outcome::Ready(expected)), WatchObservation::Ready(actual)) => {
            assert_eq!(actual, expected)
        }
        (
            Some(Outcome::Failed(key, expected)),
            WatchObservation::Unavailable { request, failure },
        ) => {
            assert_eq!(request, requests[key.0 as usize - 1]);
            assert_eq!(failure, expected);
        }
        (expected, actual) => {
            panic!("delayed watch outcome: expected {expected:?}, actual {actual:?}")
        }
    };
    for (step, key) in order.into_iter().enumerate() {
        if failed[key] {
            registry.mark_target_unavailable(owner, requests[key]);
            facts[key] = Some(Err(ResponseFailure::TargetUnavailable));
        } else {
            let mut reply_claim_requests_key =
                Some(registry.begin_reply(targets[key], requests[key]).unwrap());
            crate::request::test_support::complete_optional_reply(
                &registry,
                &mut reply_claim_requests_key,
                None,
            );
            facts[key] = Some(Ok(()));
        }
        // The oracle records transitions independently of public reads. The
        // production registry must advance its own evaluator at settlement.
        let expected = reference.observe(&facts);
        if inspect[step] {
            check(expected);
        }
    }
    let expected = reference.observe(&facts);
    check(expected.clone());
    check(expected);
}

#[test]
fn unobserved_right_choice_survives_later_left_completion() {
    // At the first public read both alternatives are ready. Recomputing from
    // current facts would choose left; the actual transition selected right.
    delayed_registry_history([1, 0, 2], [false; 3], [false; 3], [false; 3]);
}

fn delayed_observation_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 192;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

proptest! {
    #![proptest_config(delayed_observation_config())]
    #[test]
    fn delayed_public_observations_match_transition_latched_history(
        priorities in any::<[u16; 3]>(),
        failed in any::<[bool; 3]>(),
        allow_failure in any::<[bool; 3]>(),
        inspect in any::<[bool; 3]>(),
    ) {
        let mut order = [0, 1, 2];
        order.sort_by_key(|&key| (priorities[key], key));
        delayed_registry_history(order, failed, allow_failure, inspect);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]
    #[test]
    fn nested_histories_match_recursive_latched_oracle(
        expr in expressions(),
        events in prop::collection::vec((0u8..4,0u8..3),0..40),
        registration in 0usize..40,
    ) {
        let mut nodes=Vec::new();let mut reference=construct(&expr,&mut nodes);
        let plan=Plan::checked(nodes,reference.index).unwrap();
        let mut facts=vec![None;4];
        let registration=registration.min(events.len());
        for &(key,event) in &events[..registration] {publish(&mut facts,key,event);}
        let mut evaluation=Evaluation::new(&plan);
        prop_assert_eq!(observe(&plan,&mut evaluation,&facts),outcome(reference.observe(&facts)));
        for &(key,event) in &events[registration..] {
            publish(&mut facts,key,event);
            prop_assert_eq!(observe(&plan,&mut evaluation,&facts),outcome(reference.observe(&facts)),"expr={:?},events={:?}",expr,events);
        }
    }
}

#[test]
fn nested_choice_latches_before_parent_finishes_and_ignores_loser_release() {
    let plan = Plan::checked(
        vec![
            Node::Leaf((0, false)),
            Node::Leaf((1, false)),
            Node::Either(0, 1),
            Node::Leaf((2, false)),
            Node::All(2, 3),
        ],
        4,
    )
    .unwrap();
    let mut evaluation = Evaluation::new(&plan);
    let mut facts = vec![None; 4];
    publish(&mut facts, 1, 0);
    assert_eq!(observe(&plan, &mut evaluation, &facts), None);
    publish(&mut facts, 0, 2);
    assert_eq!(observe(&plan, &mut evaluation, &facts), None);
    publish(&mut facts, 2, 0);
    assert_eq!(
        observe(&plan, &mut evaluation, &facts),
        Some(Outcome::Ready(Decision {
            leaves: vec![(1, None), (3, None)],
            choices: vec![(2, false)]
        }))
    );
    publish(&mut facts, 1, 2);
    assert_eq!(
        observe(&plan, &mut evaluation, &facts),
        Some(Outcome::Ready(Decision {
            leaves: vec![(1, None), (3, None)],
            choices: vec![(2, false)]
        }))
    );
}

#[test]
fn initial_ties_prefer_left_and_first_failure_is_terminal() {
    let plan = Plan::checked(
        vec![
            Node::Leaf((0, false)),
            Node::Leaf((1, false)),
            Node::Either(0, 1),
        ],
        2,
    )
    .unwrap();
    let mut both = Evaluation::new(&plan);
    let facts = vec![
        Some(Err(ResponseFailure::Cancelled)),
        Some(Ok(())),
        None,
        None,
    ];
    assert_eq!(
        observe(&plan, &mut both, &facts),
        Some(Outcome::Failed(RequestId(1), ResponseFailure::Cancelled))
    );
    let mut right_first = Evaluation::new(&plan);
    let mut facts = vec![None, Some(Ok(())), None, None];
    let ready = observe(&plan, &mut right_first, &facts);
    facts[0] = Some(Err(ResponseFailure::Cancelled));
    assert_eq!(observe(&plan, &mut right_first, &facts), ready);
}

#[test]
fn shared_graph_stays_linear_and_invalid_graphs_are_refused() {
    let mut nodes = vec![Node::Ready];
    for index in 1..1024 {
        nodes.push(if index % 2 == 0 {
            Node::All(index - 1, index - 1)
        } else {
            Node::Either(index - 1, index - 1)
        });
    }
    let plan = Plan::<()>::checked(nodes, 1023).unwrap();
    assert_eq!(plan.nodes.len(), 1024);
    assert!(matches!(
        Evaluation::new(&plan).advance(&plan, |_, _| LeafState::Pending),
        Some(Outcome::Ready(_))
    ));
    for (nodes, root) in [
        (vec![Node::All(0, 0)], 0),
        (vec![Node::Ready, Node::Ready], 0),
        (vec![Node::Ready], 1),
        (vec![], 0),
    ] {
        assert_eq!(
            Plan::<()>::checked(nodes, root),
            Err(ReplyError::InvalidReadiness)
        );
    }
}
