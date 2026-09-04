//! Shared, typed observation of the external application bound to an actor.
//!
//! Provider usage is sampled, not accumulated here. The backend reports the
//! latest provider response and may return the same sample on several polls;
//! this owner deduplicates those polls while retaining activation correlation.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

const MAX_PROVIDER_SAMPLES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorActivationKind {
    RootStarted,
    RequestActivated {
        request: crate::RequestId,
        activation_sequence: u64,
    },
    EventsActivated {
        inbox_sequences: Vec<u64>,
    },
}

impl Default for ActorActivationKind {
    fn default() -> Self {
        Self::RootStarted
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderUsageScope {
    LastProviderResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheBoundaryReason {
    Fresh,
    ForkedPrefix,
    ReattachedThread,
    ProviderUnknown,
}

impl Default for CacheBoundaryReason {
    fn default() -> Self {
        Self::ProviderUnknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageSample {
    pub activation_sequence: Option<u64>,
    pub observed_at_unix_ms: u64,
    pub scope: ProviderUsageScope,
    pub cache_boundary: CacheBoundaryReason,
    pub prompt_profile: Option<String>,
    pub prompt_catalog_version: Option<u32>,
    pub prompt_fingerprint: Option<String>,
    pub cached_input_tokens: i64,
    pub uncached_input_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActorRuntimeObservation {
    pub provider_thread: Option<String>,
    pub provider_parent_thread: Option<String>,
    pub current_activation_sequence: Option<u64>,
    pub activation_kind: ActorActivationKind,
    pub event_watermark: u64,
    pub cache_boundary: CacheBoundaryReason,
    pub prompt_profile: Option<String>,
    pub prompt_catalog_version: Option<u32>,
    pub prompt_fingerprint: Option<String>,
    pub provider_usage: Vec<ProviderUsageSample>,
}

impl ActorRuntimeObservation {
    #[must_use]
    pub fn latest_provider_usage(&self) -> Option<&ProviderUsageSample> {
        self.provider_usage.last()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ActorRuntimeObservationHandle {
    inner: Arc<RwLock<ActorRuntimeObservation>>,
}

impl ActorRuntimeObservationHandle {
    #[must_use]
    pub fn snapshot(&self) -> ActorRuntimeObservation {
        self.inner.read().clone()
    }

    pub fn publish_provider_binding(&self, parent_thread: Option<String>, thread: String) {
        let mut observation = self.inner.write();
        observation.provider_parent_thread = parent_thread;
        observation.provider_thread = Some(thread);
    }

    pub fn begin_activation(&self, sequence: u64) {
        self.inner.write().current_activation_sequence = Some(sequence);
    }

    pub fn publish_request_activation(&self, request: crate::RequestId, sequence: u64) {
        let mut observation = self.inner.write();
        observation.current_activation_sequence = Some(sequence);
        observation.activation_kind = ActorActivationKind::RequestActivated {
            request,
            activation_sequence: sequence,
        };
    }

    pub fn publish_event_activation(&self, inbox_sequences: Vec<u64>, watermark: u64) {
        let mut observation = self.inner.write();
        observation.event_watermark = watermark;
        observation.activation_kind = ActorActivationKind::EventsActivated { inbox_sequences };
    }

    pub fn publish_cache_boundary(&self, cache_boundary: CacheBoundaryReason) {
        self.inner.write().cache_boundary = cache_boundary;
    }

    /// Record the exact composed developer prompt installed for this actor.
    /// The body stays out of runtime observations; profile, catalog version,
    /// and content fingerprint are enough to correlate cache behavior.
    pub fn publish_prompt_profile(
        &self,
        profile: impl Into<String>,
        catalog_version: u32,
        fingerprint: impl Into<String>,
    ) {
        let mut observation = self.inner.write();
        observation.prompt_profile = Some(profile.into());
        observation.prompt_catalog_version = Some(catalog_version);
        observation.prompt_fingerprint = Some(fingerprint.into());
    }

    pub fn publish_cache_usage(&self, cached_input_tokens: i64, uncached_input_tokens: i64) {
        let mut observation = self.inner.write();
        let activation_sequence = observation.current_activation_sequence;
        let cache_boundary = observation.cache_boundary;
        let prompt_profile = observation.prompt_profile.clone();
        let prompt_catalog_version = observation.prompt_catalog_version;
        let prompt_fingerprint = observation.prompt_fingerprint.clone();
        if observation.provider_usage.last().is_some_and(|sample| {
            sample.activation_sequence == activation_sequence
                && sample.cached_input_tokens == cached_input_tokens
                && sample.uncached_input_tokens == uncached_input_tokens
                && sample.cache_boundary == cache_boundary
                && sample.prompt_fingerprint == prompt_fingerprint
        }) {
            return;
        }
        observation.provider_usage.push(ProviderUsageSample {
            activation_sequence,
            observed_at_unix_ms: unix_time_ms(),
            scope: ProviderUsageScope::LastProviderResponse,
            cache_boundary,
            prompt_profile,
            prompt_catalog_version,
            prompt_fingerprint,
            cached_input_tokens,
            uncached_input_tokens,
        });
        if observation.provider_usage.len() > MAX_PROVIDER_SAMPLES {
            let remove = observation.provider_usage.len() - MAX_PROVIDER_SAMPLES;
            observation.provider_usage.drain(..remove);
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_provider_metrics_remain_distinct_from_measured_zero() {
        let observation = ActorRuntimeObservationHandle::default();
        assert_eq!(observation.snapshot().latest_provider_usage(), None);
        observation.publish_cache_usage(0, 12);
        assert_eq!(
            observation
                .snapshot()
                .latest_provider_usage()
                .map(|usage| usage.cached_input_tokens),
            Some(0)
        );
    }

    #[test]
    fn repeated_polls_deduplicate_but_new_activations_remain_visible() {
        let observation = ActorRuntimeObservationHandle::default();
        observation.begin_activation(4);
        observation.publish_cache_boundary(CacheBoundaryReason::ForkedPrefix);
        observation.publish_cache_usage(80, 20);
        observation.publish_cache_usage(80, 20);
        observation.begin_activation(5);
        observation.publish_cache_usage(80, 20);

        let snapshot = observation.snapshot();
        assert_eq!(snapshot.provider_usage.len(), 2);
        assert_eq!(snapshot.provider_usage[0].activation_sequence, Some(4));
        assert_eq!(snapshot.provider_usage[1].activation_sequence, Some(5));
        assert_eq!(
            snapshot.provider_usage[1].scope,
            ProviderUsageScope::LastProviderResponse
        );
    }

    #[test]
    fn activation_kind_tracks_typed_request_and_batched_events() {
        let observation = ActorRuntimeObservationHandle::default();
        assert_eq!(
            observation.snapshot().activation_kind,
            ActorActivationKind::RootStarted
        );
        observation.publish_request_activation(crate::RequestId(7), 3);
        assert_eq!(
            observation.snapshot().activation_kind,
            ActorActivationKind::RequestActivated {
                request: crate::RequestId(7),
                activation_sequence: 3,
            }
        );
        observation.publish_event_activation(vec![11, 12], 14);
        let snapshot = observation.snapshot();
        assert_eq!(snapshot.event_watermark, 14);
        assert_eq!(
            snapshot.activation_kind,
            ActorActivationKind::EventsActivated {
                inbox_sequences: vec![11, 12]
            }
        );
    }

    #[test]
    fn usage_sample_carries_the_prompt_identity_active_for_that_response() {
        let observation = ActorRuntimeObservationHandle::default();
        observation.publish_prompt_profile("coding-v1", 3, "abc123");
        observation.publish_cache_usage(90, 10);
        let snapshot = observation.snapshot();
        let sample = snapshot.latest_provider_usage().unwrap();
        assert_eq!(sample.prompt_profile.as_deref(), Some("coding-v1"));
        assert_eq!(sample.prompt_catalog_version, Some(3));
        assert_eq!(sample.prompt_fingerprint.as_deref(), Some("abc123"));
    }
}
