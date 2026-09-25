//! Shared, typed observation of the external application bound to an actor.
//!
//! The backend recovers durable provider observations and response aggregates.
//! Polls replace totals and deduplicate display samples by source identity;
//! polling time never supplies causal attribution.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

const MAX_PROVIDER_SAMPLES: usize = 32;

/// Launch-time checkout mapping supplied by the process-boundary owner.
/// Paths are presentation evidence, not worktree authority or live Git status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorWorkspaceObservation {
    pub workspace_path: std::path::PathBuf,
    pub host_storage_path: std::path::PathBuf,
    pub worktree_id: Option<String>,
    pub expected_branch: Option<String>,
}

impl ActorWorkspaceObservation {
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "workspace_path={:?} (native tools); host_storage_path={:?}; assigned_worktree={:?}; expected_branch={:?}",
            self.workspace_path, self.host_storage_path, self.worktree_id, self.expected_branch,
        )
    }

    #[must_use]
    pub fn orientation(&self) -> String {
        format!("{}\n  The workspace path can be identical across actors; each actor sees its assigned checkout. Worktree receipt `cwd` is the host storage path. Verify assignment with Git identity, not a unique-looking `pwd`.", self.summary())
    }
}

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
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorWorkbenchTransfer {
    Reply,
    CancellationAcknowledgement,
}

/// Inbound delivery of tracked messages to this actor, as the host's delivery
/// pump last saw it. The pump republishes it on every tick from the same
/// state that decides submission, deferral and withdrawal, so a status view
/// built from it cannot disagree with delivery.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboundDeliveryObservation {
    pub inbox: InboxDelivery,
    pub last_message: Option<TrackedMessageObservation>,
    pub next: InboundNext,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InboxDelivery {
    #[default]
    Open,
    /// The front message waits for the actor's computing `haskell` cell to
    /// end before it is submitted.
    WaitingForCell { since_unix_ms: u64 },
    /// The front message stayed without native evidence past the grace
    /// period, or is terminally fenced; no later tracked message can be
    /// presented before it resolves. An ordinary in-flight message is not
    /// fenced.
    Fenced { reason: String, since_unix_ms: u64 },
}

/// The most recent tracked message at the front of the actor's inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedMessageObservation {
    pub sequence: u64,
    pub state: TrackedMessageState,
    /// When the pump first observed `state`.
    pub at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedMessageState {
    Queued,
    Submitted,
    Unconfirmed,
    Presented,
    Withdrawn,
    Rejected,
    Compacted,
}

/// What happens next for inbound delivery, named as an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InboundNext {
    /// Nothing is owed; the next event arrives on its own.
    #[default]
    AwaitEvent,
    /// The front message is submitted once the computing cell ends.
    AwaitCell,
    /// The pump is withdrawing or re-delivering the front message.
    Resubmitting,
    /// The host cannot recover delivery on its own; hand the work off.
    NoHostRecovery,
}

/// One source layer's active revision against the latest observed on-disk
/// capture, and which modules' digests differ between them.
///
/// Part of the what-is-live status view's source-drift rows
/// (`resident_actor::live_status_text`). The data lives in
/// `tidepool::exomonad::source`, which this crate cannot depend on (`tidepool`
/// depends on `exomonad-actor`, never the reverse); `tidepool`'s composition
/// root publishes this into the observation channel instead. Absence of a
/// value for this field (`ActorSourceDriftObservation::layer` is `None`)
/// means the layer has not been observed yet — never means it was checked
/// and found identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLayerDrift {
    pub active_identity: String,
    pub active_generation: u64,
    pub disk_identity: String,
    pub disk_generation: u64,
    /// Module names whose digest differs between the active and on-disk
    /// revisions, including a module present in one and absent from the
    /// other. Empty means checked and identical.
    pub changed_modules: Vec<String>,
}

/// A managed checkout's Git head and dirty files, observed on the same poll
/// as [`SourceLayerDrift`].
///
/// There is no recorded build revision for the running binary anywhere in
/// this system: no build script or embedded string captures the Git commit
/// the binary was built from (`CARGO_PKG_VERSION`, where it appears, is the
/// crate's semver, not a commit). A status view over this data can report
/// what Git itself observes about the checkout; it cannot honestly compare
/// that to a binary revision that was never recorded, and does not invent
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutGitDrift {
    pub head: String,
    /// Staged, unstaged and untracked paths, sorted. Empty means checked and
    /// clean.
    pub dirty_files: Vec<String>,
}

/// Frozen workspace modules whose digest differs from the same module read
/// live off disk right now. The frozen capture is the run's immutable floor;
/// this is independent of any layer republished in front of it
/// ([`SourceLayerDrift`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenSourceDrift {
    /// Empty means checked and identical.
    pub changed_modules: Vec<String>,
}

/// The three source-drift rows of the what-is-live status view, each
/// observed and published independently. A field left `None` means that row
/// has not been observed for this actor, which the view renders distinctly
/// from an observed-and-empty (no drift) result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActorSourceDriftObservation {
    pub layer: Option<SourceLayerDrift>,
    pub checkout: Option<CheckoutGitDrift>,
    pub frozen: Option<FrozenSourceDrift>,
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
    pub compactions: Option<u64>,
    pub backend_executable: Option<String>,
    pub backend_version: Option<String>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub confirmed_model: Option<String>,
    pub confirmed_effort: Option<String>,
    pub provider_observation_stale: bool,
    pub provider_turn: Option<exomonad_model::ProviderTurnObservation>,
    pub provider_failures: Vec<exomonad_model::ProviderTurnObservation>,
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
    /// The retained first sample is the first response in the current aggregate.
    /// Legacy-to-durable transitions may change source identity; counts alone
    /// cannot establish membership for subtraction.
    pub first_usage_in_summary: bool,
    pub provider_usage_summary: Option<exomonad_model::ProviderUsageSummary>,
    pub latest_turn_usage_summary: Option<exomonad_model::ProviderUsageSummary>,
    pub workbench_posture: ActorWorkbenchPosture,
    pub workspace: Option<ActorWorkspaceObservation>,
    pub launch_role: Option<crate::EffectiveRole>,
    pub launched_at_unix_ms: Option<i64>,
    /// Set by the host while it launches this actor's provider application
    /// (the current launch phase); cleared once the provider binds.
    pub launch_pending: Option<String>,
    /// Whether what is running still matches what is on disk. See
    /// [`ActorSourceDriftObservation`].
    pub source_drift: ActorSourceDriftObservation,
    /// When the host first observed the current provider turn no longer
    /// active; `None` while a turn is active or no turn is observed.
    pub provider_idle_since_unix_ms: Option<u64>,
    /// `None` until the host's delivery pump runs for this actor.
    pub inbound_delivery: Option<InboundDeliveryObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentDisposition {
    Working,
    NeedsAttention,
    SettledAwaitingProvider,
    IdleRetained,
}

impl AgentDisposition {
    pub(crate) fn constructor_name(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsAttention => "NeedsAttention",
            Self::SettledAwaitingProvider => "SettledAwaitingProvider",
            Self::IdleRetained => "IdleRetained",
        }
    }
}

impl ActorRuntimeObservation {
    /// Fixed per-incarnation launch orientation; detailed observations remain in status.
    #[must_use]
    pub fn launch_orientation(&self) -> Option<String> {
        let role = self.launch_role.as_ref()?;
        let budget = role.descendants();
        let mut text = format!(
            "Actor authority: role={:?}; native_tools={:?}; workspace={:?}; descendant_depth={}; max_active_children={}.",
            role.role(), role.native_tools(), role.workspace(),
            budget.maximum_depth, crate::render_child_budget(budget.maximum_active_children),
        );
        if let Some(workspace) = &self.workspace {
            text.push_str("\nWorkspace binding: ");
            text.push_str(&format!(
                "workspace_path={:?} (native tools); expected_branch={:?}; seed={}",
                workspace.workspace_path,
                workspace.expected_branch.as_deref().unwrap_or("unassigned"),
                "currentCheckout"
            ));
            if role.role() == crate::ActorRole::Root && workspace.worktree_id.is_none() {
                text.push_str("\nRoot checkout: writable project repository. currentCheckout resolves this checkout when seeding children; in a bound child it resolves that child's checkout. Explicit worktree and commit references remain fixed.");
            }
        }
        Some(text)
    }

    /// One compact line for a parent's status view of this actor:
    /// `<label> req=<n> pending; provider=<..>; inbox=<..>; last_message=<..>;
    /// [source=<head>;] next=<action>`. Every field comes from published
    /// observations; `source` appears only once a checkout head was observed.
    pub(crate) fn delivery_status_line(
        &self,
        label: &str,
        requests: usize,
        now_unix_ms: u64,
    ) -> String {
        use exomonad_model::ProviderTurnState;
        let provider = match (&self.provider_turn, self.provider_observation_stale) {
            (None, _) | (Some(_), true) => "unknown".to_owned(),
            (Some(turn), false) => match &turn.state {
                ProviderTurnState::Active => format!("running turn={}", turn.turn),
                ProviderTurnState::Succeeded => match self.provider_idle_since_unix_ms {
                    Some(since) => format!("idle {}", render_age(since, now_unix_ms)),
                    None => "idle".to_owned(),
                },
                ProviderTurnState::Failed(_) => "failed".to_owned(),
                ProviderTurnState::Interrupted => "interrupted".to_owned(),
            },
        };
        let (inbox, last_message, next) = match &self.inbound_delivery {
            None => ("unobserved".to_owned(), "none".to_owned(), "await-event"),
            Some(delivery) => (
                match &delivery.inbox {
                    InboxDelivery::Open => "open".to_owned(),
                    InboxDelivery::WaitingForCell { since_unix_ms } => format!(
                        "waiting(computing cell, {})",
                        render_age(*since_unix_ms, now_unix_ms)
                    ),
                    InboxDelivery::Fenced {
                        reason,
                        since_unix_ms,
                    } => format!(
                        "fenced({reason}, {})",
                        render_age(*since_unix_ms, now_unix_ms)
                    ),
                },
                delivery.last_message.as_ref().map_or_else(
                    || "none".to_owned(),
                    |message| {
                        let state = match message.state {
                            TrackedMessageState::Queued => "queued/not-presented".to_owned(),
                            TrackedMessageState::Submitted => "submitted/not-presented".to_owned(),
                            TrackedMessageState::Unconfirmed => {
                                "unconfirmed/not-presented".to_owned()
                            }
                            TrackedMessageState::Presented => {
                                format!("presented@{}", render_clock(message.at_unix_ms))
                            }
                            TrackedMessageState::Withdrawn => "withdrawn/not-presented".to_owned(),
                            TrackedMessageState::Rejected => "rejected/not-presented".to_owned(),
                            TrackedMessageState::Compacted => "compacted/unknown".to_owned(),
                        };
                        format!("ref{} {state}", message.sequence)
                    },
                ),
                match delivery.next {
                    InboundNext::AwaitEvent => "await-event",
                    InboundNext::AwaitCell => "await-cell",

                    InboundNext::Resubmitting => "resubmitting",
                    InboundNext::NoHostRecovery => "no host recovery; hand off",
                },
            ),
        };
        let source = self
            .source_drift
            .checkout
            .as_ref()
            .map(|checkout| {
                format!(
                    " source={};",
                    checkout.head.get(..7).unwrap_or(&checkout.head)
                )
            })
            .unwrap_or_default();
        format!(
            "{label} req={requests} pending; provider={provider}; inbox={inbox}; last_message={last_message};{source} next={next}"
        )
    }

    pub(crate) fn disposition(&self, has_requests: bool) -> AgentDisposition {
        use exomonad_model::ProviderTurnState;
        use AgentDisposition::*;
        if self.provider_observation_stale || self.provider_turn.is_none() {
            return NeedsAttention;
        }
        match self.provider_turn.as_ref().map(|turn| &turn.state) {
            Some(ProviderTurnState::Failed(_) | ProviderTurnState::Interrupted) => NeedsAttention,
            _ if has_requests => Working,
            Some(ProviderTurnState::Succeeded) => IdleRetained,
            Some(ProviderTurnState::Active) => SettledAwaitingProvider,
            _ => NeedsAttention,
        }
    }
    #[must_use]
    pub fn latest_provider_usage(&self) -> Option<&ProviderUsageSample> {
        self.provider_usage.last()
    }

    pub(crate) fn usage_summary_display(&self) -> String {
        use exomonad_model::ProviderUsageCompleteness;
        let first_completeness = self
            .provider_usage_summary
            .as_ref()
            .filter(|_| self.first_usage_in_summary)
            .map_or(ProviderUsageCompleteness::Partial, |summary| {
                summary.completeness
            });
        let first = match &self.first_provider_usage {
            Some(sample) => format!(
                "first_observed={first_completeness:?} input={} cached={}",
                sample.cached_input_tokens + sample.uncached_input_tokens,
                sample.cached_input_tokens,
            ),
            None => "first_observed=unavailable".into(),
        };
        let thread = match &self.provider_usage_summary {
            Some(summary) => format!(
                "thread_usage={:?} responses={} cached={} uncached={}",
                summary.completeness,
                summary.observations,
                summary.usage.cached_input_tokens,
                summary.usage.input_tokens - summary.usage.cached_input_tokens,
            ),
            None => "thread_usage=unavailable".into(),
        };
        let subsequent = match self.subsequent_input_usage() {
            Some((completeness, responses, cached, uncached)) => format!(
                "subsequent_usage={completeness:?} responses={responses} cached={cached} uncached={uncached}"
            ),
            None => "subsequent_usage=unavailable".into(),
        };
        format!("{first} {subsequent} {thread}")
    }

    fn subsequent_input_usage(
        &self,
    ) -> Option<(exomonad_model::ProviderUsageCompleteness, i64, i64, i64)> {
        if !self.first_usage_in_summary {
            return None;
        }
        let first = self.first_provider_usage.as_ref()?;
        let summary = self.provider_usage_summary.as_ref()?;
        let responses = summary.observations.checked_sub(1)?;
        let cached = summary
            .usage
            .cached_input_tokens
            .checked_sub(first.cached_input_tokens)?;
        let uncached = summary
            .usage
            .input_tokens
            .checked_sub(summary.usage.cached_input_tokens)?
            .checked_sub(first.uncached_input_tokens)?;
        (responses >= 0 && cached >= 0 && uncached >= 0).then_some((
            summary.completeness,
            responses,
            cached,
            uncached,
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ActorRuntimeObservationHandle {
    inner: Arc<RwLock<ActorRuntimeObservation>>,
}

impl ActorRuntimeObservationHandle {
    pub fn publish_launch_role(&self, role: crate::EffectiveRole, launched_at_unix_ms: i64) {
        let mut observation = self.inner.write();
        observation.launch_role = Some(role);
        observation.launched_at_unix_ms = Some(launched_at_unix_ms);
    }

    pub fn publish_workspace(&self, workspace: ActorWorkspaceObservation) {
        self.inner.write().workspace = Some(workspace);
    }

    pub fn publish_provider_observation(&self, observation: exomonad_model::ProviderObservation) {
        {
            let mut state = self.inner.write();
            if observation
                .turn
                .as_ref()
                .zip(state.provider_turn.as_ref())
                .is_some_and(|(next, previous)| {
                    next.thread == previous.thread && next.revision < previous.revision
                })
            {
                state.provider_observation_stale = true;
                return;
            }
            state.provider_observation_stale = false;
            state.provider_failures = observation.failures;
            state.compactions = observation.compactions;
            state.confirmed_model = observation.confirmed_model;
            state.confirmed_effort = observation.confirmed_effort;
            if let Some(turn) = observation.turn {
                if state.provider_turn.as_ref().is_none_or(|previous| {
                    previous.thread != turn.thread || previous.revision <= turn.revision
                }) {
                    let same_turn = state.provider_turn.as_ref().is_some_and(|previous| {
                        previous.thread == turn.thread && previous.turn == turn.turn
                    });
                    if turn.state == exomonad_model::ProviderTurnState::Active {
                        state.provider_idle_since_unix_ms = None;
                    } else if !same_turn || state.provider_idle_since_unix_ms.is_none() {
                        state.provider_idle_since_unix_ms = Some(unix_time_ms());
                    }
                    state.provider_turn = Some(turn);
                }
            } else {
                state.provider_observation_stale = true;
            }
        }
        if let Some(usage) = observation.usage {
            self.publish_cache_usage(usage);
        }
    }

    pub fn mark_provider_observation_stale(&self) {
        self.inner.write().provider_observation_stale = true;
    }

    pub fn publish_backend_provenance(
        &self,
        executable: String,
        version: String,
        model: Option<String>,
        effort: Option<String>,
    ) {
        let mut state = self.inner.write();
        state.backend_executable = Some(executable);
        state.backend_version = Some(version);
        state.requested_model = model;
        state.requested_effort = effort;
    }

    #[must_use]
    pub fn snapshot(&self) -> ActorRuntimeObservation {
        self.inner.read().clone()
    }

    /// Record the host's current launch phase for an actor whose provider
    /// has not bound yet.
    pub fn publish_launch_pending(&self, phase: impl Into<String>) {
        self.inner.write().launch_pending = Some(phase.into());
    }

    pub fn publish_provider_binding(&self, parent_thread: Option<String>, thread: String) {
        let mut observation = self.inner.write();
        observation.launch_pending = None;
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

    pub fn publish_inbound_delivery(&self, delivery: InboundDeliveryObservation) {
        self.inner.write().inbound_delivery = Some(delivery);
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

    /// Publish row 1 of the source-drift view: this actor's source layer,
    /// active vs. latest observed disk.
    pub fn publish_source_layer_drift(&self, drift: SourceLayerDrift) {
        self.inner.write().source_drift.layer = Some(drift);
    }

    /// Publish row 2: the checkout's Git head and dirty files.
    pub fn publish_checkout_git_drift(&self, drift: CheckoutGitDrift) {
        self.inner.write().source_drift.checkout = Some(drift);
    }

    /// Publish row 3: frozen workspace modules that differ from disk.
    pub fn publish_frozen_source_drift(&self, drift: FrozenSourceDrift) {
        self.inner.write().source_drift.frozen = Some(drift);
    }

    pub fn publish_cache_usage(&self, usage: exomonad_model::ProviderUsageSnapshot) {
        let mut observation = self.inner.write();
        // Replace authoritative aggregates even when the latest response is
        // unchanged: a later durable completion event can settle its scope.
        observation.provider_usage_summary = usage.thread_summary;
        observation.latest_turn_usage_summary = usage.latest_turn_summary;
        let make_sample = |source: exomonad_model::ProviderUsageObservation| ProviderUsageSample {
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
            observation.first_provider_usage = Some(first.clone());
        }
        observation.first_usage_in_summary = observation.provider_usage_summary.is_some()
            && observation
                .first_provider_usage
                .as_ref()
                .is_some_and(|retained| {
                    retained.observation_id == first.observation_id
                        && retained.cached_input_tokens == first.cached_input_tokens
                        && retained.uncached_input_tokens == first.uncached_input_tokens
                });
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

#[cfg(test)]
mod provider_health_tests {
    use super::*;
    use exomonad_model::{ProviderObservation, ProviderTurnObservation, ProviderTurnState};

    #[test]
    fn retirement_candidate_requires_positive_idle_and_no_request() {
        let mut runtime = ActorRuntimeObservation::default();
        assert_eq!(runtime.disposition(false), AgentDisposition::NeedsAttention);
        runtime.provider_turn = Some(ProviderTurnObservation {
            thread: "thread".into(),
            turn: "turn".into(),
            revision: 1,
            state: ProviderTurnState::Active,
        });
        assert_eq!(
            runtime.disposition(false),
            AgentDisposition::SettledAwaitingProvider
        );
        assert_eq!(runtime.disposition(true), AgentDisposition::Working);
        runtime.provider_turn.as_mut().unwrap().state = ProviderTurnState::Succeeded;
        assert_eq!(runtime.disposition(false), AgentDisposition::IdleRetained);
        assert_eq!(runtime.disposition(true), AgentDisposition::Working);
        runtime.provider_observation_stale = true;
        assert_eq!(runtime.disposition(false), AgentDisposition::NeedsAttention);
        runtime.provider_observation_stale = false;
        runtime.provider_turn.as_mut().unwrap().state =
            ProviderTurnState::Failed(exomonad_model::ProviderFailure::RequestRejected);
        assert_eq!(runtime.disposition(true), AgentDisposition::NeedsAttention);
    }

    #[test]
    fn provider_health_preserves_newer_evidence_and_marks_read_failure_stale() {
        let handle = ActorRuntimeObservationHandle::default();
        let observation = |revision, state| ProviderObservation {
            usage: None,
            turn: Some(ProviderTurnObservation {
                thread: "thread".into(),
                turn: "turn".into(),
                revision,
                state,
            }),
            ..Default::default()
        };
        handle.publish_provider_observation(observation(10, ProviderTurnState::Succeeded));
        handle.publish_provider_observation(observation(2, ProviderTurnState::Active));
        assert_eq!(
            handle.snapshot().provider_turn.unwrap().state,
            ProviderTurnState::Succeeded
        );
        handle.mark_provider_observation_stale();
        let stale = handle.snapshot();
        assert!(stale.provider_observation_stale);
        assert_eq!(stale.provider_turn.unwrap().revision, 10);
        handle.publish_provider_observation(ProviderObservation::default());
        assert!(handle.snapshot().provider_observation_stale);
        handle.publish_provider_observation(observation(11, ProviderTurnState::Active));
        assert!(!handle.snapshot().provider_observation_stale);
    }
}

/// Compact elapsed time: `42s`, `27m`, `3h`, `2d`.
fn render_age(since_unix_ms: u64, now_unix_ms: u64) -> String {
    let seconds = now_unix_ms.saturating_sub(since_unix_ms) / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

/// UTC wall-clock time of day, `HH:MM:SSZ`.
fn render_clock(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1000) % 86400;
    format!(
        "{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

pub(crate) fn unix_time_ms() -> u64 {
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
    fn delivery_status_line_names_a_fenced_inbox_and_its_recovery() {
        let minute = 60_000;
        let now = 100 * minute;
        let observation = ActorRuntimeObservation {
            provider_turn: Some(exomonad_model::ProviderTurnObservation {
                thread: "thread".into(),
                turn: "turn-4".into(),
                revision: 4,
                state: exomonad_model::ProviderTurnState::Succeeded,
            }),
            provider_idle_since_unix_ms: Some(now - 27 * minute),
            inbound_delivery: Some(InboundDeliveryObservation {
                inbox: InboxDelivery::Fenced {
                    reason: "message 2 unconfirmed, 6 behind".into(),
                    since_unix_ms: now - 41 * minute,
                },
                last_message: Some(TrackedMessageObservation {
                    sequence: 2,
                    state: TrackedMessageState::Unconfirmed,
                    at_unix_ms: now - 41 * minute,
                }),
                next: InboundNext::Resubmitting,
            }),
            source_drift: ActorSourceDriftObservation {
                checkout: Some(CheckoutGitDrift {
                    head: "8d45d32f00ba".into(),
                    dirty_files: Vec::new(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            observation.delivery_status_line("core-lead", 1, now),
            "core-lead req=1 pending; provider=idle 27m; inbox=fenced(message 2 unconfirmed, 6 behind, 41m); last_message=ref2 unconfirmed/not-presented; source=8d45d32; next=resubmitting"
        );
        let mut terminal = observation.clone();
        if let Some(delivery) = terminal.inbound_delivery.as_mut() {
            delivery.next = InboundNext::NoHostRecovery;
        }
        assert!(terminal
            .delivery_status_line("core-lead", 1, now)
            .ends_with("; next=no host recovery; hand off"));

        let healthy = ActorRuntimeObservation {
            provider_turn: Some(exomonad_model::ProviderTurnObservation {
                thread: "thread".into(),
                turn: "turn-5".into(),
                revision: 5,
                state: exomonad_model::ProviderTurnState::Active,
            }),
            inbound_delivery: Some(InboundDeliveryObservation {
                last_message: Some(TrackedMessageObservation {
                    sequence: 9,
                    state: TrackedMessageState::Presented,
                    at_unix_ms: (3600 + 2 * 60 + 5) * 1000,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            healthy.delivery_status_line("core-lead", 1, now),
            "core-lead req=1 pending; provider=running turn=turn-5; inbox=open; last_message=ref9 presented@01:02:05Z; next=await-event"
        );
    }

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

    fn usage(id: &str, cached: i64, uncached: i64) -> exomonad_model::ProviderUsageSnapshot {
        let observation = exomonad_model::ProviderUsageObservation {
            id: id.into(),
            timestamp: Some("2026-09-05T00:00:00Z".into()),
            usage: exomonad_model::TokenUsage {
                input_tokens: cached + uncached,
                cached_input_tokens: cached,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: cached + uncached,
            },
        };
        exomonad_model::ProviderUsageSnapshot {
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
        assert_eq!(
            observation.snapshot().usage_summary_display(),
            "first_observed=unavailable subsequent_usage=unavailable thread_usage=unavailable"
        );
        observation.publish_cache_usage(usage("first", 0, 12));
        assert_eq!(
            observation.snapshot().usage_summary_display(),
            "first_observed=Partial input=12 cached=0 subsequent_usage=unavailable thread_usage=unavailable"
        );
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
        use exomonad_model::{ProviderUsageCompleteness, ProviderUsageScope, ProviderUsageSummary};
        let observation = ActorRuntimeObservationHandle::default();
        let mut last = usage("last", 80, 20);
        for i in 0..40 {
            observation.publish_cache_usage(usage(&format!("{i}"), 80, 20));
        }
        last.thread_summary = Some(ProviderUsageSummary {
            scope: ProviderUsageScope::Thread("thread".into()),
            completeness: ProviderUsageCompleteness::Partial,
            observations: 100,
            usage: exomonad_model::TokenUsage {
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
            "first_observed=Partial input=100 cached=80 subsequent_usage=unavailable thread_usage=Complete responses=100 cached=8000 uncached=2000"
        );
    }

    #[test]
    fn later_cache_hits_do_not_replace_first_observed_miss_in_status() {
        let observation = ActorRuntimeObservationHandle::default();
        observation.publish_cache_boundary(CacheBoundaryReason::ForkedPrefix);
        observation.publish_cache_usage(usage("first", 0, 100));
        observation.publish_cache_usage(usage("later", 90, 10));
        let snapshot = observation.snapshot();
        assert_eq!(
            snapshot
                .latest_provider_usage()
                .unwrap()
                .cached_input_tokens,
            90
        );
        assert_eq!(
            snapshot.usage_summary_display(),
            "first_observed=Partial input=100 cached=0 subsequent_usage=unavailable thread_usage=unavailable"
        );
    }

    #[test]
    fn subsequent_usage_subtracts_only_the_same_identified_first_response() {
        use exomonad_model::{
            ProviderUsageCompleteness::*, ProviderUsageScope, ProviderUsageSummary,
        };
        let observation = ActorRuntimeObservationHandle::default();
        let mut snapshot = usage("first", 20, 80);
        snapshot.thread_summary = Some(ProviderUsageSummary {
            scope: ProviderUsageScope::Thread("child".into()),
            completeness: Partial,
            observations: 1,
            usage: snapshot.first.usage,
        });
        observation.publish_cache_usage(snapshot.clone());
        assert_eq!(
            observation.snapshot().subsequent_input_usage(),
            Some((Partial, 0, 0, 0))
        );

        snapshot.latest = usage("later", 90, 10).latest;
        let summary = snapshot.thread_summary.as_mut().unwrap();
        summary.observations = 2;
        summary.usage.input_tokens = 200;
        summary.usage.cached_input_tokens = 110;
        observation.publish_cache_usage(snapshot.clone());
        assert_eq!(
            observation.snapshot().subsequent_input_usage(),
            Some((Partial, 1, 90, 10))
        );
        snapshot.thread_summary.as_mut().unwrap().completeness = Complete;
        observation.publish_cache_usage(snapshot.clone());
        assert_eq!(
            observation.snapshot().usage_summary_display(),
            "first_observed=Complete input=100 cached=20 subsequent_usage=Complete responses=1 cached=90 uncached=10 thread_usage=Complete responses=2 cached=110 uncached=90"
        );

        // Equal counts under another source ID do not prove aggregate membership.
        snapshot.first.id = "different-source".into();
        observation.publish_cache_usage(snapshot.clone());
        assert_eq!(observation.snapshot().subsequent_input_usage(), None);
        assert!(observation
            .snapshot()
            .usage_summary_display()
            .starts_with("first_observed=Partial"));
        snapshot.first.id = "first".into();
        snapshot
            .thread_summary
            .as_mut()
            .unwrap()
            .usage
            .cached_input_tokens = 10;
        observation.publish_cache_usage(snapshot);
        assert_eq!(observation.snapshot().subsequent_input_usage(), None);
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

    #[test]
    fn workspace_orientation_distinguishes_storage_from_shared_visible_path() {
        let first = ActorWorkspaceObservation {
            workspace_path: "/tmp/shared-visible-workspace".into(),
            host_storage_path: "/host/worktrees/first".into(),
            worktree_id: Some("first".into()),
            expected_branch: Some("research/first".into()),
        };
        let mut second = first.clone();
        second.host_storage_path = "/host/worktrees/second".into();
        second.worktree_id = Some("second".into());
        second.expected_branch = Some("research/second".into());
        let observation = ActorRuntimeObservationHandle::default();
        assert!(observation.snapshot().workspace.is_none());
        observation.publish_workspace(first.clone());
        let rendered = observation.snapshot().workspace.unwrap().orientation();
        assert!(
            rendered.contains("workspace_path=\"/tmp/shared-visible-workspace\" (native tools)")
        );
        assert!(rendered.contains("host_storage_path=\"/host/worktrees/first\""));
        assert!(rendered.contains("expected_branch=Some(\"research/first\")"));
        assert!(rendered.contains("each actor sees its assigned checkout"));
        observation.publish_workspace(second.clone());
        assert_eq!(observation.snapshot().workspace, Some(second));
        assert_ne!(
            rendered,
            observation.snapshot().workspace.unwrap().orientation()
        );
    }
}
