use tidepool_repr::Generation;

/// A confirmed range belongs to the SessionLib that owns both disjoint Lib
/// and Val namespaces. Moving the library moves the range; copying a context
/// does not copy authority. Its lower bound excludes a prior incarnation's tail.
pub(super) struct GenerationRangeLease {
    floor: Generation,
    ceiling: Generation,
}

static_assertions::assert_not_impl_any!(GenerationRangeLease: Clone, Copy);

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("session generation exhausted at {consumed}; requested {requested} identities")]
pub struct GenerationExhausted {
    pub consumed: Generation,
    pub requested: u64,
}

pub(super) fn requested_end(
    consumed: Generation,
    requested: u64,
) -> Result<Generation, GenerationExhausted> {
    consumed
        .0
        .checked_add(requested)
        .map(Generation)
        .ok_or(GenerationExhausted {
            consumed,
            requested,
        })
}

/// Refill normally amortizes one durable transaction across 64 identities.
/// The final partial range and requests larger than that block remain valid.
pub(super) fn refill_ceiling(ceiling: Generation, end: Generation) -> Generation {
    assert!(end > ceiling);
    let extension = (end.0 - ceiling.0).max(64).min(u64::MAX - ceiling.0);
    Generation(ceiling.0 + extension)
}

impl GenerationRangeLease {
    pub(super) fn confirmed(floor: Generation, ceiling: Generation) -> Self {
        assert!(floor <= ceiling);
        Self { floor, ceiling }
    }

    pub(super) fn covers(&self, consumed: Generation, end: Generation) -> bool {
        consumed >= self.floor && end >= consumed && end <= self.ceiling
    }
}

#[cfg(test)]
mod tests;
