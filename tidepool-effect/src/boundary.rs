use std::sync::Arc;

/// The point where runtime effect dispatch gives way to suspension.
///
/// Effect tags below `suspend_tag` are handled locally. Their names form a
/// positional prefix of the effect roster and travel with parked
/// continuations so machines shared by multiple runtime scopes can reject
/// incompatible handler layouts before executing code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectBoundary {
    suspend_tag: u64,
    handled_prefix: Arc<[String]>,
}

impl EffectBoundary {
    /// Derive a boundary from the complete effect-name roster.
    ///
    /// A roster shorter than `suspend_tag` is valid: every available name is
    /// retained. This mirrors effect dispatch, which cannot name entries the
    /// caller did not supply, and avoids making metadata completeness a panic
    /// condition.
    #[must_use]
    pub fn new(suspend_tag: u64, effect_names: &[String]) -> Self {
        let requested = usize::try_from(suspend_tag).unwrap_or(usize::MAX);
        let prefix_len = requested.min(effect_names.len());
        Self {
            suspend_tag,
            handled_prefix: Arc::from(&effect_names[..prefix_len]),
        }
    }

    #[must_use]
    pub fn suspend_tag(&self) -> u64 {
        self.suspend_tag
    }

    #[must_use]
    pub fn handled_prefix(&self) -> &[String] {
        &self.handled_prefix
    }

    #[must_use]
    pub fn handled_prefix_arc(&self) -> Arc<[String]> {
        self.handled_prefix.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::EffectBoundary;

    #[test]
    fn derives_the_prefix_below_the_boundary() {
        let names = ["State".to_string(), "Error".to_string(), "Ask".to_string()];
        let boundary = EffectBoundary::new(2, &names);
        assert_eq!(boundary.suspend_tag(), 2);
        assert_eq!(boundary.handled_prefix(), &names[..2]);
    }

    #[test]
    fn a_short_roster_is_valid_metadata() {
        let names = ["State".to_string()];
        let boundary = EffectBoundary::new(3, &names);
        assert_eq!(boundary.handled_prefix(), names);
    }
}
