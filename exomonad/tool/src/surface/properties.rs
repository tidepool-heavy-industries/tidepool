//! Declared compatibility at the HostedTool boundary, not actor installation
//! admission or provider schema finalization. Records have unique valid names
//! and object inputs; actor installation imposes additional policy constraints.

use super::{compare_surfaces, SurfaceChange, ToolField};
use crate::{
    ActorEffectKey, HostedTool, ToolDeclaration, ToolEffectKey, ToolImplementation, ToolKind,
    ToolScheduling,
};
use proptest::prelude::*;
use proptest::test_runner::{
    contextualize_config, Config, FileFailurePersistence, TestCaseError, TestRunner,
};
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Output {
    Absent,
    Null,
    Object(u8),
}

#[derive(Clone, Debug)]
struct Seed {
    description: u8,
    kind: u8,
    before_inference: bool,
    haskell: bool,
    effects: Vec<u8>,
    argument_type: u8,
    reversed_required: bool,
    output: Output,
    reversed_keys: bool,
}

#[derive(Clone, Debug)]
struct Record {
    id: usize,
    seed: Seed,
}

impl Record {
    fn name(&self) -> String {
        format!("tool_{}", self.id)
    }

    fn declaration(&self) -> ToolDeclaration {
        let seed = &self.seed;
        let kinds = [
            ToolKind::Call,
            ToolKind::Raw,
            ToolKind::Notify,
            ToolKind::Update,
            ToolKind::Finish,
        ];
        let keys = [
            ToolEffectKey::Actor(ActorEffectKey::Replies),
            ToolEffectKey::Actor(ActorEffectKey::Watches),
            ToolEffectKey::Actor(ActorEffectKey::Commands),
            ToolEffectKey::ContextReadWrite,
        ];
        ToolDeclaration {
            name: self.name(),
            description: format!("description {}", seed.description),
            input_schema: input(seed),
            output_schema: match seed.output {
                Output::Absent => None,
                Output::Null => Some(Value::Null),
                Output::Object(tag) => Some(object(
                    vec![
                        ("type", Value::String("object".into())),
                        ("title", Value::String(format!("result {tag}"))),
                    ],
                    seed.reversed_keys,
                )),
            },
            kind: kinds[usize::from(seed.kind)],
            schedule: if seed.before_inference {
                ToolScheduling::BeforeNextInference
            } else {
                ToolScheduling::Async
            },
            implementation: if seed.haskell {
                ToolImplementation::HaskellCell
            } else {
                ToolImplementation::ResidentHandler
            },
            effect_keys: seed
                .effects
                .iter()
                .map(|key| keys[usize::from(*key)])
                .collect(),
        }
    }
}

fn object(mut pairs: Vec<(&str, Value)>, reverse: bool) -> Value {
    if reverse {
        pairs.reverse();
    }
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.into(), value);
    }
    Value::Object(map)
}

fn input(seed: &Seed) -> Value {
    let types = ["string", "integer", "boolean"];
    let properties = object(
        vec![
            (
                "first",
                serde_json::json!({"type": types[usize::from(seed.argument_type)]}),
            ),
            ("second", serde_json::json!({"type": "string"})),
        ],
        seed.reversed_keys,
    );
    object(
        vec![
            ("type", Value::String("object".into())),
            ("properties", properties),
            (
                "required",
                if seed.reversed_required {
                    serde_json::json!(["second", "first"])
                } else {
                    serde_json::json!(["first", "second"])
                },
            ),
        ],
        seed.reversed_keys,
    )
}

fn effects(mask: u8, reverse: bool) -> Vec<u8> {
    let mut keys: Vec<_> = (0..4).filter(|key| mask & (1 << key) != 0).collect();
    if reverse {
        keys.reverse();
    }
    keys
}

fn outputs() -> impl Strategy<Value = Output> {
    prop_oneof![
        Just(Output::Absent),
        Just(Output::Null),
        (0_u8..3).prop_map(Output::Object)
    ]
}

fn seeds() -> impl Strategy<Value = Seed> {
    (
        0_u8..3,
        0_u8..5,
        any::<bool>(),
        any::<bool>(),
        0_u8..16,
        any::<bool>(),
        0_u8..3,
        any::<bool>(),
        outputs(),
        any::<bool>(),
    )
        .prop_map(
            |(
                description,
                kind,
                before_inference,
                haskell,
                mask,
                reverse_effects,
                argument_type,
                reversed_required,
                output,
                reversed_keys,
            )| Seed {
                description,
                kind,
                before_inference,
                haskell,
                effects: effects(mask, reverse_effects),
                argument_type,
                reversed_required,
                output,
                reversed_keys,
            },
        )
}

#[derive(Clone, Debug)]
enum Edit {
    Description(usize, u8),
    Kind(usize, u8),
    Scheduling(usize, bool),
    Implementation(usize, bool),
    Effects(usize, u8, bool),
    InputType(usize, u8),
    RequiredOrder(usize, bool),
    Output(usize, Output),
    ObjectKeyOrder(usize, bool),
    Move(usize, usize),
    Rename(usize),
}

fn edits() -> impl Strategy<Value = Edit> {
    prop_oneof![
        (0_usize..8, 0_u8..3).prop_map(|(at, value)| Edit::Description(at, value)),
        (0_usize..8, 0_u8..5).prop_map(|(at, value)| Edit::Kind(at, value)),
        (0_usize..8, any::<bool>()).prop_map(|(at, value)| Edit::Scheduling(at, value)),
        (0_usize..8, any::<bool>()).prop_map(|(at, value)| Edit::Implementation(at, value)),
        (0_usize..8, 0_u8..16, any::<bool>())
            .prop_map(|(at, value, reverse)| Edit::Effects(at, value, reverse)),
        (0_usize..8, 0_u8..3).prop_map(|(at, value)| Edit::InputType(at, value)),
        (0_usize..8, any::<bool>()).prop_map(|(at, value)| Edit::RequiredOrder(at, value)),
        (0_usize..8, outputs()).prop_map(|(at, value)| Edit::Output(at, value)),
        (0_usize..8, any::<bool>()).prop_map(|(at, value)| Edit::ObjectKeyOrder(at, value)),
        (0_usize..8, 0_usize..8).prop_map(|(from, to)| Edit::Move(from, to)),
        (0_usize..8).prop_map(Edit::Rename),
    ]
}

#[derive(Clone, Debug)]
struct History {
    initial: Vec<Seed>,
    additions: Vec<Seed>,
    edits: Vec<Edit>,
    remove_mask: u8,
    order: [u8; 8],
}

fn histories() -> impl Strategy<Value = History> {
    // All edits operate on a nonempty record. Removals come afterward, so
    // shrinking never turns an edit into a skipped or substituted operation.
    (
        prop::collection::vec(seeds(), 1..=5),
        prop::collection::vec(seeds(), 0..=3),
        prop::collection::vec(edits(), 0..=16),
        any::<u8>(),
        prop::array::uniform8(0_u8..8),
    )
        .prop_map(|(initial, additions, edits, remove_mask, order)| History {
            initial,
            additions,
            edits,
            remove_mask,
            order,
        })
}

// Facts use only independently generated scalar/slot identities. No production
// accessor, JSON comparison or schema canonicalizer supplies expected fields.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Fact {
    Added(String),
    Removed(String),
    Field(String, u8),
    Moved(String, usize, usize),
}

fn expected_fields(a: &Seed, b: &Seed) -> Vec<u8> {
    let raw_a = a.kind == 1;
    let raw_b = b.kind == 1;
    let input_a = (!raw_a).then_some((a.argument_type, a.reversed_required));
    let input_b = (!raw_b).then_some((b.argument_type, b.reversed_required));
    let output_a = (!raw_a)
        .then_some(&a.output)
        .filter(|slot| **slot != Output::Absent);
    let output_b = (!raw_b)
        .then_some(&b.output)
        .filter(|slot| **slot != Output::Absent);
    [
        a.description != b.description,
        a.kind != b.kind,
        a.before_inference != b.before_inference,
        a.haskell != b.haskell,
        a.effects != b.effects,
        input_a != input_b,
        output_a != output_b,
    ]
    .into_iter()
    .enumerate()
    .filter_map(|(index, changed)| changed.then_some(index as u8))
    .collect()
}

fn expected(active: &[Record], candidate: &[Record]) -> BTreeSet<Fact> {
    let a: BTreeMap<_, _> = active
        .iter()
        .enumerate()
        .map(|(at, row)| (row.name(), (at, &row.seed)))
        .collect();
    let b: BTreeMap<_, _> = candidate
        .iter()
        .enumerate()
        .map(|(at, row)| (row.name(), (at, &row.seed)))
        .collect();
    let names: BTreeSet<_> = a.keys().chain(b.keys()).cloned().collect();
    let mut facts = BTreeSet::new();
    for name in names {
        match (a.get(&name), b.get(&name)) {
            (Some(_), None) => {
                facts.insert(Fact::Removed(name));
            }
            (None, Some(_)) => {
                facts.insert(Fact::Added(name));
            }
            (Some((from, left)), Some((to, right))) => {
                for field in expected_fields(left, right) {
                    facts.insert(Fact::Field(name.clone(), field));
                }
                if from != to {
                    facts.insert(Fact::Moved(name, *from, *to));
                }
            }
            (None, None) => unreachable!(),
        }
    }
    facts
}

fn field_id(field: ToolField) -> u8 {
    match field {
        ToolField::Description => 0,
        ToolField::Kind => 1,
        ToolField::Scheduling => 2,
        ToolField::Implementation => 3,
        ToolField::EffectKeys => 4,
        ToolField::InputSchema => 5,
        ToolField::OutputSchema => 6,
        ToolField::SchemaRendering => 7,
    }
}

fn observed(changes: &[SurfaceChange]) -> Result<BTreeSet<Fact>, TestCaseError> {
    let mut facts = BTreeSet::new();
    let mut events = BTreeSet::new();
    for change in changes {
        let (name, event) = match change {
            SurfaceChange::Added { name } => {
                prop_assert!(facts.insert(Fact::Added(name.clone())));
                (name, 0)
            }
            SurfaceChange::Removed { name } => {
                prop_assert!(facts.insert(Fact::Removed(name.clone())));
                (name, 1)
            }
            SurfaceChange::Changed { name, fields } => {
                prop_assert!(!fields.is_empty());
                for field in fields {
                    prop_assert!(facts.insert(Fact::Field(name.clone(), field_id(*field))));
                }
                (name, 2)
            }
            SurfaceChange::Moved { name, from, to } => {
                prop_assert!(from != to);
                prop_assert!(facts.insert(Fact::Moved(name.clone(), *from, *to)));
                (name, 3)
            }
        };
        prop_assert!(events.insert((name, event)));
    }
    Ok(facts)
}

fn hosted(rows: &[Record]) -> Result<Vec<HostedTool>, TestCaseError> {
    let names: BTreeSet<_> = rows.iter().map(Record::name).collect();
    prop_assert_eq!(names.len(), rows.len());
    rows.iter()
        .map(|row| {
            let declaration = row.declaration();
            prop_assert_eq!(declaration.input_schema["type"].as_str(), Some("object"));
            let tool = HostedTool::try_from(declaration)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(matches!(tool, HostedTool::Custom(_)), row.seed.kind == 1);
            Ok(tool)
        })
        .collect()
}

#[derive(Default, Debug)]
struct Coverage {
    callbacks: usize,
    completed_histories: usize,
    comparisons: usize,
    changed_fields: [usize; 8],
    additions: usize,
    removals: usize,
    moves: usize,
    changed_and_moved: usize,
    equal_surfaces: usize,
    raw_schema_erasure: usize,
    none_null_output: usize,
    effect_order: usize,
    required_array_order: usize,
    object_key_order: usize,
}

fn check_pair(
    active: &[Record],
    candidate: &[Record],
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    coverage.comparisons += 1;
    let a = hosted(active)?;
    let b = hosted(candidate)?;
    let changes = compare_surfaces(&a, &b);
    let facts = observed(&changes)?;
    let model = expected(active, candidate);
    prop_assert_eq!(
        &facts,
        &model,
        "active={:?}, candidate={:?}",
        active,
        candidate
    );
    prop_assert!(compare_surfaces(&a, &a).is_empty());
    prop_assert!(compare_surfaces(&b, &b).is_empty());
    let inverse: BTreeSet<_> = model
        .iter()
        .map(|fact| match fact {
            Fact::Added(name) => Fact::Removed(name.clone()),
            Fact::Removed(name) => Fact::Added(name.clone()),
            Fact::Field(name, field) => Fact::Field(name.clone(), *field),
            Fact::Moved(name, from, to) => Fact::Moved(name.clone(), *to, *from),
        })
        .collect();
    prop_assert_eq!(observed(&compare_surfaces(&b, &a))?, inverse);

    // The public ordering contract is active rows first (Changed before Moved),
    // followed by additions in candidate order. Field membership is checked above.
    let active_positions: BTreeMap<_, _> = active
        .iter()
        .enumerate()
        .map(|(at, row)| (row.name(), at))
        .collect();
    let candidate_positions: BTreeMap<_, _> = candidate
        .iter()
        .enumerate()
        .map(|(at, row)| (row.name(), at))
        .collect();
    let mut previous = None;
    for change in &changes {
        let key = match change {
            SurfaceChange::Added { name } => (1, candidate_positions[name], 0),
            SurfaceChange::Moved { name, .. } => (0, active_positions[name], 1),
            _ => (0, active_positions[change.name()], 0),
        };
        prop_assert!(previous.is_none_or(|previous| previous < key));
        previous = Some(key);
    }

    coverage.equal_surfaces += usize::from(facts.is_empty());
    for fact in &facts {
        match fact {
            Fact::Added(_) => coverage.additions += 1,
            Fact::Removed(_) => coverage.removals += 1,
            Fact::Field(_, field) => coverage.changed_fields[usize::from(*field)] += 1,
            Fact::Moved(name, _, _) => {
                coverage.moves += 1;
                coverage.changed_and_moved += usize::from(
                    facts
                        .iter()
                        .any(|fact| matches!(fact, Fact::Field(changed, _) if changed == name)),
                );
            }
        }
    }
    for row in active {
        if let Some(other) = candidate.iter().find(|other| other.id == row.id) {
            let (x, y) = (&row.seed, &other.seed);
            if x.kind == 1
                && y.kind == 1
                && (x.argument_type != y.argument_type
                    || x.reversed_required != y.reversed_required
                    || x.output != y.output)
            {
                coverage.raw_schema_erasure += 1;
            }
            if x.kind != 1 && y.kind != 1 {
                coverage.none_null_output += usize::from(matches!(
                    (&x.output, &y.output),
                    (Output::Absent, Output::Null) | (Output::Null, Output::Absent)
                ));
                coverage.required_array_order +=
                    usize::from(x.reversed_required != y.reversed_required);
            }
            let mut xs = x.effects.clone();
            let mut ys = y.effects.clone();
            xs.sort_unstable();
            ys.sort_unstable();
            coverage.effect_order += usize::from(xs == ys && x.effects != y.effects);
            coverage.object_key_order += usize::from(x.reversed_keys != y.reversed_keys);
        }
    }
    Ok(())
}

fn apply(edit: &Edit, rows: &mut Vec<Record>, next_id: &mut usize) {
    let len = rows.len();
    match edit {
        Edit::Move(from, to) => {
            let row = rows.remove(from % len);
            rows.insert(to % len, row);
        }
        Edit::Rename(at) => {
            rows[at % len].id = *next_id;
            *next_id += 1;
        }
        Edit::Description(at, value) => rows[at % len].seed.description = *value,
        Edit::Kind(at, value) => rows[at % len].seed.kind = *value,
        Edit::Scheduling(at, value) => rows[at % len].seed.before_inference = *value,
        Edit::Implementation(at, value) => rows[at % len].seed.haskell = *value,
        Edit::Effects(at, mask, reverse) => rows[at % len].seed.effects = effects(*mask, *reverse),
        Edit::InputType(at, value) => rows[at % len].seed.argument_type = *value,
        Edit::RequiredOrder(at, value) => rows[at % len].seed.reversed_required = *value,
        Edit::Output(at, value) => rows[at % len].seed.output = value.clone(),
        Edit::ObjectKeyOrder(at, value) => rows[at % len].seed.reversed_keys = *value,
    }
}

fn replay(history: &History, coverage: &mut Coverage) -> Result<(), TestCaseError> {
    let active: Vec<_> = history
        .initial
        .iter()
        .cloned()
        .enumerate()
        .map(|(id, seed)| Record { id, seed })
        .collect();
    let mut candidate = active.clone();
    let mut next_id = active.len();
    check_pair(&active, &candidate, coverage)?;
    for seed in &history.additions {
        candidate.push(Record {
            id: next_id,
            seed: seed.clone(),
        });
        next_id += 1;
        check_pair(&active, &candidate, coverage)?;
    }
    for edit in &history.edits {
        apply(edit, &mut candidate, &mut next_id);
        check_pair(&active, &candidate, coverage)?;
    }
    candidate = candidate
        .into_iter()
        .enumerate()
        .filter_map(|(at, row)| (history.remove_mask & (1 << at) == 0).then_some(row))
        .collect();
    check_pair(&active, &candidate, coverage)?;
    candidate = {
        let mut ranked: Vec<_> = candidate.into_iter().enumerate().collect();
        ranked.sort_by_key(|(at, _)| (history.order[*at], *at));
        ranked.into_iter().map(|(_, row)| row).collect()
    };
    check_pair(&active, &candidate, coverage)?;
    coverage.completed_histories += 1;
    Ok(())
}

#[test]
fn declared_surface_histories_match_independent_field_model() {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::declared_surface_histories_match_independent_field_model"
    ));
    let resolved = config.clone();
    let coverage = RefCell::new(Coverage::default());
    let result = TestRunner::new(config).run(&histories(), |history| {
        let mut coverage = coverage.borrow_mut();
        coverage.callbacks += 1;
        replay(&history, &mut coverage)
    });
    eprintln!("surface resolved runner configuration: {resolved:#?}");
    eprintln!(
        "surface configured fresh cases: {}, maximum shrink iterations: {}",
        resolved.cases, resolved.max_shrink_iters
    );
    eprintln!(
        "declared surface observed coverage: {:#?}",
        coverage.borrow()
    );
    result.unwrap();
}

fn plain() -> Seed {
    Seed {
        description: 0,
        kind: 0,
        before_inference: false,
        haskell: false,
        effects: vec![0, 1],
        argument_type: 0,
        reversed_required: false,
        output: Output::Absent,
        reversed_keys: false,
    }
}

#[test]
fn guided_surface_contract_partitions_are_observed() {
    let function = Record {
        id: 0,
        seed: plain(),
    };
    let mut raw = Record {
        id: 1,
        seed: plain(),
    };
    raw.seed.kind = 1;
    let active = vec![function.clone(), raw.clone()];
    let mut coverage = Coverage::default();
    for edit in [
        Edit::Description(0, 1),
        Edit::Kind(0, 1),
        Edit::Kind(0, 2),
        Edit::Kind(1, 0),
        Edit::Kind(0, 3),
        Edit::Kind(0, 4),
        Edit::Scheduling(0, true),
        Edit::Implementation(0, true),
        Edit::Effects(0, 3, true),
        Edit::InputType(0, 1),
        Edit::RequiredOrder(0, true),
        Edit::Output(0, Output::Null),
        Edit::ObjectKeyOrder(0, true),
        Edit::InputType(1, 2),
        Edit::Output(1, Output::Object(1)),
        Edit::Move(0, 1),
        Edit::Rename(1),
    ] {
        let mut candidate = active.clone();
        let mut next_id = 2;
        apply(&edit, &mut candidate, &mut next_id);
        check_pair(&active, &candidate, &mut coverage).unwrap();
    }
    let mut candidate = vec![raw, function];
    candidate[1].seed.description = 2;
    check_pair(&active, &candidate, &mut coverage).unwrap();
    check_pair(&active, &[], &mut coverage).unwrap();
    check_pair(&[], &active, &mut coverage).unwrap();
    assert!(coverage.changed_fields[..7].iter().all(|count| *count > 0));
    assert!(coverage.additions > 0 && coverage.removals > 0 && coverage.moves > 0);
    assert!(coverage.changed_and_moved > 0 && coverage.equal_surfaces > 0);
    assert!(coverage.raw_schema_erasure > 0 && coverage.none_null_output > 0);
    assert!(
        coverage.effect_order > 0
            && coverage.required_array_order > 0
            && coverage.object_key_order > 0
    );
    // serde_json's declared package features normalize object insertion order.
    // Arrays remain exact values: no JSON Schema semantic equivalence is assumed.
    let forward = Record {
        id: 0,
        seed: plain(),
    }
    .declaration();
    let mut reverse_seed = plain();
    reverse_seed.reversed_keys = true;
    let reverse = Record {
        id: 0,
        seed: reverse_seed,
    }
    .declaration();
    assert_eq!(forward.input_schema, reverse.input_schema);
    assert_eq!(
        forward.input_schema.to_string(),
        reverse.input_schema.to_string()
    );
    assert_eq!(coverage.changed_fields[7], 0);
    eprintln!("surface deterministic support coverage: {coverage:#?}");
}
