/// How a suspended effect request may retain one live heap value by reference.
///
/// The data bridge may expose a closure sentinel, but only an explicitly
/// selected field authorizes retaining the original heap value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LivePayloadPolicy {
    /// Suspended requests must cross as ordinary data.
    #[default]
    None,
    /// Retain this field only when ordinary bridging found a closure. This is
    /// the compatibility path for consumers that use the live root solely as
    /// a fallback for an otherwise unbridgeable value.
    ClosureField(usize),
    /// Always retain this field as the authoritative in-heap value, including
    /// first-order data that also has a bridged diagnostic projection.
    ValueField(usize),
}

impl LivePayloadPolicy {
    /// Current Haskell effect-request convention: site/metadata in field 0 and
    /// the value crossing the runtime boundary in field 1.
    pub const HASKELL_EFFECT_VALUE: Self = Self::ValueField(1);

    /// Historical closure-only crossing used by consumers that reconstruct
    /// ordinary data from the bridge.
    pub const HASKELL_EFFECT_CLOSURE: Self = Self::ClosureField(1);
}

/// How one run treats effect requests relative to its installed handlers.
///
/// Routing is nominal: handlers recognize request constructors, never
/// freer-simple union positions. This policy therefore controls only whether
/// handlers are consulted and what an unhandled constructor means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum EffectRunPolicy {
    /// Consult installed handlers; an unrecognized request is an error.
    #[default]
    HandleOrError,
    /// Consult installed handlers; suspend an unrecognized request.
    HandleOrSuspend,
    /// Suspend every request without consulting installed handlers.
    SuspendAll,
}

#[cfg(test)]
mod tests {
    use super::EffectRunPolicy;

    #[test]
    fn ordinary_runs_handle_or_error_by_default() {
        assert_eq!(EffectRunPolicy::default(), EffectRunPolicy::HandleOrError);
    }
}
