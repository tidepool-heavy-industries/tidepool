//! Shared, typed observation of the external application bound to an actor.
//!
//! The backend recovers durable provider observations and response aggregates.
//! Polls replace totals and deduplicate display samples by source identity;
//! polling time never supplies causal attribution.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

const MAX_PROVIDER_SAMPLES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActorActivationKind {
    #[default]
    RootStarted,
    RequestActivated {
        request: crate::RequestId,
        activation_sequence: u64,
    },
    EventsActivated {
        inbox_sequences: Vec<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheBoundaryReason {
    Fresh,
    ForkedPrefix,
    ReattachedThread,
    #[default]
    ProviderUnknown,
}

/// Current posture of the hosted Haskell workbench.
///
/// This deliberately distinguishes running Haskell from waiting inside a
/// named effect handler. It is an observation only; scheduler control remains
/// in the actor and resident machine owners.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ActorWorkbenchPosture {
    #[default]
    Idle,
    RunningUnit {
        input_unit_index: usize,
        total: usize,
    },
    AwaitingEffect {
        input_unit_index: usize,
        total: usize,
        effect: String,
    },
    TerminalTransfer {
        transfer: ActorWorkbenchTransfer,
    },
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorWorkbenchTransfer {
    Reply,
    CancellationAcknowledgement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageSample {
    pub observation_id: String,
    pub source_timestamp: Option<String>,
    pub observed_at_unix_ms: u64,
    pub cache_boundary: CacheBoundaryReason,
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
    pub first_provider_usage: Option<ProviderUsageSample>,
    pub provider_usage_summary: Option<tidepool_model::ProviderUsageSummary>,
    pub latest_turn_usage_summary: Option<tidepool_model::ProviderUsageSummary>,
    pub workbench_posture: ActorWorkbenchPosture,
}

impl ActorRuntimeObservation {
    #[must_use]
    pub fn latest_provider_usage(&self) -> Option<&ProviderUsageSample> {
        self.provider_usage.last()
    }

    pub(crate) fn usage_summary_display(&self) -> String {
        match &self.provider_usage_summary {
            Some(summary) => format!(
                "usage={:?} responses={} cached={} uncached={}",
                summary.completeness,
                summary.observations,
                summary.usage.cached_input_tokens,
                summary.usage.input_tokens - summary.usage.cached_input_tokens,
            ),
            None => "usage=unavailable".into(),
        }
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

    pub fn publish_workbench_posture(&self, posture: ActorWorkbenchPosture) {
        self.inner.write().workbench_posture = posture;
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

    pub fn publish_cache_usage(&self, usage: tidepool_model::ProviderUsageSnapshot) {
        let mut observation = self.inner.write();
        // Replace authoritative aggregates even when the latest response is
        // unchanged: a later durable completion event can settle its scope.
        observation.provider_usage_summary = usage.thread_summary;
        observation.latest_turn_usage_summary = usage.latest_turn_summary;
        let make_sample = |source: tidepool_model::ProviderUsageObservation| ProviderUsageSample {
            observation_id: source.id,
            source_timestamp: source.timestamp,
            observed_at_unix_ms: unix_time_ms(),
            cache_boundary: observation.cache_boundary,
            cached_input_tokens: source.usage.cached_input_tokens,
            uncached_input_tokens: source.usage.input_tokens - source.usage.cached_input_tokens,
        };
        let first = make_sample(usage.first);
        let latest = make_sample(usage.latest);
        if observation.first_provider_usage.is_none() {
            observation.first_provider_usage = Some(first);
        }
        if observation
            .provider_usage
            .last()
            .is_some_and(|sample| sample.observation_id == latest.observation_id)
        {
            return;
        }
        observation.provider_usage.push(latest);
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
    fn first_observation_survives_history_eviction_and_equal_counts_are_distinct() {
        let observation = ActorRuntimeObservationHandle::default();
        let first = usage("first", 10, 20).first;
        for index in 0..40 {
            let mut snapshot = usage(&format!("response-{index}"), 10, 20);
            snapshot.first = first.clone();
            observation.publish_cache_usage(snapshot);
        }
        let snapshot = observation.snapshot();
        assert_eq!(
            snapshot
                .first_provider_usage
                .as_ref()
                .unwrap()
                .observation_id,
            "first"
        );
        assert_eq!(snapshot.provider_usage.len(), MAX_PROVIDER_SAMPLES);
        assert_eq!(
            snapshot.latest_provider_usage().unwrap().observation_id,
            "response-39"
        );
    }

    fn usage(id: &str, cached: i64, uncached: i64) -> tidepool_model::ProviderUsageSnapshot {
        let observation = tidepool_model::ProviderUsageObservation {
            id: id.into(),
            timestamp: Some("2026-09-05T00:00:00Z".into()),
            usage: tidepool_model::TokenUsage {
                input_tokens: cached + uncached,
                cached_input_tokens: cached,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: cached + uncached,
            },
        };
        tidepool_model::ProviderUsageSnapshot {
            first: observation.clone(),
            latest: observation,
            thread_summary: None,
            latest_turn_summary: None,
        }
    }

    #[test]
    fn absent_provider_metrics_remain_distinct_from_measured_zero() {
        let observation = ActorRuntimeObservationHandle::default();
        assert_eq!(observation.snapshot().latest_provider_usage(), None);
        observation.publish_cache_usage(usage("first", 0, 12));
        assert_eq!(
            observation
                .snapshot()
                .latest_provider_usage()
                .map(|usage| usage.cached_input_tokens),
            Some(0)
        );
    }

    #[test]
    fn repeated_polls_do_not_attribute_old_usage_to_new_activations() {
        let observation = ActorRuntimeObservationHandle::default();
        observation.begin_activation(4);
        observation.publish_cache_boundary(CacheBoundaryReason::ForkedPrefix);
        observation.publish_cache_usage(usage("first", 80, 20));
        observation.publish_cache_usage(usage("first", 80, 20));
        observation.begin_activation(5);
        observation.publish_cache_usage(usage("first", 80, 20));

        let snapshot = observation.snapshot();
        assert_eq!(snapshot.provider_usage.len(), 1);
    }

    #[test]
    fn aggregate_usage_replacement_survives_eviction_and_completion_without_new_response() {
        use tidepool_model::{ProviderUsageCompleteness, ProviderUsageScope, ProviderUsageSummary};
        let observation = ActorRuntimeObservationHandle::default();
        let mut last = usage("last", 80, 20);
        for i in 0..40 {
            observation.publish_cache_usage(usage(&format!("{i}"), 80, 20));
        }
        last.thread_summary = Some(ProviderUsageSummary {
            scope: ProviderUsageScope::Thread("thread".into()),
            completeness: ProviderUsageCompleteness::Partial,
            observations: 100,
            usage: tidepool_model::TokenUsage {
                input_tokens: 10000,
                cached_input_tokens: 8000,
                ..Default::default()
            },
        });
        observation.publish_cache_usage(last.clone());
        last.thread_summary.as_mut().unwrap().completeness = ProviderUsageCompleteness::Complete;
        observation.publish_request_activation(crate::RequestId(8), 5);
        observation.publish_cache_usage(last.clone());
        let snapshot = observation.snapshot();
        assert_eq!(snapshot.provider_usage.len(), MAX_PROVIDER_SAMPLES);
        assert_eq!(snapshot.provider_usage_summary, last.thread_summary);
        assert_eq!(snapshot.latest_turn_usage_summary, None);
        assert_eq!(
            snapshot.usage_summary_display(),
            "usage=Complete responses=100 cached=8000 uncached=2000"
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
    fn workbench_posture_preserves_the_named_suspension_boundary() {
        let observation = ActorRuntimeObservationHandle::default();
        observation.publish_workbench_posture(ActorWorkbenchPosture::RunningUnit {
            input_unit_index: 2,
            total: 5,
        });
        assert_eq!(
            observation.snapshot().workbench_posture,
            ActorWorkbenchPosture::RunningUnit {
                input_unit_index: 2,
                total: 5,
            }
        );

        observation.publish_workbench_posture(ActorWorkbenchPosture::AwaitingEffect {
            input_unit_index: 2,
            total: 5,
            effect: "watch replies".into(),
        });
        assert_eq!(
            observation.snapshot().workbench_posture,
            ActorWorkbenchPosture::AwaitingEffect {
                input_unit_index: 2,
                total: 5,
                effect: "watch replies".into(),
            }
        );
    }
}
