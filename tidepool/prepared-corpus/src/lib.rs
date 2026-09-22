//! Stage evidence and execution support for the prepared-STG semantic corpus.

pub mod watchdog;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_bridge::shapes::unbox_char;
use tidepool_bridge::{FromHaskell, HaskellValue};
use tidepool_codegen::prepared_program::{admit_prepared, CompiledProgram, RunOptions};
use tidepool_repr::execution_schema::{
    link_program, parse_program, DecodeLimits, MachineImports, ProgramRequirements,
};
use tidepool_repr::DataConTable;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Expectation {
    /// The reference program has no finite observation. Keep compiler-stage
    /// coverage, but never count omission of native execution as a match.
    NoFiniteObservation,
    /// A top whose weak head normal form is a finite constructor but whose
    /// reachable graph is cyclic. Native execution runs; only exhaustion of the
    /// observation budget is accepted, and it is recorded as a classification,
    /// never as a comparison pass.
    CyclicObservation,
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
    RaisedException,
}

/// A language-error oracle matches only its exact reusable machine cause.
/// Cancellation, integrity failure, rejection and watchdog termination never
/// count as evidence for an expected Haskell exception.
pub fn matches_expected_failure(
    error: &tidepool_codegen::prepared_program::ExecutionError,
    expected: &ExpectedFailure,
) -> bool {
    use tidepool_codegen::host_fns::RuntimeError;
    use tidepool_codegen::machine_state::{MachineDisposition, MachineFailure};
    use tidepool_codegen::prepared_program::ExecutionError;
    matches!(
        (error, expected),
        (
            ExecutionError::Runtime(MachineFailure {
                cause: RuntimeError::BlackHole,
                disposition: MachineDisposition::Reusable,
            }),
            ExpectedFailure::Blackhole
        ) | (
            ExecutionError::Runtime(MachineFailure {
                cause: RuntimeError::RaisedException | RuntimeError::RaisedExceptionMessage(_),
                disposition: MachineDisposition::Reusable,
            }),
            ExpectedFailure::RaisedException
        )
    )
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Expectations {
    pub source_revision: String,
    /// The generated oracle's domain: every manifest expectation key that GHC
    /// resolves to a binding declared in the source module. `None` for
    /// hand-authored contract cohorts, whose rows are all oracle-bearing.
    #[serde(default)]
    pub source_tops: Option<BTreeSet<String>>,
    pub expectations: BTreeMap<String, Expectation>,
}

/// Whether a row can carry a native GHC oracle at all. Compiler-introduced
/// tops (simplifier floats, workers, dictionaries, `local` bindings) have no
/// source-level name to evaluate; their evidence is transitive through the
/// source tops that reference them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleScope {
    /// The expectation file declares no domain; every row is oracle-bearing.
    Unscoped,
    SourceTop,
    CompilerIntroduced,
}

impl OracleScope {
    pub fn of(expectation_key: Option<&str>, expectations: &Expectations) -> Self {
        match (&expectations.source_tops, expectation_key) {
            (None, _) => Self::Unscoped,
            (Some(tops), Some(key)) if tops.contains(key) => Self::SourceTop,
            (Some(_), _) => Self::CompilerIntroduced,
        }
    }
}

/// Produced directly from GHC's prepared modules, not translated Core filenames.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectionManifest {
    pub version: u32,
    pub source_targets: Vec<SourceTargetMapping>,
    pub programs: Vec<ProjectionRecord>,
}

/// Old artifact names are coverage provenance, not STG entry identities.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceTargetMapping {
    pub source_name: String,
    pub identity: Option<SourceIdentity>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectionRecord {
    pub name: String,
    /// Only an exact external top may carry a historical expectation key.
    pub expectation_key: Option<String>,
    #[serde(flatten)]
    pub projection: ProjectionOutcome,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProjectionOutcome {
    Projected {
        artifact: String,
        identity: SourceIdentity,
    },
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
    #[serde(default)]
    pub record_parent: Option<String>,
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

impl Stage {
    /// Every stage in pipeline order; a stage's position here is its
    /// discriminant, which [`StageRecords`] relies on for lookup.
    pub const ALL: [Stage; 6] = [
        Stage::Projection,
        Stage::Validation,
        Stage::Admission,
        Stage::Compilation,
        Stage::Execution,
        Stage::Comparison,
    ];
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Passed,
    Failed {
        reason: String,
    },
    /// Typed evidence that this stage has no first-order result to produce.
    /// Counted separately from both passes and failures.
    Classified {
        class: Classification,
        reason: String,
    },
    MissingExpectation,
    /// Comparison only: the row is compiler-introduced and has no oracle.
    NoOracle,
    NotReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// The engine refused entry because arguments are required. The corpus
    /// supplies no arguments, so this top is not a closed program.
    NotClosed,
    /// The oracle declares no finite observation (execution omitted), or a
    /// declared cyclic top exhausted the observation budget.
    NoFiniteObservation,
    /// Native execution returned, but the result graph reaches a function or
    /// partial application, which has no first-order observation.
    FunctionValued,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StageRecord {
    pub stage: Stage,
    pub outcome: Outcome,
}

/// Exactly one record per [`Stage`], in [`Stage::ALL`] order. The shape is
/// established once -- at construction or when a report is parsed -- so
/// every later lookup by stage is total. Serializes as the same JSON array
/// of stage records the corpus scripts read.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(try_from = "Vec<StageRecord>", into = "Vec<StageRecord>")]
pub struct StageRecords([StageRecord; 6]);

impl StageRecords {
    fn not_reached() -> Self {
        Self(Stage::ALL.map(|stage| StageRecord {
            stage,
            outcome: Outcome::NotReached,
        }))
    }

    pub fn get(&self, stage: Stage) -> &StageRecord {
        &self.0[stage as usize]
    }

    pub fn get_mut(&mut self, stage: Stage) -> &mut StageRecord {
        &mut self.0[stage as usize]
    }
}

impl TryFrom<Vec<StageRecord>> for StageRecords {
    type Error = String;

    fn try_from(records: Vec<StageRecord>) -> Result<Self, Self::Error> {
        let stages: Vec<Stage> = records.iter().map(|record| record.stage).collect();
        if stages != Stage::ALL {
            return Err(format!(
                "a program record must list every stage once in pipeline order, found {stages:?}"
            ));
        }
        let records: [StageRecord; 6] = records
            .try_into()
            .map_err(|_| "a program record must list exactly six stages".to_owned())?;
        Ok(Self(records))
    }
}

impl From<StageRecords> for Vec<StageRecord> {
    fn from(records: StageRecords) -> Self {
        records.0.into()
    }
}

impl std::ops::Deref for StageRecords {
    type Target = [StageRecord];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for StageRecords {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a> IntoIterator for &'a StageRecords {
    type Item = &'a StageRecord;
    type IntoIter = std::slice::Iter<'a, StageRecord>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut StageRecords {
    type Item = &'a mut StageRecord;
    type IntoIter = std::slice::IterMut<'a, StageRecord>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProgramRecord {
    pub name: String,
    pub stages: StageRecords,
}

/// Run one already-produced prepared artifact through the consumer boundary.
/// The caller supplies the production requirements and constructor metadata;
/// native execution is intentionally kept behind this per-artifact function so
/// a runner can invoke it in a subprocess. `persist` observes every stage only
/// after its outcome is known, including the compilation success immediately
/// before `run_entry` enters native code. `scope` decides whether a row that
/// executed without an oracle is a missing expectation or has no oracle.
pub fn run_prepared_artifact<F>(
    name: impl Into<String>,
    bytes: &[u8],
    requirements: &ProgramRequirements,
    expected: Option<&Expectation>,
    scope: OracleScope,
    constructors: &DataConTable,
    mut persist: F,
) -> ProgramRecord
where
    F: FnMut(&ProgramRecord),
{
    let mut record = ProgramRecord::new(name.into());
    record_stage(
        &mut record,
        Stage::Projection,
        Outcome::Passed,
        &mut persist,
    );
    record_stage(
        &mut record,
        Stage::Validation,
        Outcome::Running,
        &mut persist,
    );
    let prepared = match parse_program(bytes, requirements, DecodeLimits::default()) {
        Ok(prepared) => {
            record_stage(
                &mut record,
                Stage::Validation,
                Outcome::Passed,
                &mut persist,
            );
            prepared
        }
        Err(error) => {
            record_stage(
                &mut record,
                Stage::Validation,
                Outcome::Failed {
                    reason: error.to_string(),
                },
                &mut persist,
            );
            return record;
        }
    };

    record_stage(
        &mut record,
        Stage::Admission,
        Outcome::Running,
        &mut persist,
    );
    if let Err(error) = admit_prepared(&prepared) {
        record_stage(
            &mut record,
            Stage::Admission,
            Outcome::Failed {
                reason: error.to_string(),
            },
            &mut persist,
        );
        return record;
    }
    record_stage(&mut record, Stage::Admission, Outcome::Passed, &mut persist);

    record_stage(
        &mut record,
        Stage::Compilation,
        Outcome::Running,
        &mut persist,
    );
    let linked = match link_program(prepared, &MachineImports::default()) {
        Ok(linked) => linked,
        Err(error) => {
            record_stage(
                &mut record,
                Stage::Compilation,
                Outcome::Failed {
                    reason: error.to_string(),
                },
                &mut persist,
            );
            return record;
        }
    };
    let entry = linked.prepared().entry();
    let program = match CompiledProgram::compile(&linked) {
        Ok(program) => program,
        Err(error) => {
            record_stage(
                &mut record,
                Stage::Compilation,
                Outcome::Failed {
                    reason: error.to_string(),
                },
                &mut persist,
            );
            return record;
        }
    };
    record_stage(
        &mut record,
        Stage::Compilation,
        Outcome::Passed,
        &mut persist,
    );

    if matches!(expected, Some(Expectation::NoFiniteObservation)) {
        record_stage(
            &mut record,
            Stage::Execution,
            Outcome::Classified {
                class: Classification::NoFiniteObservation,
                reason: "oracle has no finite observation; native execution omitted".into(),
            },
            &mut persist,
        );
        return record;
    }

    record_stage(
        &mut record,
        Stage::Execution,
        Outcome::Running,
        &mut persist,
    );
    let run = match program.run_entry(
        entry,
        &[],
        &RunOptions {
            nursery_bytes: 4096,
            observation_budget: 100_000,
            collect_before_observation: false,
        },
        Arc::new(AtomicBool::new(false)),
    ) {
        Ok(run) => run,
        Err(error) => {
            let outcome = match classify_execution_error(&error, expected) {
                Some((class, reason)) => Outcome::Classified { class, reason },
                None => Outcome::Failed {
                    reason: error.to_string(),
                },
            };
            record_stage(&mut record, Stage::Execution, outcome, &mut persist);
            // An error oracle is compared only after native execution reached
            // its terminal result. Validation, admission, compilation, and
            // watchdog failures remain stage failures, never language evidence.
            if let Some(outcome) = comparison_for_execution_error(&error, expected) {
                record_stage(
                    &mut record,
                    Stage::Comparison,
                    Outcome::Running,
                    &mut persist,
                );
                record_stage(&mut record, Stage::Comparison, outcome, &mut persist);
            }
            return record;
        }
    };
    record_stage(&mut record, Stage::Execution, Outcome::Passed, &mut persist);

    record_stage(
        &mut record,
        Stage::Comparison,
        Outcome::Running,
        &mut persist,
    );
    let outcome = match expected {
        None => match scope {
            OracleScope::CompilerIntroduced => Outcome::NoOracle,
            OracleScope::Unscoped | OracleScope::SourceTop => Outcome::MissingExpectation,
        },
        Some(expected) => match compare_values(&run.values, expected, constructors) {
            Ok(()) => Outcome::Passed,
            Err(reason) => Outcome::Failed { reason },
        },
    };
    record_stage(&mut record, Stage::Comparison, outcome, &mut persist);
    record
}

fn record_stage<F>(record: &mut ProgramRecord, stage: Stage, outcome: Outcome, persist: &mut F)
where
    F: FnMut(&ProgramRecord),
{
    record.record(stage, outcome);
    persist(record);
}

fn comparison_for_expected_failure(
    error: &tidepool_codegen::prepared_program::ExecutionError,
    expected: &ExpectedFailure,
) -> Outcome {
    if matches_expected_failure(error, expected) {
        Outcome::Passed
    } else {
        Outcome::Failed {
            reason: format!("expected {expected:?}, received execution failure: {error}"),
        }
    }
}

/// Only an error oracle is compared against a failure. A row without one has
/// no comparison to make, so the stage stays not reached rather than reporting
/// a missing expectation for a program that produced no value.
fn comparison_for_execution_error(
    error: &tidepool_codegen::prepared_program::ExecutionError,
    expected: Option<&Expectation>,
) -> Option<Outcome> {
    match expected {
        Some(Expectation::Error(expected)) => {
            Some(comparison_for_expected_failure(error, expected))
        }
        _ => None,
    }
}

/// Classifications of a native call that returned without a first-order
/// observation. Each requires one exact typed cause. A value oracle always
/// wins: a row GHC evaluated to data is a failure if it cannot be observed.
fn classify_execution_error(
    error: &tidepool_codegen::prepared_program::ExecutionError,
    expected: Option<&Expectation>,
) -> Option<(Classification, String)> {
    use tidepool_codegen::prepared_program::{ExecutionError, ObservationFailure};
    use tidepool_heap::execution_descriptor::ObjectKind;
    match (error, expected) {
        (
            ExecutionError::Unsupported(
                tidepool_codegen::prepared_program::Unsupported::HostArguments(_),
            ),
            None,
        )
        | (
            ExecutionError::Arguments {
                actual: 0,
                expected: 1..,
            },
            None,
        ) => Some((Classification::NotClosed, error.to_string())),
        (
            ExecutionError::Observation(ObservationFailure::Unobservable(
                ObjectKind::Function | ObjectKind::Pap,
            )),
            None,
        ) => Some((Classification::FunctionValued, error.to_string())),
        (
            ExecutionError::Observation(ObservationFailure::BudgetExceeded { .. }),
            Some(Expectation::CyclicObservation),
        ) => Some((
            Classification::NoFiniteObservation,
            format!("declared cyclic observation: {error}"),
        )),
        _ => None,
    }
}

impl ProgramRecord {
    /// Recording a later stage never implicitly marks earlier stages passed.
    pub fn new(name: String) -> Self {
        Self {
            name,
            stages: StageRecords::not_reached(),
        }
    }

    pub fn record(&mut self, stage: Stage, outcome: Outcome) {
        self.stages.get_mut(stage).outcome = outcome;
    }
}

/// Compare materialized logical constructor shapes using the metadata owner.
/// Missing expectations are separate from success; no timeout or admission
/// rejection satisfies an error oracle.
pub fn compare_values(
    values: &[HaskellValue],
    expected: &Expectation,
    constructors: &DataConTable,
) -> Result<(), String> {
    if matches!(
        expected,
        Expectation::NoFiniteObservation | Expectation::CyclicObservation
    ) {
        return Err("no finite observation is available for value comparison".into());
    }
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
        Compare(&'a HaskellValue, &'a Expectation),
        List(&'a HaskellValue, &'a [Expectation]),
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
                    let got = canonical_char(value, constructors)
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
                    // Native oracle values use zero tolerance: preserve signed
                    // zero as well as the exact finite binary64 value.
                    let within = if *absolute_tolerance == 0.0 {
                        got.is_finite() && got.to_bits() == want.to_bits()
                    } else {
                        // A NaN distance is outside every tolerance.
                        matches!(
                            (got - *want).abs().partial_cmp(absolute_tolerance),
                            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                        )
                    };
                    if !within {
                        return Err(format!(
                            "expected Float64 within {absolute_tolerance} of {want}, received {got}"
                        ));
                    }
                }
                Expectation::List(elements) => work.push(Task::List(value, elements)),
                Expectation::Tuple(elements) => {
                    let name = tuple_name(elements.len())
                        .ok_or_else(|| "one-element tuples are not a Haskell shape".to_string())?;
                    let fields = constructor(value, &name, constructors)?;
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
                Expectation::NoFiniteObservation | Expectation::CyclicObservation => {
                    return Err("no finite observation is available for value comparison".into());
                }
            },
            Task::List(value, expected_elements) => {
                let mut cursor = value;
                let mut observed_elements = Vec::with_capacity(expected_elements.len());
                loop {
                    match cursor {
                        HaskellValue::Con(id, fields)
                            if constructors.name_of(*id) == Some("[]") =>
                        {
                            if !fields.is_empty() {
                                return Err(format!(
                                    "malformed [] constructor with {} fields",
                                    fields.len()
                                ));
                            }
                            break;
                        }
                        HaskellValue::Con(id, fields) if constructors.name_of(*id) == Some(":") => {
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

fn canonical_char(value: &HaskellValue, constructors: &DataConTable) -> Option<char> {
    unbox_char(value, constructors)
}

fn constructor<'a>(
    value: &'a HaskellValue,
    expected_name: &str,
    constructors: &DataConTable,
) -> Result<&'a [HaskellValue], String> {
    match value {
        HaskellValue::Con(id, fields) if constructors.name_of(*id) == Some(expected_name) => {
            Ok(fields)
        }
        HaskellValue::Con(id, _) => Err(format!(
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
            (10, "C#", 1),
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

    fn list(values: Vec<HaskellValue>) -> HaskellValue {
        let mut result = HaskellValue::Con(DataConId(1), vec![]);
        for value in values.into_iter().rev() {
            result = HaskellValue::Con(DataConId(2), vec![value, result]);
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

    fn runtime_failure(
        cause: tidepool_codegen::host_fns::RuntimeError,
        disposition: tidepool_codegen::machine_state::MachineDisposition,
    ) -> tidepool_codegen::prepared_program::ExecutionError {
        tidepool_codegen::prepared_program::ExecutionError::Runtime(
            tidepool_codegen::machine_state::MachineFailure { cause, disposition },
        )
    }

    #[test]
    fn expected_failure_matching_requires_the_exact_reusable_cause() {
        use tidepool_codegen::host_fns::RuntimeError;
        use tidepool_codegen::machine_state::MachineDisposition;

        assert!(matches_expected_failure(
            &runtime_failure(RuntimeError::RaisedException, MachineDisposition::Reusable),
            &ExpectedFailure::RaisedException,
        ));
        assert!(matches_expected_failure(
            &runtime_failure(RuntimeError::BlackHole, MachineDisposition::Reusable),
            &ExpectedFailure::Blackhole,
        ));
        assert!(!matches_expected_failure(
            &runtime_failure(RuntimeError::RaisedException, MachineDisposition::Reusable),
            &ExpectedFailure::Blackhole,
        ));
        assert!(!matches_expected_failure(
            &runtime_failure(RuntimeError::BlackHole, MachineDisposition::Reusable),
            &ExpectedFailure::RaisedException,
        ));
        assert!(!matches_expected_failure(
            &runtime_failure(RuntimeError::Cancelled, MachineDisposition::Reusable),
            &ExpectedFailure::RaisedException,
        ));
        assert!(!matches_expected_failure(
            &runtime_failure(
                RuntimeError::RaisedException,
                MachineDisposition::Unavailable
            ),
            &ExpectedFailure::RaisedException,
        ));
    }

    #[test]
    fn expected_execution_failure_preserves_execution_reason_and_records_comparison() {
        use tidepool_codegen::host_fns::RuntimeError;
        use tidepool_codegen::machine_state::MachineDisposition;

        let error = runtime_failure(RuntimeError::RaisedException, MachineDisposition::Reusable);
        let mut report = ProgramRecord::new("raised".into());
        report.record(
            Stage::Execution,
            Outcome::Failed {
                reason: error.to_string(),
            },
        );
        report.record(
            Stage::Comparison,
            comparison_for_expected_failure(&error, &ExpectedFailure::RaisedException),
        );
        assert!(matches!(
            report.stages[4].outcome,
            Outcome::Failed { ref reason } if reason == "Haskell exception raised"
        ));
        assert!(matches!(report.stages[5].outcome, Outcome::Passed));

        report.record(
            Stage::Comparison,
            comparison_for_expected_failure(&error, &ExpectedFailure::Blackhole),
        );
        assert!(matches!(report.stages[5].outcome, Outcome::Failed { .. }));

        assert!(comparison_for_execution_error(&error, None).is_none());
        assert!(comparison_for_execution_error(&error, Some(&Expectation::Int(1))).is_none());
    }

    #[test]
    fn comparison_requires_an_actual_value_and_preserves_integer_semantics() {
        let table = DataConTable::default();
        assert!(compare_values(&[], &Expectation::Int(7), &table).is_err());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitInt(7))],
            &Expectation::Int(7),
            &table
        )
        .is_ok());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitInt(8))],
            &Expectation::Int(7),
            &table
        )
        .is_err());
    }

    #[test]
    fn compares_nested_list_tuple_maybe_and_either_by_constructor_name() {
        let table = constructor_table();
        let value = HaskellValue::Con(
            DataConId(4),
            vec![HaskellValue::Con(
                DataConId(7),
                vec![
                    list(vec![HaskellValue::Lit(tidepool_repr::Literal::LitInt(7))]),
                    HaskellValue::Con(
                        DataConId(6),
                        vec![HaskellValue::Lit(tidepool_repr::Literal::LitInt(9))],
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
        let malformed = HaskellValue::Con(
            DataConId(2),
            vec![HaskellValue::Lit(tidepool_repr::Literal::LitInt(1))],
        );
        assert!(compare_values(
            &[malformed],
            &Expectation::List(vec![Expectation::Int(1)]),
            &table
        )
        .is_err());

        let wrong_constructor = HaskellValue::Con(DataConId(8), vec![]);
        assert!(compare_values(&[wrong_constructor], &Expectation::Maybe(None), &table).is_err());
    }

    #[test]
    fn canonical_word_char_and_float_tolerance_are_supported() {
        let table = constructor_table();
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitWord(
                'A' as u64
            ))],
            &Expectation::Char('A'),
            &table
        )
        .is_ok());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitWord(
                0x1_00000061
            ))],
            &Expectation::Char('a'),
            &table
        )
        .is_err());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitWord(0xd800))],
            &Expectation::Char('a'),
            &table
        )
        .is_err());
        assert!(compare_values(
            &[HaskellValue::Con(
                DataConId(10),
                vec![HaskellValue::Lit(tidepool_repr::Literal::LitWord(
                    'A' as u64
                ))],
            )],
            &Expectation::Char('A'),
            &table
        )
        .is_ok());
        assert!(compare_values(
            &[HaskellValue::Con(
                DataConId(10),
                vec![HaskellValue::Lit(tidepool_repr::Literal::LitInt(
                    'A' as i64
                ))],
            )],
            &Expectation::Char('A'),
            &table
        )
        .is_err());
        assert!(compare_values(
            &[HaskellValue::Con(DataConId(10), vec![])],
            &Expectation::Char('A'),
            &table
        )
        .is_err());
        assert!(compare_values(
            &[HaskellValue::Con(
                DataConId(10),
                vec![HaskellValue::Lit(tidepool_repr::Literal::LitWord(
                    0x1_00000041
                ))],
            )],
            &Expectation::Char('A'),
            &table
        )
        .is_err());
        assert!(compare_values(
            &[HaskellValue::Con(
                DataConId(4),
                vec![HaskellValue::Lit(tidepool_repr::Literal::LitWord(
                    'A' as u64
                ))],
            )],
            &Expectation::Char('A'),
            &table
        )
        .is_err());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitDouble(
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
    fn native_float_oracle_decodes_roundtrip_and_preserves_signed_zero() {
        let expected: Expectation = serde_json::from_str(
            r#"{"kind":"float64_approx","value":{"expected":1.1045419098831014e66,"absolute_tolerance":0.0}}"#,
        )
        .unwrap();
        let Expectation::Float64Approx {
            expected: number, ..
        } = &expected
        else {
            panic!("expected a float oracle");
        };
        assert_eq!(number.to_bits(), 0x4da4_f9fc_3c6d_a5d7);
        let table = DataConTable::default();
        let value = |bits| HaskellValue::Lit(tidepool_repr::Literal::LitDouble(bits));
        assert!(compare_values(&[value(number.to_bits())], &expected, &table).is_ok());
        assert!(compare_values(&[value(number.to_bits() + 1)], &expected, &table).is_err());

        let negative_zero: Expectation = serde_json::from_str(
            r#"{"kind":"float64_approx","value":{"expected":-0.0,"absolute_tolerance":0.0}}"#,
        )
        .unwrap();
        assert!(compare_values(&[value((-0.0f64).to_bits())], &negative_zero, &table).is_ok());
        assert!(compare_values(&[value(0.0f64.to_bits())], &negative_zero, &table).is_err());
    }

    #[test]
    fn failure_expectations_never_match_values_or_missing_results() {
        let table = constructor_table();
        let expectation = Expectation::Error(ExpectedFailure::Blackhole);
        assert!(compare_values(&[], &expectation, &table).is_err());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitInt(1))],
            &expectation,
            &table
        )
        .is_err());
        assert!(compare_values(
            &[
                HaskellValue::Lit(tidepool_repr::Literal::LitInt(1)),
                HaskellValue::Lit(tidepool_repr::Literal::LitInt(1)),
            ],
            &Expectation::Int(1),
            &table
        )
        .is_err());
    }

    #[test]
    fn malformed_artifact_fails_validation_after_projection_without_running_jit() {
        let requirements = ProgramRequirements {
            schema_version: tidepool_repr::execution_schema::SCHEMA_VERSION,
            projection_profile: "ghc-9.12-prepared-stg".into(),
            toolchain: "ghc-9.12.2".into(),
            execution_abi_version: tidepool_repr::execution_schema::EXECUTION_ABI_VERSION,
            target: tidepool_repr::execution_schema::TargetDescriptor {
                architecture: tidepool_repr::execution_schema::Architecture::X86_64,
                endianness: tidepool_repr::execution_schema::Endianness::Little,
                pointer_width: 64,
                word_width: 64,
                abi: "sysv64".into(),
                features: vec![],
            },
        };
        let mut snapshots = Vec::new();
        let record = run_prepared_artifact(
            "malformed",
            &[0xff],
            &requirements,
            None,
            OracleScope::Unscoped,
            &DataConTable::default(),
            |record| {
                snapshots.push(
                    record
                        .stages
                        .iter()
                        .map(|stage| (stage.stage, matches!(stage.outcome, Outcome::Running)))
                        .collect::<Vec<_>>(),
                );
            },
        );
        assert!(matches!(record.stages[0].outcome, Outcome::Passed));
        assert!(matches!(record.stages[1].outcome, Outcome::Failed { .. }));
        assert!(record.stages[2..]
            .iter()
            .all(|stage| matches!(stage.outcome, Outcome::NotReached)));
        assert!(snapshots.iter().any(|snapshot| {
            snapshot
                .iter()
                .any(|(stage, running)| *stage == Stage::Validation && *running)
        }));
    }

    /// A minimal closed `TPSTG` artifact (one nullary function returning a
    /// literal `Int64`) encoded by hand against the running schema constants,
    /// never a captured artifact, so it tracks `SCHEMA_VERSION` automatically.
    fn finite_prepared_program_bytes() -> Vec<u8> {
        fn head(major: u8, length: usize) -> Vec<u8> {
            let mut result = Vec::new();
            if length <= 23 {
                result.push((major << 5) | length as u8);
            } else if length <= u8::MAX as usize {
                result.extend([(major << 5) | 24, length as u8]);
            } else if length <= u16::MAX as usize {
                result.push((major << 5) | 25);
                result.extend((length as u16).to_be_bytes());
            } else {
                result.push((major << 5) | 26);
                result.extend((length as u32).to_be_bytes());
            }
            result
        }
        fn array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
            let values: Vec<_> = values.into_iter().collect();
            let mut result = head(4, values.len());
            for value in values {
                result.extend(value);
            }
            result
        }
        fn uint(value: u64) -> Vec<u8> {
            if value <= 23 {
                vec![value as u8]
            } else if value <= u8::MAX as u64 {
                vec![0x18, value as u8]
            } else if value <= u16::MAX as u64 {
                let mut result = vec![0x19];
                result.extend((value as u16).to_be_bytes());
                result
            } else {
                let mut result = vec![0x1a];
                result.extend((value as u32).to_be_bytes());
                result
            }
        }
        fn text(value: &str) -> Vec<u8> {
            let mut result = head(3, value.len());
            result.extend(value.as_bytes());
            result
        }
        fn bytes(value: &[u8]) -> Vec<u8> {
            let mut result = head(2, value.len());
            result.extend(value);
            result
        }

        let rep_int64 = array([uint(4), uint(64)]);
        let signature = array([array([]), array([uint(0), array([rep_int64])])]);
        let scalar_42 = array([uint(0), uint(64), bytes(&42_i64.to_be_bytes())]);
        let atom = array([uint(1), scalar_42]);
        let return_frame = array([uint(0), array([atom])]);
        let symbol = array([
            text("fixture"),
            text("Suite"),
            text("value"),
            text("lit_42"),
            array([uint(0)]),
        ]);
        let function_rhs = array([uint(0), uint(0), array([]), array([]), uint(0)]);
        let heap_binding = array([uint(0), function_rhs]);
        let top_binding = array([symbol, heap_binding]);
        let group_nonrecursive = array([uint(0), top_binding]);
        array([
            text("TPSTG"),
            uint(tidepool_repr::execution_schema::SCHEMA_VERSION),
            text("ghc-9.12-prepared-stg"),
            text("ghc-9.12.2"),
            uint(tidepool_repr::execution_schema::EXECUTION_ABI_VERSION),
            array([
                uint(0),
                uint(0),
                uint(64),
                uint(64),
                text("sysv64"),
                array([]),
            ]),
            array([signature]),
            array([]),
            array([]),
            array([]),
            array([return_frame]),
            array([group_nonrecursive]),
            uint(0),
            array([]),
            array([]),
            array([]),
            array([uint(0)]),
        ])
    }

    #[test]
    fn no_finite_observation_preserves_compile_evidence_without_native_execution() {
        use tidepool_repr::execution_schema::testing;

        let expectations: Expectations = serde_json::from_str(include_str!(
            "../fixtures/prepared-corpus-expectations.json"
        ))
        .unwrap();
        // Pinned GHC 9.12.2 at -O2 -fno-full-laziness -fcpr-anal lowers
        // Suite.thunk_blackhole to a self-recursive let-no-escape join. Its
        // compiled binary times out; it does not produce a blackhole error.
        let expected = expectations.expectations.get("thunk_blackhole").unwrap();
        assert!(matches!(expected, Expectation::NoFiniteObservation));
        assert_eq!(
            serde_json::to_value(expected).unwrap(),
            serde_json::json!({"kind": "no_finite_observation"})
        );

        // A minimal closed prepared program (return literal 42, mirroring
        // Suite.lit_42) is finite: without the typed guard, this row would
        // execute and reach comparison instead. Built directly against the
        // running schema and requirements rather than frozen wire bytes, so
        // it cannot strand on a schema bump the way a captured artifact would.
        let bytes = finite_prepared_program_bytes();
        let bytes = bytes.as_slice();
        let envelope = testing::envelope();
        let requirements = ProgramRequirements {
            schema_version: envelope.schema_version,
            projection_profile: envelope.projection_profile,
            toolchain: envelope.toolchain,
            execution_abi_version: envelope.execution_abi_version,
            target: envelope.target,
        };
        let mut saw_execution_running = false;
        let record = run_prepared_artifact(
            "finite-test-artifact",
            bytes,
            &requirements,
            Some(expected),
            OracleScope::SourceTop,
            &DataConTable::default(),
            |record| {
                saw_execution_running |= record.stages.iter().any(|stage| {
                    stage.stage == Stage::Execution && matches!(stage.outcome, Outcome::Running)
                });
            },
        );
        assert!(record.stages[..4]
            .iter()
            .all(|stage| matches!(stage.outcome, Outcome::Passed)));
        assert!(matches!(
            record.stages[4].outcome,
            Outcome::Classified {
                class: Classification::NoFiniteObservation,
                ref reason,
            } if reason.contains("native execution omitted")
        ));
        assert!(matches!(record.stages[5].outcome, Outcome::NotReached));
        assert!(!saw_execution_running);
    }

    #[test]
    fn stage_records_lookup_matches_pipeline_order_and_rejects_bad_shapes() {
        for (position, stage) in Stage::ALL.into_iter().enumerate() {
            assert_eq!(stage as usize, position);
        }
        let mut record = ProgramRecord::new("row".into());
        record.record(Stage::Execution, Outcome::Passed);
        assert!(matches!(
            record.stages.get(Stage::Execution).outcome,
            Outcome::Passed
        ));
        let json = serde_json::to_string(&record).unwrap();
        let back: ProgramRecord = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.stages[4].outcome, Outcome::Passed));

        let mut reordered: Vec<StageRecord> = record.stages.clone().into();
        reordered.swap(0, 1);
        assert!(StageRecords::try_from(reordered).is_err());
        let mut short: Vec<StageRecord> = record.stages.clone().into();
        short.pop();
        assert!(StageRecords::try_from(short).is_err());
    }

    #[test]
    fn classifications_require_the_exact_typed_cause_and_yield_to_oracles() {
        use tidepool_codegen::prepared_program::{ExecutionError, ObservationFailure};
        use tidepool_heap::execution_descriptor::ObjectKind;

        let arguments = ExecutionError::Arguments {
            actual: 0,
            expected: 1,
        };
        assert!(matches!(
            classify_execution_error(&arguments, None),
            Some((Classification::NotClosed, _))
        ));
        assert!(classify_execution_error(&arguments, Some(&Expectation::Int(1))).is_none());
        assert!(classify_execution_error(
            &ExecutionError::Arguments {
                actual: 1,
                expected: 2
            },
            None
        )
        .is_none());

        let function =
            ExecutionError::Observation(ObservationFailure::Unobservable(ObjectKind::Function));
        assert!(matches!(
            classify_execution_error(&function, None),
            Some((Classification::FunctionValued, _))
        ));
        assert!(classify_execution_error(&function, Some(&Expectation::Int(1))).is_none());
        assert!(
            classify_execution_error(&function, Some(&Expectation::CyclicObservation)).is_none()
        );
        let thunk =
            ExecutionError::Observation(ObservationFailure::Unobservable(ObjectKind::Thunk));
        assert!(classify_execution_error(&thunk, None).is_none());

        let budget = ExecutionError::Observation(ObservationFailure::BudgetExceeded { limit: 1 });
        assert!(matches!(
            classify_execution_error(&budget, Some(&Expectation::CyclicObservation)),
            Some((Classification::NoFiniteObservation, _))
        ));
        assert!(classify_execution_error(&budget, None).is_none());
        assert!(compare_values(
            &[HaskellValue::Lit(tidepool_repr::Literal::LitInt(1))],
            &Expectation::CyclicObservation,
            &DataConTable::default()
        )
        .is_err());
    }

    #[test]
    fn oracle_scope_distinguishes_source_tops_only_when_a_domain_is_declared() {
        let mut expectations = Expectations {
            source_revision: "test".into(),
            source_tops: None,
            expectations: BTreeMap::new(),
        };
        assert_eq!(
            OracleScope::of(Some("t_swap1"), &expectations),
            OracleScope::Unscoped
        );
        assert_eq!(OracleScope::of(None, &expectations), OracleScope::Unscoped);
        expectations.source_tops = Some(["t_swap".to_string()].into_iter().collect());
        assert_eq!(
            OracleScope::of(Some("t_swap"), &expectations),
            OracleScope::SourceTop
        );
        assert_eq!(
            OracleScope::of(Some("t_swap1"), &expectations),
            OracleScope::CompilerIntroduced
        );
        assert_eq!(
            OracleScope::of(None, &expectations),
            OracleScope::CompilerIntroduced
        );
    }

    #[test]
    fn new_outcomes_and_expectations_have_stable_json_shapes() {
        let outcome = Outcome::Classified {
            class: Classification::NotClosed,
            reason: "r".into(),
        };
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({"status": "classified", "class": "not_closed", "reason": "r"})
        );
        assert_eq!(
            serde_json::to_value(Outcome::NoOracle).unwrap(),
            serde_json::json!({"status": "no_oracle"})
        );
        let cyclic: Expectation =
            serde_json::from_value(serde_json::json!({"kind": "cyclic_observation"})).unwrap();
        assert!(matches!(cyclic, Expectation::CyclicObservation));
        let older_format: Expectations =
            serde_json::from_value(serde_json::json!({"source_revision": "x", "expectations": {}}))
                .unwrap();
        assert!(older_format.source_tops.is_none());
    }
}
