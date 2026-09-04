//! Shared, typed observation of the external application bound to an actor.

use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActorRuntimeObservation {
    pub provider_thread: Option<String>,
    pub provider_parent_thread: Option<String>,
    pub cached_input_tokens: Option<i64>,
    pub uncached_input_tokens: Option<i64>,
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

    pub fn publish_cache_usage(&self, cached_input_tokens: i64, uncached_input_tokens: i64) {
        let mut observation = self.inner.write();
        observation.cached_input_tokens = Some(cached_input_tokens);
        observation.uncached_input_tokens = Some(uncached_input_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_provider_metrics_remain_distinct_from_measured_zero() {
        let observation = ActorRuntimeObservationHandle::default();
        assert_eq!(observation.snapshot().cached_input_tokens, None);
        observation.publish_cache_usage(0, 12);
        assert_eq!(observation.snapshot().cached_input_tokens, Some(0));
    }
}
