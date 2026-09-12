use tidepool_repr::execution_schema::{
    LayoutError, RuntimeRep, Signature, StorageLayout, TargetDescriptor,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum CallStatus {
    Success = 0,
    LanguageFailure = 1,
    IntegrityFailure = 2,
    Cancelled = 3,
}

impl CallStatus {
    pub fn from_raw(raw: i64) -> Result<Self, ControlError> {
        match raw {
            0 => Ok(Self::Success),
            1 => Ok(Self::LanguageFailure),
            2 => Ok(Self::IntegrityFailure),
            3 => Ok(Self::Cancelled),
            other => Err(ControlError::UnknownStatus(other)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum NativeValue {
    ManagedRef(u64),
    Address(u64),
    Int { bits: u8, value: i128 },
    Word { bits: u8, value: u128 },
    Float { bits: u8, bytes: Vec<u8> },
}

impl NativeValue {
    pub fn rep(&self) -> RuntimeRep {
        match self {
            Self::ManagedRef(_) => RuntimeRep::LiftedRef,
            Self::Address(_) => RuntimeRep::Address,
            Self::Int { bits, .. } => RuntimeRep::Int(*bits),
            Self::Word { bits, .. } => RuntimeRep::Word(*bits),
            Self::Float { bits, .. } => RuntimeRep::Float(*bits),
        }
    }
}

/// Caller-owned result storage. A callee may fill it, but only a successful
/// status publishes initialized values to the caller.
#[derive(Clone, Debug)]
pub struct ResultArea {
    layout: StorageLayout,
    result_reps: Vec<RuntimeRep>,
    values: Vec<Option<NativeValue>>,
}

impl ResultArea {
    pub fn new(
        target: &TargetDescriptor,
        result_reps: &[RuntimeRep],
    ) -> Result<Self, ControlError> {
        Ok(Self {
            layout: StorageLayout::for_reps(target, result_reps)?,
            result_reps: result_reps.to_vec(),
            values: vec![None; result_reps.len()],
        })
    }

    pub fn layout(&self) -> &StorageLayout {
        &self.layout
    }

    pub fn write(&mut self, logical_index: usize, value: NativeValue) -> Result<(), ControlError> {
        let expected = self
            .result_reps
            .get(logical_index)
            .copied()
            .ok_or(ControlError::ResultIndex(logical_index))?;
        if expected == RuntimeRep::Void {
            return Err(ControlError::VoidResultWrite(logical_index));
        }
        if !rep_matches(expected, value.rep()) {
            return Err(ControlError::Representation {
                expected,
                actual: value.rep(),
            });
        }
        self.values[logical_index] = Some(value);
        Ok(())
    }

    /// Publish the whole result vector on success. Failure statuses erase any
    /// partial payload so callers cannot accidentally consume it.
    pub fn finish(mut self, status: CallStatus) -> Result<Vec<Option<NativeValue>>, ControlError> {
        if status != CallStatus::Success {
            self.values.fill(None);
            return Err(ControlError::CallFailed(status));
        }
        for (index, rep) in self.result_reps.iter().copied().enumerate() {
            if rep != RuntimeRep::Void && self.values[index].is_none() {
                return Err(ControlError::UninitializedResult(index));
            }
        }
        Ok(self.values)
    }
}

#[derive(Clone, Debug)]
pub struct JoinContract {
    arguments: Vec<RuntimeRep>,
    results: Vec<RuntimeRep>,
}

impl JoinContract {
    pub fn from_signature(signature: &Signature) -> Self {
        Self {
            arguments: signature.arguments.clone(),
            results: signature.results.clone(),
        }
    }

    pub fn check_arguments(&self, actual: &[RuntimeRep]) -> Result<(), ControlError> {
        check_reps(&self.arguments, actual)
    }

    pub fn check_results(&self, actual: &[RuntimeRep]) -> Result<(), ControlError> {
        check_reps(&self.results, actual)
    }
}

fn check_reps(expected: &[RuntimeRep], actual: &[RuntimeRep]) -> Result<(), ControlError> {
    if expected.len() != actual.len() {
        return Err(ControlError::Arity {
            expected: expected.len(),
            actual: actual.len(),
        });
    }
    for (expected, actual) in expected.iter().copied().zip(actual.iter().copied()) {
        if !rep_matches(expected, actual) {
            return Err(ControlError::Representation { expected, actual });
        }
    }
    Ok(())
}

fn rep_matches(expected: RuntimeRep, actual: RuntimeRep) -> bool {
    expected == actual
        || matches!(
            (expected, actual),
            (RuntimeRep::LiftedRef, RuntimeRep::UnliftedRef)
                | (RuntimeRep::UnliftedRef, RuntimeRep::LiftedRef)
        )
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error("unknown native call status {0}")]
    UnknownStatus(i64),
    #[error("native call failed with {0:?}")]
    CallFailed(CallStatus),
    #[error("result index {0} is outside the signature")]
    ResultIndex(usize),
    #[error("attempted to write semantic Void result {0}")]
    VoidResultWrite(usize),
    #[error("result {0} was not initialized")]
    UninitializedResult(usize),
    #[error("control-flow arity mismatch: expected {expected}, got {actual}")]
    Arity { expected: usize, actual: usize },
    #[error("control-flow representation mismatch: expected {expected:?}, got {actual:?}")]
    Representation {
        expected: RuntimeRep,
        actual: RuntimeRep,
    },
}
