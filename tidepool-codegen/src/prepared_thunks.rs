use tidepool_repr::execution_schema::UpdatePolicy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvaluationToken(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThunkEnter<T, E> {
    Evaluate(EvaluationToken),
    Cached(T),
    Blackhole,
    Failed(E),
    SingleEntryReentered,
    TokenExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThunkStateView {
    Unevaluated,
    Evaluating,
    Evaluated,
    Failed,
    Consumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ThunkUpdateError {
    #[error("stale or duplicate thunk update")]
    StaleUpdate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ThunkState<T, E> {
    Unevaluated,
    Evaluating(EvaluationToken),
    Evaluated(T),
    Failed(E),
    Consumed,
}

/// Transactional update state for one prepared-STG thunk.
///
/// An evaluation token is the sole authority to publish a result, failure, or
/// cancellation. Stale tokens cannot overwrite a newer attempt. Failure stores
/// only the first cause; cancellation publishes no payload and makes the thunk
/// eligible for an explicitly retried evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedThunk<T, E> {
    policy: UpdatePolicy,
    next_token: u64,
    state: ThunkState<T, E>,
}

impl<T: Clone, E: Clone> PreparedThunk<T, E> {
    pub fn new(policy: UpdatePolicy) -> Self {
        Self {
            policy,
            next_token: 0,
            state: ThunkState::Unevaluated,
        }
    }

    pub fn enter(&mut self) -> ThunkEnter<T, E> {
        match &self.state {
            ThunkState::Unevaluated => {
                let Some(next_token) = self.next_token.checked_add(1) else {
                    return ThunkEnter::TokenExhausted;
                };
                self.next_token = next_token;
                let token = EvaluationToken(self.next_token);
                self.state = ThunkState::Evaluating(token);
                ThunkEnter::Evaluate(token)
            }
            ThunkState::Evaluating(_) => ThunkEnter::Blackhole,
            ThunkState::Evaluated(value) => ThunkEnter::Cached(value.clone()),
            ThunkState::Failed(error) => ThunkEnter::Failed(error.clone()),
            ThunkState::Consumed => ThunkEnter::SingleEntryReentered,
        }
    }

    pub fn complete(&mut self, token: EvaluationToken, value: T) -> Result<(), ThunkUpdateError> {
        self.verify_token(token)?;
        self.state = match self.policy {
            UpdatePolicy::Memoize => ThunkState::Evaluated(value),
            UpdatePolicy::SingleEntry => ThunkState::Consumed,
        };
        Ok(())
    }

    pub fn fail(&mut self, token: EvaluationToken, error: E) -> Result<(), ThunkUpdateError> {
        self.verify_token(token)?;
        self.state = ThunkState::Failed(error);
        Ok(())
    }

    pub fn cancel(&mut self, token: EvaluationToken) -> Result<(), ThunkUpdateError> {
        self.verify_token(token)?;
        self.state = ThunkState::Unevaluated;
        Ok(())
    }

    pub fn state(&self) -> ThunkStateView {
        match &self.state {
            ThunkState::Unevaluated => ThunkStateView::Unevaluated,
            ThunkState::Evaluating(_) => ThunkStateView::Evaluating,
            ThunkState::Evaluated(_) => ThunkStateView::Evaluated,
            ThunkState::Failed(_) => ThunkStateView::Failed,
            ThunkState::Consumed => ThunkStateView::Consumed,
        }
    }

    fn verify_token(&self, token: EvaluationToken) -> Result<(), ThunkUpdateError> {
        match &self.state {
            ThunkState::Evaluating(current) if *current == token => Ok(()),
            _ => Err(ThunkUpdateError::StaleUpdate),
        }
    }
}
