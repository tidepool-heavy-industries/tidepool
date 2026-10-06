use super::{ParseError, SymbolIdentity};

/// Work for one byte-entry operation, shared across all parsing/validation passes.
/// Units are raw CBOR items, payload-copy bytes, typed visits/copies and the graph
/// owner's charged validation/freeze work. These are admission units, not CPU cycles.
/// Logical node/table limits remain local to their owners.
pub(super) struct OperationBudget {
    limit: usize,
    spent: usize,
}

impl OperationBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self { limit, spent: 0 }
    }

    pub(super) fn charge(&mut self, amount: usize) -> Result<(), ParseError> {
        let spent = self
            .spent
            .checked_add(amount)
            .filter(|spent| *spent <= self.limit)
            .ok_or(ParseError::LimitExceeded("work"))?;
        self.spent = spent;
        Ok(())
    }

    pub(super) fn reserve<T>(&mut self, count: usize) -> Result<(), ParseError> {
        self.charge(
            count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or(ParseError::LimitExceeded("work"))?,
        )
    }

    pub(super) fn remaining(&self) -> usize {
        self.limit - self.spent
    }

    pub(super) fn charge_symbol_copy(&mut self, symbol: &SymbolIdentity) -> Result<(), ParseError> {
        let bytes = [
            &symbol.unit,
            &symbol.module,
            &symbol.namespace,
            &symbol.occurrence,
        ]
        .into_iter()
        .chain(symbol.record_parent.iter())
        .try_fold(0_usize, |bytes, text| bytes.checked_add(text.len()))
        .ok_or(ParseError::LimitExceeded("work"))?;
        self.charge(bytes)
    }

    #[cfg(test)]
    pub(super) fn spent(&self) -> usize {
        self.spent
    }
}
