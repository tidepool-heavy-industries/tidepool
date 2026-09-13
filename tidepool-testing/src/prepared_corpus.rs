//! Stage evidence for the prepared-STG semantic corpus, never Core execution.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tidepool_bridge::Value;
use tidepool_repr::DataConTable;

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
    _values: &[Value],
    _expected: &Expectation,
    _constructors: &DataConTable,
) -> Result<(), String> {
    todo!("corpus:COMPARE")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
