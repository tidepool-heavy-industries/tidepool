//! Stage evidence for the prepared-STG semantic corpus, never Core execution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tidepool_bridge::shapes::unbox_char;
use tidepool_bridge::{FromCore, Value};
use tidepool_repr::DataConTable;
use tidepool_repr::Literal;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Expectation {
    Int(i64),
    Bool(bool),
    Char(char),
    Text(String),
    Float64Approx {
        expected: f64,
        absolute_tolerance: f64,
    },
    List(Vec<Expectation>),
    Tuple(Vec<Expectation>),
    Maybe(Option<Box<Expectation>>),
    EitherLeft(Box<Expectation>),
    EitherRight(Box<Expectation>),
    Error(ExpectedFailure),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedFailure {
    Blackhole,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Expectations {
    pub source_revision: String,
    pub expectations: BTreeMap<String, Expectation>,
}

/// Produced directly from GHC's prepared modules, not translated Core filenames.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectionManifest {
    pub version: u32,
    pub programs: Vec<ProjectionRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectionRecord {
    pub name: String,
    #[serde(flatten)]
    pub projection: ProjectionOutcome,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProjectionOutcome {
    Projected { artifact: String, identity: SourceIdentity },
    Rejected { reason: String },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SourceIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Projection,
    Validation,
    Admission,
    Compilation,
    Execution,
    Comparison,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed { reason: String },
    MissingExpectation,
    NotReached,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StageRecord {
    pub stage: Stage,
    pub outcome: Outcome,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProgramRecord {
    pub name: String,
    pub stages: Vec<StageRecord>,
}

impl ProgramRecord {
    /// Recording a later stage never implicitly marks earlier stages passed.
    pub fn new(name: String) -> Self {
        Self {
            name,
            stages: [
                Stage::Projection,
                Stage::Validation,
                Stage::Admission,
                Stage::Compilation,
                Stage::Execution,
                Stage::Comparison,
            ]
            .into_iter()
            .map(|stage| StageRecord {
                stage,
                outcome: Outcome::NotReached,
            })
            .collect(),
        }
    }

    pub fn record(&mut self, stage: Stage, outcome: Outcome) {
        self.stages
            .iter_mut()
            .find(|entry| entry.stage == stage)
            .expect("the fixed stage list contains every Stage")
            .outcome = outcome;
    }
}

/// Values are already materialized without forcing. Compare logical constructor
/// shapes using the metadata owner, not tag numbers or rendered Debug strings.
/// Preserve the historical float tolerance. Missing expectations are separate
/// from success; no timeout or admission rejection satisfies an error oracle.
pub fn compare_values(
    values: &[Value],
    expected: &Expectation,
    constructors: &DataConTable,
) -> Result<(), String> {
    if let Expectation::Error(failure) = expected {
        return Err(format!(
            "expected {:?}, but comparison received no failure evidence",
            failure
        ));
    }
    if values.len() != 1 {
        return Err(format!(
            "expected exactly one Haskell result, received {}",
            values.len()
        ));
    }

    enum Task<'a> {
        Compare(&'a Value, &'a Expectation),
        List(&'a Value, &'a [Expectation]),
    }

    let mut work = vec![Task::Compare(&values[0], expected)];
    while let Some(task) = work.pop() {
        match task {
            Task::Compare(value, expectation) => match expectation {
                Expectation::Int(want) => {
                    let got = i64::from_value(value, constructors)
                        .map_err(|error| mismatch("Int", error))?;
                    if got != *want {
                        return Err(format!("expected Int {want}, received Int {got}"));
                    }
                }
                Expectation::Bool(want) => {
                    let got = bool::from_value(value, constructors)
                        .map_err(|error| mismatch("Bool", error))?;
                    if got != *want {
                        return Err(format!("expected Bool {want}, received Bool {got}"));
                    }
                }
                Expectation::Char(want) => {
                    let got = unbox_char(value, constructors)
                        .or_else(|| match value {
                            Value::Lit(Literal::LitWord(code_point)) => {
                                char::from_u32(*code_point as u32)
                            }
                            _ => None,
                        })
                        .ok_or_else(|| "expected Char or canonical Word64 Char".to_string())?;
                    if got != *want {
                        return Err(format!("expected Char {:?}, received Char {:?}", want, got));
                    }
                }
                Expectation::Text(want) => {
                    let got = String::from_value(value, constructors)
                        .map_err(|error| mismatch("Text", error))?;
                    if got != *want {
                        return Err(format!("expected Text {want:?}, received {got:?}"));
                    }
                }
                Expectation::Float64Approx {
                    expected: want,
                    absolute_tolerance,
                } => {
                    let got = f64::from_value(value, constructors)
                        .map_err(|error| mismatch("Float64", error))?;
                    if !((got - *want).abs() <= *absolute_tolerance) {
                        return Err(format!(
                            "expected Float64 within {absolute_tolerance} of {want}, received {got}"
                        ));
                    }
                }
                Expectation::List(elements) => work.push(Task::List(value, elements)),
                Expectation::Tuple(elements) => {
                    let name = tuple_name(elements.len())
                        .ok_or_else(|| "one-element tuples are not a Haskell shape".to_string())?;
                    let fields = constructor(value, name, constructors)?;
                    if fields.len() != elements.len() {
                        return Err(format!(
                            "expected tuple {name} with {} fields, received {}",
                            elements.len(),
                            fields.len()
                        ));
                    }
                    for (field, expectation) in fields.iter().zip(elements).rev() {
                        work.push(Task::Compare(field, expectation));
                    }
                }
                Expectation::Maybe(None) => {
                    let fields = constructor(value, "Nothing", constructors)?;
                    if !fields.is_empty() {
                        return Err(format!(
                            "expected Nothing with no fields, received {}",
                            fields.len()
                        ));
                    }
                }
                Expectation::Maybe(Some(element)) => {
                    let fields = constructor(value, "Just", constructors)?;
                    if fields.len() != 1 {
                        return Err(format!(
                            "expected Just with one field, received {}",
                            fields.len()
                        ));
                    }
                    work.push(Task::Compare(&fields[0], element));
                }
                Expectation::EitherLeft(element) => {
                    let fields = constructor(value, "Left", constructors)?;
                    if fields.len() != 1 {
                        return Err(format!(
                            "expected Left with one field, received {}",
                            fields.len()
                        ));
                    }
                    work.push(Task::Compare(&fields[0], element));
                }
                Expectation::EitherRight(element) => {
                    let fields = constructor(value, "Right", constructors)?;
                    if fields.len() != 1 {
                        return Err(format!(
                            "expected Right with one field, received {}",
                            fields.len()
                        ));
                    }
                    work.push(Task::Compare(&fields[0], element));
                }
                Expectation::Error(failure) => {
                    return Err(format!(
                        "expected {:?}, but comparison received a normal value",
                        failure
                    ));
                }
            },
            Task::List(value, expected_elements) => {
                let mut cursor = value;
                let mut observed_elements = Vec::with_capacity(expected_elements.len());
                loop {
                    match cursor {
                        Value::Con(id, fields) if constructors.name_of(*id) == Some("[]") => {
                            if !fields.is_empty() {
                                return Err(format!(
                                    "malformed [] constructor with {} fields",
                                    fields.len()
                                ));
                            }
                            break;
                        }
                        Value::Con(id, fields) if constructors.name_of(*id) == Some(":") => {
                            if fields.len() != 2 {
                                return Err(format!(
                                    "malformed : constructor with {} fields",
                                    fields.len()
                                ));
                            }
                            observed_elements.push(&fields[0]);
                            cursor = &fields[1];
                        }
                        _ => return Err("expected a list constructor".to_string()),
                    }
                }
                if observed_elements.len() != expected_elements.len() {
                    return Err(format!(
                        "expected list with {} elements, received {}",
                        expected_elements.len(),
                        observed_elements.len()
                    ));
                }
                for (observed, expectation) in observed_elements
                    .iter()
                    .copied()
                    .zip(expected_elements)
                    .rev()
                {
                    work.push(Task::Compare(observed, expectation));
                }
            }
        }
    }
    Ok(())
}

fn constructor<'a>(
    value: &'a Value,
    expected_name: &str,
    constructors: &DataConTable,
) -> Result<&'a [Value], String> {
    match value {
        Value::Con(id, fields) if constructors.name_of(*id) == Some(expected_name) => Ok(fields),
        Value::Con(id, _) => Err(format!(
            "expected constructor {expected_name}, received {}",
            constructors.name_of(*id).unwrap_or("unknown constructor")
        )),
        _ => Err(format!("expected constructor {expected_name}")),
    }
}

fn tuple_name(arity: usize) -> Option<String> {
    match arity {
        0 => Some("()".to_string()),
        1 => None,
        arity => Some(format!("({})", ",".repeat(arity - 1))),
    }
}

fn mismatch(expected: &str, error: tidepool_bridge::BridgeError) -> String {
    format!("expected {expected}: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{DataCon, DataConId, SrcBang};

    fn constructor_table() -> DataConTable {
        let mut table = DataConTable::new();
        for (id, name, arity) in [
            (1, "[]", 0),
            (2, ":", 2),
            (3, "Nothing", 0),
            (4, "Just", 1),
            (5, "Left", 1),
            (6, "Right", 1),
            (7, "(,)", 2),
            (8, "True", 0),
            (9, "False", 0),
        ] {
            table.insert(DataCon {
                id: DataConId(id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: vec![SrcBang::NoSrcBang; arity as usize],
                qualified_name: None,
                type_name: name.into(),
            });
        }
        table
    }

    fn list(values: Vec<Value>) -> Value {
        let mut result = Value::Con(DataConId(1), vec![]);
        for value in values.into_iter().rev() {
            result = Value::Con(DataConId(2), vec![value, result]);
        }
        result
    }

    #[test]
    fn reporting_admission_failure_does_not_fabricate_other_stage_evidence() {
        let mut report = ProgramRecord::new("sample".into());
        report.record(
            Stage::Admission,
            Outcome::Failed {
                reason: "closed program required".into(),
            },
        );
        assert!(matches!(report.stages[0].outcome, Outcome::NotReached));
        assert!(matches!(report.stages[4].outcome, Outcome::NotReached));
    }

    #[test]
    fn comparison_requires_an_actual_value_and_preserves_integer_semantics() {
        let table = DataConTable::default();
        assert!(compare_values(&[], &Expectation::Int(7), &table).is_err());
        assert!(compare_values(
            &[Value::Lit(tidepool_repr::Literal::LitInt(7))],
            &Expectation::Int(7),
            &table
        )
        .is_ok());
        assert!(compare_values(
            &[Value::Lit(tidepool_repr::Literal::LitInt(8))],
            &Expectation::Int(7),
            &table
        )
        .is_err());
    }

    #[test]
    fn compares_nested_list_tuple_maybe_and_either_by_constructor_name() {
        let table = constructor_table();
        let value = Value::Con(
            DataConId(4),
            vec![Value::Con(
                DataConId(7),
                vec![
                    list(vec![Value::Lit(tidepool_repr::Literal::LitInt(7))]),
                    Value::Con(
                        DataConId(6),
                        vec![Value::Lit(tidepool_repr::Literal::LitInt(9))],
                    ),
                ],
            )],
        );
        let expectation = Expectation::Maybe(Some(Box::new(Expectation::Tuple(vec![
            Expectation::List(vec![Expectation::Int(7)]),
            Expectation::EitherRight(Box::new(Expectation::Int(9))),
        ]))));
        assert!(compare_values(&[value], &expectation, &table).is_ok());
    }

    #[test]
    fn rejects_malformed_shape_and_wrong_constructor_without_using_tag_identity() {
        let table = constructor_table();
        let malformed = Value::Con(
            DataConId(2),
            vec![Value::Lit(tidepool_repr::Literal::LitInt(1))],
        );
        assert!(compare_values(
            &[malformed],
            &Expectation::List(vec![Expectation::Int(1)]),
            &table
        )
        .is_err());

        let wrong_constructor = Value::Con(DataConId(8), vec![]);
        assert!(compare_values(&[wrong_constructor], &Expectation::Maybe(None), &table).is_err());
    }

    #[test]
    fn canonical_word_char_and_float_tolerance_are_supported() {
        let table = constructor_table();
        assert!(compare_values(
            &[Value::Lit(tidepool_repr::Literal::LitWord('A' as u64))],
            &Expectation::Char('A'),
            &table
        )
        .is_ok());
        assert!(compare_values(
            &[Value::Lit(tidepool_repr::Literal::LitDouble(
                1.0f64.to_bits()
            ))],
            &Expectation::Float64Approx {
                expected: 1.0 + 1e-11,
                absolute_tolerance: 1e-10,
            },
            &table
        )
        .is_ok());
    }

    #[test]
    fn failure_expectations_never_match_values_or_missing_results() {
        let table = constructor_table();
        let expectation = Expectation::Error(ExpectedFailure::Blackhole);
        assert!(compare_values(&[], &expectation, &table).is_err());
        assert!(compare_values(
            &[Value::Lit(tidepool_repr::Literal::LitInt(1))],
            &expectation,
            &table
        )
        .is_err());
        assert!(compare_values(
            &[
                Value::Lit(tidepool_repr::Literal::LitInt(1)),
                Value::Lit(tidepool_repr::Literal::LitInt(1)),
            ],
            &Expectation::Int(1),
            &table
        )
        .is_err());
    }
}
