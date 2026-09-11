//! Backend-neutral ownership seam for long-lived interactive agent applications.
//!
//! This is deliberately separate from [`crate::backend::AgentBackend`]. A
//! headless worker exposes a stepwise turn protocol; an interactive agent owns
//! its native conversation and terminal UI. Tidepool launches and supervises
//! the latter, pushes messages through its supported channel, and services its
//! actor-scoped hosted tools. Pretending those are the same lifecycle would make
//! either side lie.

use std::future::Future;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use sha2::{Digest, Sha256};

use crate::{AgentBackendError, BackendThreadId, ReasoningEffort};

/// Stable producer identity allocated by the host's existing run/inbox owner.
///
/// This value deliberately does not encode actor policy. The host maps its run
/// scope and exact actor incarnation into one bounded identifier before crossing
/// the backend-neutral seam.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputProducerId(String);

impl InputProducerId {
    pub fn new(value: String) -> Result<Self, InputEnvelopeError> {
        if value.is_empty() || value.len() > 512 {
            return Err(InputEnvelopeError::InvalidProducer);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputOperationId {
    pub producer: InputProducerId,
    pub sequence: NonZeroU64,
}

impl InputOperationId {
    /// Bounded native `client_user_message_id` encoding. The host producer is
    /// already an authority-scoped identity; hashing only keeps the transport
    /// key bounded and does not mint or broaden that authority.
    pub fn native_key(&self) -> String {
        let producer = Sha256::digest(self.producer.as_str().as_bytes());
        let mut encoded = String::with_capacity(4 + producer.len() * 2 + 20);
        encoded.push_str("tp1:");
        for byte in producer {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded.push(':');
        encoded.push_str(&self.sequence.to_string());
        encoded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputPurpose {
    Bootstrap,
    Assignment,
    RequestUpdate,
    Notification,
    OperatorInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InteractiveInputMode {
    QueueOnly,
    StartOrSteer,
}

/// Stable target and optional owner correlation frozen before publication.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractiveInputTarget {
    pub conversation: BackendThreadId,
    pub actor: String,
    pub correlation: Option<String>,
}

pub const MAX_INTERACTIVE_INPUT_BYTES: usize = 256 * 1024;

/// One immutable host input. Attempt generations and live binding generations
/// are intentionally absent from the canonical digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveInputEnvelope {
    id: InputOperationId,
    purpose: InputPurpose,
    mode: InteractiveInputMode,
    target: InteractiveInputTarget,
    bytes: Vec<u8>,
    digest: [u8; 32],
}

impl InteractiveInputEnvelope {
    pub fn new(
        id: InputOperationId,
        purpose: InputPurpose,
        mode: InteractiveInputMode,
        target: InteractiveInputTarget,
        bytes: Vec<u8>,
    ) -> Result<Self, InputEnvelopeError> {
        if bytes.len() > MAX_INTERACTIVE_INPUT_BYTES {
            return Err(InputEnvelopeError::PayloadTooLarge {
                actual: bytes.len(),
                limit: MAX_INTERACTIVE_INPUT_BYTES,
            });
        }
        let digest = canonical_input_digest(mode, &target, &bytes);
        Ok(Self {
            id,
            purpose,
            mode,
            target,
            bytes,
            digest,
        })
    }

    pub fn id(&self) -> &InputOperationId {
        &self.id
    }
    pub fn purpose(&self) -> InputPurpose {
        self.purpose
    }
    pub fn mode(&self) -> InteractiveInputMode {
        self.mode
    }
    pub fn target(&self) -> &InteractiveInputTarget {
        &self.target
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Reconstruct persisted input only after verifying its canonical digest.
    /// Native admission uses this boundary before accepting stored or wire data.
    pub fn from_persisted(
        id: InputOperationId,
        purpose: InputPurpose,
        mode: InteractiveInputMode,
        target: InteractiveInputTarget,
        bytes: Vec<u8>,
        persisted_digest: [u8; 32],
    ) -> Result<Self, InputEnvelopeError> {
        let envelope = Self::new(id, purpose, mode, target, bytes)?;
        if envelope.digest != persisted_digest {
            return Err(InputEnvelopeError::DigestMismatch);
        }
        Ok(envelope)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InputEnvelopeError {
    #[error("input producer identity must contain 1..=512 bytes")]
    InvalidProducer,
    #[error("interactive input contains {actual} bytes; limit is {limit}")]
    PayloadTooLarge { actual: usize, limit: usize },
    #[error("persisted interactive input digest does not match canonical content")]
    DigestMismatch,
}

fn canonical_input_digest(
    mode: InteractiveInputMode,
    target: &InteractiveInputTarget,
    bytes: &[u8],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"tidepool-interactive-input-v1\0");
    digest.update([match mode {
        InteractiveInputMode::QueueOnly => 0,
        InteractiveInputMode::StartOrSteer => 1,
    }]);
    digest_field(&mut digest, target.conversation.0.as_bytes());
    digest_field(&mut digest, target.actor.as_bytes());
    match &target.correlation {
        Some(correlation) => {
            digest.update([1]);
            digest_field(&mut digest, correlation.as_bytes());
        }
        None => digest.update([0]),
    }
    digest_field(&mut digest, bytes);
    digest.finalize().into()
}

fn digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InteractiveLaunchId(pub u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativeApplicationInstance(pub u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativeSessionGeneration(pub NonZeroU64);

/// Freshly challenged binding to one exact native application generation.
/// Persisted locators may reconstruct this value only after a new handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveSessionBinding {
    pub launch_id: String,
    pub instance_id: String,
    pub generation: NonZeroU64,
    pub nonce: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAdmission {
    NotSubmitted,
    Admitted,
    Dispatching,
    Presented,
    Withdrawn,
    Rejected,
    /// Native retained evidence was intentionally removed only after a prior
    /// acknowledgement. This fences resubmission but cannot reconstruct the
    /// earlier terminal outcome.
    Compacted,
    Unknown,
}

/// Result of a producer-level native input control operation.
///
/// This is deliberately separate from hosted-call completion acknowledgement:
/// it controls retention of native input outcomes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProducerControlOutcome {
    Applied,
    Rejected,
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum InteractiveInputError {
    #[error("native input was not submitted: {0}")]
    NotSubmitted(String),
    #[error("native input outcome is unconfirmed: {0}")]
    Unconfirmed(String),
}

pub type InteractiveInputFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InputAdmission, InteractiveInputError>> + Send + 'a>>;
pub type InputProducerControlFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<InputProducerControlOutcome, InteractiveInputError>> + Send + 'a,
    >,
>;

/// An interactive conversation whose durable rollout can be addressed by a
/// separate native queue or archive process.
///
/// Only the interactive binding owner can construct this proof after the
/// hosted-session readiness contract has been durably recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReadyThread {
    thread: BackendThreadId,
    input_control_socket: Option<PathBuf>,
    session_binding: Option<InteractiveSessionBinding>,
}

impl QueueReadyThread {
    pub(crate) fn new(thread: BackendThreadId) -> Self {
        Self {
            thread,
            input_control_socket: None,
            session_binding: None,
        }
    }

    pub(crate) fn with_input_control(mut self, socket: Option<PathBuf>) -> Self {
        self.input_control_socket = socket;
        self
    }

    pub(crate) fn input_control_socket(&self) -> Option<&Path> {
        self.input_control_socket.as_deref()
    }

    /// Whether this exact TUI binding advertises normal active-input delivery.
    #[must_use]
    pub fn supports_active_input(&self) -> bool {
        self.input_control_socket.is_some()
    }

    pub fn with_challenged_session_binding(
        mut self,
        binding: Option<InteractiveSessionBinding>,
    ) -> Self {
        self.session_binding = binding;
        self
    }

    pub(crate) fn session_binding(&self) -> Option<&InteractiveSessionBinding> {
        self.session_binding.as_ref()
    }

    #[must_use]
    pub fn id(&self) -> &BackendThreadId {
        &self.thread
    }
}

/// One exact, behaviorally verified interactive-agent installation.
///
/// Shoal resolves this once before it mutates tmux state, then passes the
/// value through its private host-process boundary. Every launch and lifecycle
/// command therefore addresses the same executable rather than consulting
/// `PATH` again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentInstallation {
    executable: PathBuf,
    version: String,
    executable_sha256: String,
    package_root: Option<PathBuf>,
}

impl InteractiveAgentInstallation {
    pub(crate) fn new(
        executable: PathBuf,
        version: String,
        executable_sha256: String,
        package_root: Option<PathBuf>,
    ) -> Self {
        Self {
            executable,
            version,
            executable_sha256,
            package_root,
        }
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// SHA-256 of the exact executable accepted by the capability probes.
    #[must_use]
    pub fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }

    /// Package root when the executable has the conventional
    /// `<package>/bin/<program>` layout. The path itself records whether this
    /// is an immutable store package or a mutable developer installation.
    #[must_use]
    pub fn package_root(&self) -> Option<&Path> {
        self.package_root.as_deref()
    }
}

/// Delivery certainty is control flow: a failure before submission allows the
/// assignment to continue; uncertainty after submission must keep its fence.
/// Details describe this delivery operation, not the original agent run.
#[derive(Debug, thiserror::Error)]
pub enum UpdatePresentationError {
    #[error("update was not submitted: {0}")]
    NotSubmitted(String),
    #[error("update presentation is unconfirmed: {0}")]
    Unconfirmed(String),
}

pub type UpdatePresentationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), UpdatePresentationError>> + Send + 'a>>;

/// A boxed asynchronous operation at the backend-neutral boundary.
pub type InteractiveFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AgentBackendError>> + Send + 'a>>;

/// How a long-lived agent conversation begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveLaunchMode {
    Fresh,
    Resume(BackendThreadId),
    Fork {
        parent: BackendThreadId,
        after_call: String,
    },
}

/// Which layer owns native filesystem containment for an interactive agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveNativeSandbox {
    /// Let the backend confine writes to its workspace.
    BackendWorkspaceWrite,
    /// A validated outer process mount boundary owns containment, so the
    /// backend must not install its conflicting `.git`-protecting sandbox.
    HostMountBoundary,
}

/// Native command policy selected by the actor's effective role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveNativeToolPolicy {
    /// Ordinary coding and orchestration tools are available.
    Standard,
    /// Source inspection remains available, while common build and artifact
    /// producers are rejected by the backend before process execution.
    InspectionOnly,
}

/// One backend-owned policy directory to mount over its ordinary config
/// directory for a single interactive process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractivePolicyMount {
    pub source: PathBuf,
    pub target: PathBuf,
}

/// A backend-rendered interactive process invocation.
///
/// Process ownership stays with the deployment adapter (tmux for Shoal). The
/// backend owns only the exact executable and arguments required by its native
/// client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Whether autonomous goals belong to this interactive process or its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveGoalPolicy {
    /// Preserve the operator's configured autonomous goal behavior.
    Configured,
    /// The host owns assignments and continuation; do not inherit or run goals.
    Disabled,
}

/// Backend-neutral configuration frozen when an interactive process starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveAgentSpec {
    pub mode: InteractiveLaunchMode,
    pub goal_policy: InteractiveGoalPolicy,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    /// Host-owned, immutable base instructions shared across this run.
    /// Must be an absolute path readable in the launched process.
    pub base_instructions_file: PathBuf,
    pub developer_instructions: String,
    /// Optional first user message. Hosted agents do not need a synthetic
    /// message because their session handshake publishes only queue-ready
    /// conversations.
    pub initial_prompt: Option<String>,
    pub native_sandbox: InteractiveNativeSandbox,
    /// Actor-scoped host dynamic tools served over HTTP/1.1 on this Unix
    /// socket. The deployment adapter binds it before calling `render`.
    pub host_tools_socket: PathBuf,
}

/// Operations a concrete interactive-agent adapter must provide.
///
/// The composition root owns processes and durable delivery. `render` is pure
/// command construction; `push` is only the final backend hop to an
/// already-bound exact conversation.
pub trait InteractiveAgentBackend: Send + Sync {
    /// Execute and control a command through the already-bound native process owner.
    fn command<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _id: &'a str,
        _operation: NativeCommandOperation,
    ) -> InteractiveFuture<'a, NativeCommandReply> {
        Box::pin(async {
            Err(AgentBackendError::ProtocolRejected {
                detail: "native commands are unsupported".into(),
            })
        })
    }

    /// Bind the already host-challenged native generation before any input
    /// operation. This is the native half of the existing attach, not another
    /// challenge or conversation handshake.
    fn bind_input<'a>(&'a self, _thread: &'a QueueReadyThread) -> InteractiveInputFuture<'a> {
        Box::pin(async {
            Err(InteractiveInputError::NotSubmitted(
                "bound native input control is unavailable".into(),
            ))
        })
    }

    fn submit_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _envelope: &'a InteractiveInputEnvelope,
    ) -> InteractiveInputFuture<'a> {
        Box::pin(async {
            Err(InteractiveInputError::NotSubmitted(
                "bound native input control is unavailable".into(),
            ))
        })
    }

    fn query_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _id: &'a InputOperationId,
    ) -> InteractiveInputFuture<'a> {
        Box::pin(async {
            Err(InteractiveInputError::NotSubmitted(
                "bound native input control is unavailable".into(),
            ))
        })
    }

    fn withdraw_input<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _id: &'a InputOperationId,
    ) -> InteractiveInputFuture<'a> {
        Box::pin(async {
            Err(InteractiveInputError::NotSubmitted(
                "bound native input control is unavailable".into(),
            ))
        })
    }

    fn seal_input_producer<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _producer: &'a InputProducerId,
    ) -> InputProducerControlFuture<'a> {
        Box::pin(async {
            Err(InteractiveInputError::NotSubmitted(
                "bound native input control is unavailable".into(),
            ))
        })
    }

    fn acknowledge_input_prefix<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _producer: &'a InputProducerId,
        _through_sequence: NonZeroU64,
    ) -> InputProducerControlFuture<'a> {
        Box::pin(async {
            Err(InteractiveInputError::NotSubmitted(
                "bound native input control is unavailable".into(),
            ))
        })
    }
    /// Control a process-owned workspace publication lease. Transport errors are
    /// unconfirmed: retain the same durable sequence until it can be reconciled.
    fn workspace_publication<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
        _sequence: std::num::NonZeroU64,
        _operation: PublicationOperation,
    ) -> InteractiveFuture<'a, PublicationReply> {
        Box::pin(async {
            Ok(PublicationReply::Unavailable {
                detail: "workspace publication is unsupported".into(),
            })
        })
    }
    /// Materialize backend-native command policy under `staging_root`.
    ///
    /// The deployment owner installs returned directories as read-only mount
    /// overlays for this process only. The standard policy needs no overlay.
    fn prepare_native_tool_policy(
        &self,
        policy: InteractiveNativeToolPolicy,
        staging_root: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError>;

    fn render(
        &self,
        spec: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError>;

    fn push<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
        message: &'a str,
    ) -> InteractiveFuture<'a, ()>;

    /// Present an update in the existing conversation at a safe model boundary,
    /// waking it if idle. Success requires observed model-visible insertion with
    /// the supplied correlation key, not merely acceptance into a queue.
    fn present_update<'a>(
        &'a self,
        _cwd: &'a str,
        _thread: &'a QueueReadyThread,
        _key: &'a str,
        _message: &'a str,
    ) -> UpdatePresentationFuture<'a> {
        Box::pin(async {
            Err(UpdatePresentationError::NotSubmitted(
                "this backend does not support confirmed active updates".into(),
            ))
        })
    }

    /// Usage and provider turn health from one durable observation.
    /// `None` means unavailable, never measured zero or an idle process.
    fn observe<'a>(
        &'a self,
        _thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, Option<tidepool_model::ProviderObservation>> {
        Box::pin(async { Ok(None) })
    }

    fn archive<'a>(
        &'a self,
        cwd: &'a str,
        thread: &'a QueueReadyThread,
    ) -> InteractiveFuture<'a, ()>;
}

#[derive(Clone, Debug)]
pub enum NativeCommandOperation {
    Start(tidepool_bridge_effects::CommandSpec),
    Wait,
    Output(usize),
    Read {
        stream: tidepool_bridge_effects::CommandStream,
        position: tidepool_bridge_effects::CommandPosition,
    },
    Input(String),
    CloseInput,
    Resize {
        rows: u16,
        columns: u16,
    },
    Cancel,
}

#[derive(Clone, Debug)]
pub enum NativeCommandReply {
    Pending,
    Finished { exit_code: i32, cancelled: bool },
    Unconfirmed(String),
    Output(tidepool_bridge_effects::CommandOutput),
    Page(tidepool_bridge_effects::CommandPage),
    Acknowledged,
}

#[derive(Clone, Copy, Debug)]
pub enum PublicationOperation {
    Begin {
        expected: Option<PublicationIdentity>,
    },
    Finish {
        expected: PublicationIdentity,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicationIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub mount_namespace_inode: u64,
}

#[derive(Debug)]
pub enum PublicationReply {
    Ready {
        /// Socket owner PID in the host's namespace; `pid` is native-local.
        peer_pid: u32,
        pid: u32,
        start_ticks: u64,
        mount_namespace_inode: u64,
        cgroup_path: PathBuf,
    },
    Settled,
    Busy,
    Conflict,
    Unavailable {
        detail: String,
    },
}

#[cfg(test)]
mod input_envelope_tests {
    use super::*;

    fn envelope(mode: InteractiveInputMode, bytes: &[u8]) -> InteractiveInputEnvelope {
        InteractiveInputEnvelope::new(
            InputOperationId {
                producer: InputProducerId::new("run-7/inbox-2/actor-3.1".into()).unwrap(),
                sequence: NonZeroU64::new(9).unwrap(),
            },
            InputPurpose::Assignment,
            mode,
            InteractiveInputTarget {
                conversation: BackendThreadId("thread-4".into()),
                actor: "actor-3.1".into(),
                correlation: Some("request-5".into()),
            },
            bytes.to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn canonical_digest_fixes_mode_target_and_frozen_bytes() {
        let original = envelope(InteractiveInputMode::QueueOnly, b"hello");
        assert_eq!(
            original.digest(),
            envelope(InteractiveInputMode::QueueOnly, b"hello").digest()
        );
        assert_ne!(
            original.digest(),
            envelope(InteractiveInputMode::StartOrSteer, b"hello").digest()
        );
        assert_ne!(
            original.digest(),
            envelope(InteractiveInputMode::QueueOnly, b"hello!").digest()
        );
    }

    #[test]
    fn producer_scope_is_part_of_identity_not_content_digest() {
        let original = envelope(InteractiveInputMode::QueueOnly, b"hello");
        let second = InteractiveInputEnvelope::new(
            InputOperationId {
                producer: InputProducerId::new("another-run/inbox-2/actor-3.1".into()).unwrap(),
                sequence: original.id().sequence,
            },
            original.purpose(),
            original.mode(),
            original.target().clone(),
            original.bytes().to_vec(),
        )
        .unwrap();
        assert_ne!(original.id(), second.id());
        assert_ne!(original.id().native_key(), second.id().native_key());
        assert_eq!(original.digest(), second.digest());
    }

    #[test]
    fn persisted_reconstruction_rejects_noncanonical_content() {
        let original = envelope(InteractiveInputMode::QueueOnly, b"hello");
        let error = InteractiveInputEnvelope::from_persisted(
            original.id().clone(),
            original.purpose(),
            InteractiveInputMode::StartOrSteer,
            original.target().clone(),
            original.bytes().to_vec(),
            *original.digest(),
        )
        .unwrap_err();
        assert_eq!(error, InputEnvelopeError::DigestMismatch);
    }

    #[test]
    fn payload_bound_is_enforced_before_publication() {
        let error = InteractiveInputEnvelope::new(
            InputOperationId {
                producer: InputProducerId::new("run/inbox/actor".into()).unwrap(),
                sequence: NonZeroU64::new(1).unwrap(),
            },
            InputPurpose::Bootstrap,
            InteractiveInputMode::QueueOnly,
            InteractiveInputTarget {
                conversation: BackendThreadId("thread".into()),
                actor: "actor".into(),
                correlation: None,
            },
            vec![0; MAX_INTERACTIVE_INPUT_BYTES + 1],
        )
        .unwrap_err();
        assert!(matches!(error, InputEnvelopeError::PayloadTooLarge { .. }));
    }
}
