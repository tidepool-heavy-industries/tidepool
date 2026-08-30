use std::sync::Arc;

/// How a suspended effect request may retain one live heap value by reference.
///
/// This is part of the Haskell/Rust effect ABI. The data bridge may expose a
/// closure sentinel, but only an ABI-declared field authorizes retaining the
/// original heap value.
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

/// Immutable identity of one ordered Haskell effect stack.
///
/// This is the full actor capability ABI, not the machine-handled prefix.
/// Names remain available for diagnostics and Haskell surface generation; the
/// digest is the compact identity attached to compiled/deployed artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectStackAbi {
    names: Arc<[String]>,
    live_payload: LivePayloadPolicy,
    digest: EffectAbiDigest,
}

impl EffectStackAbi {
    #[must_use]
    pub fn new(
        names: impl IntoIterator<Item = impl Into<String>>,
        live_payload: LivePayloadPolicy,
    ) -> Self {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tidepool-effect-stack-abi-v2\0");
        for name in &names {
            hasher.update(&(name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
        }
        match live_payload {
            LivePayloadPolicy::None => {
                hasher.update(&[0]);
            }
            LivePayloadPolicy::RequestField(field) => {
                hasher.update(&[1]);
                hasher.update(&(field as u64).to_le_bytes());
            }
        }
        Self {
            names: Arc::from(names),
            live_payload,
            digest: EffectAbiDigest(*hasher.finalize().as_bytes()),
        }
    }

    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    #[must_use]
    pub fn digest(&self) -> EffectAbiDigest {
        self.digest
    }

    #[must_use]
    pub fn live_payload(&self) -> LivePayloadPolicy {
        self.live_payload
    }

    /// Derive only the runtime dispatch boundary from this full ABI.
    #[must_use]
    pub fn boundary(&self, suspend_tag: u64) -> EffectBoundary {
        EffectBoundary::new(suspend_tag, &self.names)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectAbiDigest([u8; 32]);

impl EffectAbiDigest {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for EffectAbiDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// How two non-empty handled-effect prefixes disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixMismatch {
    Length,
    Position(usize),
}

impl std::fmt::Display for PrefixMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Length => write!(f, "differing lengths"),
            Self::Position(position) => write!(f, "position {position}"),
        }
    }
}

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

    /// Check this boundary against the prefix already established by a shared
    /// machine. An empty prefix dispatches no effects and is compatible with
    /// any layout. Otherwise positional dispatch requires exact equality.
    pub fn check_compatible_prefix(
        &self,
        established: Option<&[String]>,
    ) -> Result<(), PrefixMismatch> {
        if self.handled_prefix.is_empty() {
            return Ok(());
        }
        let Some(established) = established else {
            return Ok(());
        };
        if established.len() != self.handled_prefix.len() {
            return Err(PrefixMismatch::Length);
        }
        established
            .iter()
            .zip(self.handled_prefix.iter())
            .position(|(left, right)| left != right)
            .map_or(Ok(()), |position| Err(PrefixMismatch::Position(position)))
    }
}

#[cfg(test)]
mod tests {
    use super::{EffectBoundary, EffectStackAbi, LivePayloadPolicy};

    #[test]
    fn full_abi_identity_is_ordered_and_separate_from_boundaries() {
        let abi = EffectStackAbi::new(
            ["State", "Actor", "Deliberate"],
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        let same = EffectStackAbi::new(
            ["State", "Actor", "Deliberate"],
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        let reordered = EffectStackAbi::new(
            ["Actor", "State", "Deliberate"],
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        assert_eq!(abi.digest(), same.digest());
        assert_ne!(abi.digest(), reordered.digest());

        let all_suspended = abi.boundary(0);
        let state_handled = abi.boundary(1);
        assert_eq!(all_suspended.handled_prefix(), &[] as &[String]);
        assert_eq!(state_handled.handled_prefix(), &["State"]);
        assert_eq!(abi.names(), ["State", "Actor", "Deliberate"]);
    }

    #[test]
    fn live_payload_policy_participates_in_abi_identity() {
        let data_only = EffectStackAbi::new(["Actor"], LivePayloadPolicy::None);
        let live = EffectStackAbi::new(["Actor"], LivePayloadPolicy::HASKELL_EFFECT_VALUE);
        assert_ne!(data_only.digest(), live.digest());
    }

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

    #[test]
    fn compatibility_is_exact_for_non_empty_prefixes() {
        let names = ["State".to_string(), "Error".to_string()];
        let boundary = EffectBoundary::new(2, &names);
        assert_eq!(boundary.check_compatible_prefix(Some(&names)), Ok(()));
        assert_eq!(
            boundary.check_compatible_prefix(Some(&names[..1])),
            Err(super::PrefixMismatch::Length)
        );
        let different = ["State".to_string(), "Reader".to_string()];
        assert_eq!(
            boundary.check_compatible_prefix(Some(&different)),
            Err(super::PrefixMismatch::Position(1))
        );
    }
}
