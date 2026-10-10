use tidepool_repr::Generation;

/// A confirmed range belongs to the SessionLib that owns it. Moving the
/// library moves the range; copying a declaration log does not copy authority.
pub(super) struct GenerationRangeLease {
    ceiling: Generation,
}

static_assertions::assert_not_impl_any!(GenerationRangeLease: Clone, Copy);

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("declaration generation exhausted at {consumed}; requested {requested} identities")]
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
    pub(super) fn confirmed(ceiling: Generation) -> Self {
        Self { ceiling }
    }

    pub(super) fn covers(&self, end: Generation) -> bool {
        end <= self.ceiling
    }
}

#[cfg(test)]
mod tests;
