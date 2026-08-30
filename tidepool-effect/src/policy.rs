/// How a suspended effect request may retain one live heap value by reference.
///
/// The data bridge may expose a closure sentinel, but only an explicitly
/// selected field authorizes retaining the original heap value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LivePayloadPolicy {
    /// Suspended requests must cross as ordinary data.
    #[default]
    None,
    /// Retain this zero-based request-constructor field when its bridged value
    /// contains a closure sentinel.
    RequestField(usize),
}

impl LivePayloadPolicy {
    /// Current Haskell effect-request convention: site/metadata in field 0 and
    /// the value crossing the runtime boundary in field 1.
    pub const HASKELL_EFFECT_VALUE: Self = Self::RequestField(1);
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
