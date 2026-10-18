//! Durable actor identity and lifecycle evidence for host reconstruction.
//!
//! This journal records only Rust-owned facts. Live Haskell values, mailbox
//! payloads, continuations, requests, and watches remain intentionally absent:
//! a restarted host reports those as lost instead of pretending to serialize
//! the resident heap.

use crate::{ActorDescriptor, ActorPlacement, ActorRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tidepool_atomic_write::DirectoryAnchor;
use tidepool_repr::jsonl::{JsonlReadError, SyncPolicy, TailPolicy};

// V12 retains exact immutable model authority. Earlier runs require their matching binaries.
const VERSION: u32 = 12;

/// Issued by the Forest after checking its existing descriptor and directory.
/// The process-local placement is deliberately absent from durable rows.
#[derive(Clone, Debug)]
pub struct RootRecoveryPlacement {
    actor: ActorRef,
    owner: tidepool_runtime::session::RecoveryPublicOwner,
    placement: ActorPlacement,
}

impl RootRecoveryPlacement {
    pub(crate) fn new(
        actor: ActorRef,
        owner: tidepool_runtime::session::RecoveryPublicOwner,
        placement: ActorPlacement,
    ) -> Self {
        Self {
            actor,
            owner,
            placement,
        }
    }
    pub fn actor(&self) -> ActorRef {
        self.actor
    }
    pub fn owner(&self) -> &tidepool_runtime::session::RecoveryPublicOwner {
        &self.owner
    }
    pub fn placement(&self) -> ActorPlacement {
        self.placement
    }
    pub fn matches(
        &self,
        session: tidepool_repr::SessionId,
        target: tidepool_codegen::scope::ScopeId,
    ) -> bool {
        self.placement.session == session && self.placement.lexical_scope == target
    }
}

/// Opaque proof of the actual journal's complete predecessor and durably
/// prepared successor. A copied admission DTO cannot create this receipt.
pub struct DurableRootSuccessorAdmission {
    operation: u64,
    path: PathBuf,
    predecessor_owner: tidepool_runtime::session::RecoveryPublicOwner,
    store_predecessor: Option<ApplicationConversation>,
    intended_conversation: Option<ApplicationConversation>,
    manifest: Option<RootStartupManifestPin>,
    predecessor: ActorRef,
    successor: RootRecoveryPlacement,
}

impl DurableRootSuccessorAdmission {
    pub fn operation(&self) -> u64 {
        self.operation
    }
    pub fn predecessor(&self) -> ActorRef {
        self.predecessor
    }
    pub fn successor(&self) -> &RootRecoveryPlacement {
        &self.successor
    }
    /// Validate the exact facts captured by journal certification. Current
    /// transition authority remains with the native owner.
    pub fn validate_embedded_binding(
        &self,
        run_root: &Path,
        predecessor: &ApplicationConversation,
        successor: &ApplicationConversation,
    ) -> std::io::Result<bool> {
        let canonical = run_root.canonicalize()?;
        if self.path.canonicalize()?.parent() != Some(canonical.as_path()) {
            return Ok(false);
        }
        Ok(self.store_predecessor.as_ref() == Some(predecessor)
            && self.intended_conversation.as_ref() == Some(successor)
            && matches!((predecessor, successor),
                (ApplicationConversation::Embedded {run: old_run, agent_path: old_path, ..},
                 ApplicationConversation::Embedded {run: new_run, agent_path: new_path, ..})
                 if old_run == new_run && old_path == new_path))
    }
    /// Match immutable certification to the still-owned binding operation.
    /// Native transfer has already consumed the predecessor manifest owner;
    /// lifecycle settlement separately checks committed successor readiness.
    pub fn validate_binding_claim(
        &self,
        run_root: &Path,
        claim: &crate::RootBindingOperationClaim,
    ) -> std::io::Result<bool> {
        let Ok(predecessor) = claim.predecessor_owner() else {
            return Ok(false);
        };
        if self.operation != claim.operation()
            || self.successor.actor() != claim.successor_actor()
            || self.successor.placement() != claim.successor_placement()
        {
            return Ok(false);
        }
        self.validate_successor_identity(
            run_root,
            predecessor,
            claim.successor_owner(),
            claim.successor_placement().session,
            claim.successor_placement().lexical_scope,
        )
    }

    pub fn validate_successor(
        &self,
        run_root: &Path,
        predecessor: &tidepool_runtime::session::RecoveryPublicOwner,
        successor: &tidepool_runtime::session::RecoveryPublicOwner,
        session: tidepool_repr::SessionId,
        target: tidepool_codegen::scope::ScopeId,
    ) -> std::io::Result<bool> {
        if !self.validate_successor_identity(run_root, predecessor, successor, session, target)? {
            return Ok(false);
        }
        let manifest = self.manifest.as_ref().ok_or_else(|| {
            std::io::Error::other("successor startup has no exact manifest revision pin")
        })?;
        manifest.validate(&run_root.canonicalize()?)
    }

    fn validate_successor_identity(
        &self,
        run_root: &Path,
        predecessor: &tidepool_runtime::session::RecoveryPublicOwner,
        successor: &tidepool_runtime::session::RecoveryPublicOwner,
        session: tidepool_repr::SessionId,
        target: tidepool_codegen::scope::ScopeId,
    ) -> std::io::Result<bool> {
        if !self.successor.matches(session, target) || self.successor.owner() != successor {
            return Ok(false);
        }
        let run_root = run_root.canonicalize()?;
        if self
            .path
            .parent()
            .map(Path::canonicalize)
            .transpose()?
            .as_ref()
            != Some(&run_root)
            || self.path.canonicalize()?
                != run_root.join(
                    self.path
                        .file_name()
                        .ok_or_else(|| std::io::Error::other("journal has no file name"))?,
                )
        {
            return Ok(false);
        }
        Ok(&self.predecessor_owner == predecessor)
    }
}

fn owner_for_admission(
    admission: &DurableActorAdmission,
) -> Option<tidepool_runtime::session::RecoveryPublicOwner> {
    let rendered = admission.actor_path.as_ref()?;
    let path = tidepool_repr::ActorPath::parse(rendered).ok()?;
    if path.to_string().as_str() != rendered.as_str() {
        return None;
    }
    tidepool_runtime::session::RecoveryPublicOwner::new(&path, admission.actor.incarnation.0)
}

fn is_independent_root(admission: &DurableActorAdmission) -> bool {
    admission.creator.is_none()
        && admission.supervisor_parent.is_none()
        && admission.context_parent.is_none()
        && owner_for_admission(admission).is_some()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableActorAdmission {
    pub actor: ActorRef,
    /// Authored text; display fallback never becomes durable authored metadata.
    pub label: Option<String>,
    pub creator: Option<ActorRef>,
    pub supervisor_parent: Option<ActorRef>,
    pub context_parent: Option<ActorRef>,
    pub actor_path: Option<String>,
    pub effect_keys: Vec<crate::ActorEffectKey>,
    pub model_permissions: crate::ModelPermissions,
    pub conversation_permission: Option<crate::ConversationPermission>,
    #[serde(default)]
    pub descendant_depth: u16,
    #[serde(default)]
    pub descendant_active_children: Option<u16>,
    pub model: Option<String>,
    pub reasoning: Option<crate::Reasoning>,
    pub instructions: Option<String>,
    pub launch_worktrees: Vec<String>,
    pub source_layer: Vec<PathBuf>,
}

impl DurableActorAdmission {
    #[must_use]
    pub fn display_label(&self) -> &str {
        self.label
            .as_deref()
            .or(self.actor_path.as_deref())
            .unwrap_or_default()
    }

    fn capture(actor: ActorRef, descriptor: &ActorDescriptor, launch_worktrees: &[String]) -> Self {
        let capabilities = descriptor.capabilities();
        let descendants = capabilities.descendants();
        Self {
            actor,
            label: descriptor.authored_label().map(str::to_owned),
            creator: descriptor.creator(),
            supervisor_parent: descriptor.supervisor_parent(),
            context_parent: descriptor.context_parent(),
            actor_path: descriptor.actor_path().map(ToString::to_string),
            effect_keys: capabilities.effect_keys().to_vec(),
            model_permissions: capabilities.model_permissions().clone(),
            conversation_permission: capabilities.conversation_permission().cloned(),
            descendant_depth: descendants.maximum_depth,
            descendant_active_children: descendants.maximum_active_children,
            model: descriptor.model_name().map(str::to_owned),
            reasoning: descriptor.reasoning(),
            instructions: descriptor.instructions().map(str::to_owned),
            launch_worktrees: launch_worktrees.to_vec(),
            source_layer: descriptor.source_layer().to_vec(),
        }
    }
}

/// Persist the original bounded lifecycle result, without a second projection.
/// Old V5 terminals default the absent diagnostic; older readers reject the
/// additive diagnostic field under their owned record's unknown-field policy.
pub type DurableActorTerminal = crate::ActorTerminal;

/// Pins the exact root domain and public owner observed at startup. Sibling
/// machine publications do not alter this domain's identity or revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootStartupManifestPin {
    pub path: PathBuf,
    pub database_id: [u8; 16],
    pub domain_id: tidepool_runtime::session::RecoveryDomainId,
    pub domain: tidepool_runtime::session::RecoveryDomainState,
    pub public_owner: tidepool_runtime::session::RecoveryPublicOwnerState,
}

impl RootStartupManifestPin {
    pub fn capture_for_actor(
        path: &Path,
        actor: &tidepool_runtime::session::RecoveryPublicOwner,
    ) -> std::io::Result<Self> {
        Self::observe_for_actor(path, actor)?
            .ok_or_else(|| std::io::Error::other("root recovery public owner is absent"))
    }

    /// Absence is allowed only when an admitted actor has no published surface.
    /// A selected generation with missing or unreadable evidence is refused.
    pub fn observe_for_actor(
        path: &Path,
        actor: &tidepool_runtime::session::RecoveryPublicOwner,
    ) -> std::io::Result<Option<Self>> {
        let path = path.canonicalize()?;
        let Some(snapshot) =
            tidepool_runtime::session::RunRecoveryStore::read_selected_snapshot_at(
                &path,
                tidepool_runtime::session::RecoveryDomainId::Root,
                actor,
            )
            .map_err(std::io::Error::other)?
        else {
            return Ok(None);
        };
        if snapshot.public_owner.is_none() {
            return Ok(None);
        }
        Self::from_snapshot(path, snapshot).map(Some)
    }

    fn from_snapshot(
        path: PathBuf,
        snapshot: tidepool_runtime::session::recovery_store::RecoverySnapshotIdentity,
    ) -> std::io::Result<Self> {
        let public_owner = snapshot
            .public_owner
            .ok_or_else(|| std::io::Error::other("root recovery public owner is absent"))?;
        Ok(Self {
            path,
            database_id: snapshot.database_id,
            domain_id: snapshot.domain_id,
            domain: snapshot.domain,
            public_owner,
        })
    }

    fn same_public_content(&self, other: &Self) -> bool {
        self.path == other.path
            && self.database_id == other.database_id
            && self.domain_id == other.domain_id
            && self.domain.lineage == other.domain.lineage
            && self.domain.content_revision == other.domain.content_revision
            && self.domain.allocation_ceiling == other.domain.allocation_ceiling
            && self.domain.snapshot_root == other.domain.snapshot_root
            && self.public_owner.surface_root == other.public_owner.surface_root
    }

    fn validate(&self, run_root: &Path) -> std::io::Result<bool> {
        if self.domain_id != tidepool_runtime::session::RecoveryDomainId::Root
            || self.path.canonicalize()?.parent() != Some(run_root.canonicalize()?.as_path())
        {
            return Ok(false);
        }
        Ok(&Self::capture_for_actor(&self.path, &self.public_owner.owner.actor_owner())? == self)
    }
}

/// Exact root activation intent, recorded in the same durable row as admission.
/// Manifest and backend owners may differ after an interrupted transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootStartupIntent {
    pub bootstrap_identity: String,
    pub predecessor: Option<ActorRef>,
    pub manifest_predecessor: Option<ActorRef>,
    pub manifest: Option<RootStartupManifestPin>,
    pub store_predecessor: Option<ApplicationConversation>,
    pub binding_path: PathBuf,
    pub accepted_source: Option<String>,
    pub conversation: ApplicationConversation,
}

impl RootStartupIntent {
    fn application(&self) -> DurableActorApplication {
        DurableActorApplication {
            binding_path: self.binding_path.clone(),
            conversation: None,
            intended_conversation: Some(self.conversation.clone()),
            accepted_source: self.accepted_source.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableActorRecord {
    pub admission: DurableActorAdmission,
    pub startup: Option<RootStartupIntent>,
    pub application: Option<DurableActorApplication>,
    pub terminal: Option<DurableActorTerminal>,
    pub initial_context: Option<crate::InitialContextObservation>,
}

/// Exact retained backend identity; possession grants no live actor authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationConversation {
    Codex {
        thread_id: String,
    },
    Embedded {
        run: String,
        agent_path: String,
        incarnation: String,
    },
}
impl ApplicationConversation {
    pub fn codex_thread(&self) -> Option<&str> {
        match self {
            Self::Codex { thread_id } => Some(thread_id),
            _ => None,
        }
    }
}
impl From<String> for ApplicationConversation {
    fn from(thread_id: String) -> Self {
        Self::Codex { thread_id }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableActorApplication {
    pub binding_path: PathBuf,
    pub conversation: Option<ApplicationConversation>,
    #[serde(default)]
    pub intended_conversation: Option<ApplicationConversation>,
    pub accepted_source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum EventKind {
    Created {},
    Admitted {
        admission: Box<DurableActorAdmission>,
        startup: Option<RootStartupIntent>,
    },
    ApplicationPrepared {
        actor: ActorRef,
        binding_path: PathBuf,
        #[serde(default)]
        accepted_source: Option<String>,
        #[serde(default)]
        intended_conversation: Option<ApplicationConversation>,
    },
    ApplicationBound {
        actor: ActorRef,
        conversation: ApplicationConversation,
    },
    Retired {
        actor: ActorRef,
        terminal: DurableActorTerminal,
    },
    InitialContext {
        actor: ActorRef,
        change: crate::initial_context::InitialContextEvent,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    version: u32,
    sequence: u64,
    #[serde(flatten)]
    event: EventKind,
}

struct State {
    next_sequence: u64,
    records: BTreeMap<ActorRef, DurableActorRecord>,
    uncertain: bool,
}

/// Exact evidence of a durably confirmed journal binding, not native binding
/// authority. Only the verified enclosing bind operation may issue a final
/// lifecycle settlement receipt.
#[derive(Debug)]
pub struct JournalApplicationBindingAck {
    issuer: Arc<()>,
    actor: ActorRef,
    conversation: ApplicationConversation,
    confirmed_sequence: u64,
}
impl JournalApplicationBindingAck {
    pub fn actor(&self) -> ActorRef {
        self.actor
    }
    pub fn conversation(&self) -> &ApplicationConversation {
        &self.conversation
    }
    /// The committed prefix confirming the fact; an idempotent bind need not
    /// append a new row.
    pub fn confirmed_sequence(&self) -> u64 {
        self.confirmed_sequence
    }
    pub fn belongs_to(&self, journal: &ActorRecoveryJournal) -> bool {
        Arc::ptr_eq(&self.issuer, &journal.identity)
    }
}

#[cfg(test)]
struct BootstrapPause {
    entered: tokio::sync::oneshot::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

/// Trusted host composition consumes its exclusive boot lease before sharing it.
/// This public adapter is a trust boundary, not a sealed proof against arbitrary
/// external implementations. Production supplies the actual host lease here.
pub trait RecoveryBootRegistration: Send + Sync {
    fn into_run(self: Box<Self>) -> Arc<dyn tidepool_runtime::session::RecoveryRunAuthority>;
}

/// Affine registration of the replay prefix preceding this host's first append.
pub struct JournalBootRegistration {
    issuer: Arc<()>,
    actors: Vec<ActorRef>,
    run_root: PathBuf,
    run: Arc<dyn tidepool_runtime::session::RecoveryRunAuthority>,
}

impl JournalBootRegistration {
    pub(crate) fn run_authority(&self) -> Arc<dyn tidepool_runtime::session::RecoveryRunAuthority> {
        self.run.clone()
    }
    pub(crate) fn run_root(&self) -> &Path {
        &self.run_root
    }
    pub(crate) fn qualifies(&self, actor: ActorRef, journal: &ActorRecoveryJournal) -> bool {
        Arc::ptr_eq(&self.issuer, &journal.identity) && self.actors.contains(&actor)
    }
    pub(crate) fn matches(&self, journal: &ActorRecoveryJournal) -> bool {
        Arc::ptr_eq(&self.issuer, &journal.identity)
    }
}

/// A channel handle to one dedicated, supervised blocking journal owner.
pub struct ActorRecoveryJournal {
    identity: Arc<()>,
    commands: std::sync::mpsc::Sender<Command>,
    status: tokio::sync::watch::Receiver<JournalOwnerStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalOwnerStatus {
    Running,
    Fenced,
    Stopped { clean: bool, fenced: bool },
}

impl ActorRecoveryJournal {
    #[cfg(test)]
    pub(crate) async fn pause_owner_for_test(&self) -> std::sync::mpsc::Sender<()> {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        self.commands
            .send(Command::Pause {
                entered: entered_tx,
                resume: paused,
            })
            .expect("journal owner must accept test pause");
        entered.await.expect("journal owner must enter test pause");
        resume
    }

    async fn start(
        anchor: DirectoryAnchor,
        relative_file: PathBuf,
        require_existing: bool,
        lifetime: Arc<dyn Send + Sync>,
        #[cfg(test)] bootstrap_pause: Option<BootstrapPause>,
    ) -> std::io::Result<Arc<Self>> {
        Self::start_bootstrap(
            anchor,
            relative_file,
            require_existing,
            lifetime,
            None,
            #[cfg(test)]
            bootstrap_pause,
        )
        .await
        .map(|(journal, _)| journal)
    }

    async fn start_bootstrap(
        anchor: DirectoryAnchor,
        relative_file: PathBuf,
        require_existing: bool,
        lifetime: Arc<dyn Send + Sync>,
        boot: Option<Arc<dyn tidepool_runtime::session::RecoveryRunAuthority>>,
        #[cfg(test)] bootstrap_pause: Option<BootstrapPause>,
    ) -> std::io::Result<(Arc<Self>, Vec<ActorRef>)> {
        let identity = Arc::new(());
        let owner_identity = Arc::clone(&identity);
        let (commands, incoming) = std::sync::mpsc::channel();
        let (initialized, ready) = tokio::sync::oneshot::channel();
        let (status_tx, status) = tokio::sync::watch::channel(JournalOwnerStatus::Running);
        std::thread::Builder::new()
            .name("actor-journal-owner".into())
            .spawn(move || {
                // The existing enclosing resource owner survives a cancelled
                // opening waiter and remains held while queued writes drain.
                let _lifetime = lifetime;
                #[cfg(test)]
                if let Some(pause) = bootstrap_pause {
                    let _ = pause.entered.send(());
                    let _ = pause.resume.recv();
                }
                let mut fenced = false;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Some(run) = &boot {
                        match run.owns_run(anchor.path()) {
                            Ok(true) => {}
                            Ok(false) => {
                                let _ = initialized.send(Err(std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    "boot lease does not own journal run",
                                )));
                                return false;
                            }
                            Err(error) => {
                                let _ = initialized.send(Err(error));
                                return false;
                            }
                        }
                    }
                    let mut owner = match JournalOwner::initialize(
                        &anchor,
                        &relative_file,
                        require_existing,
                        owner_identity,
                    ) {
                        Ok(owner) => {
                            let prefix = if boot.is_some() {
                                owner.state.records.keys().copied().collect()
                            } else {
                                Vec::new()
                            };
                            let _ = initialized.send(Ok(prefix));
                            owner
                        }
                        Err(error) => {
                            let _ = initialized.send(Err(error));
                            return false;
                        }
                    };
                    while let Ok(command) = incoming.recv() {
                        macro_rules! complete {
                            ($reply:expr, $operation:expr) => {{
                                let outcome = $operation;
                                fenced = owner.state.uncertain;
                                if owner.state.uncertain {
                                    status_tx.send_replace(JournalOwnerStatus::Fenced);
                                }
                                let _ = $reply.send(outcome);
                            }};
                        }
                        match command {
                            #[cfg(test)]
                            Command::Pause { entered, resume } => {
                                let _ = entered.send(());
                                let _ = resume.recv();
                            }
                            #[cfg(test)]
                            Command::Crash { reply: _reply } => {
                                panic!("injected journal owner crash")
                            }
                            Command::Records { reply } => {
                                complete!(reply, Ok(owner.records()));
                            }
                            Command::ValidatedRecords { reply } => {
                                complete!(reply, owner.validated_records());
                            }
                            Command::Conversation { actor, reply } => {
                                complete!(reply, owner.active_application_conversation(actor));
                            }
                            Command::InitialContext { actor, reply } => {
                                complete!(reply, owner.initial_context(actor));
                            }
                            Command::RecordInitialContext {
                                actor,
                                change,
                                reply,
                            } => {
                                complete!(reply, owner.record_initial_context(actor, change));
                            }
                            Command::PublicationMatches {
                                actor,
                                operation,
                                conversation,
                                reply,
                            } => {
                                complete!(
                                    reply,
                                    owner.initial_context_publication_matches(
                                        actor,
                                        &operation,
                                        &conversation
                                    )
                                );
                            }
                            Command::Admit {
                                admission,
                                startup,
                                reply,
                            } => {
                                complete!(reply, owner.admit_with_startup(admission, startup));
                            }
                            Command::Retire {
                                actor,
                                terminal,
                                reply,
                            } => {
                                complete!(reply, owner.retire(actor, terminal));
                            }
                            Command::Prepare {
                                actor,
                                binding_path,
                                accepted_source,
                                intended_conversation,
                                reply,
                            } => {
                                complete!(
                                    reply,
                                    owner.prepare_application_with_intent(
                                        actor,
                                        binding_path,
                                        accepted_source,
                                        intended_conversation
                                    )
                                );
                            }
                            Command::Bind {
                                actor,
                                conversation,
                                reply,
                            } => {
                                complete!(
                                    reply,
                                    owner.bind_application_conversation(actor, conversation)
                                );
                            }
                            Command::Certify {
                                claimed_predecessor,
                                operation,
                                predecessor,
                                successor,
                                expected_source,
                                canonical_binding_path,
                                reply,
                            } => {
                                complete!(
                                    reply,
                                    owner.certify(
                                        claimed_predecessor,
                                        operation,
                                        predecessor,
                                        successor,
                                        expected_source,
                                        canonical_binding_path
                                    )
                                );
                            }
                        }

                        if owner.state.uncertain {
                            status_tx.send_replace(JournalOwnerStatus::Fenced);
                        }
                    }
                    true
                }));
                let release = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    drop(boot);
                    drop(_lifetime);
                }));
                status_tx.send_replace(JournalOwnerStatus::Stopped {
                    clean: matches!(outcome, Ok(true)) && release.is_ok(),
                    fenced,
                });
                if outcome.is_err() || release.is_err() {
                    tracing::error!("actor journal owner terminated unexpectedly");
                }
            })?;
        let journal = Arc::new(Self {
            identity,
            commands,
            status,
        });
        let prefix = ready.await.map_err(|_| {
            std::io::Error::other("actor journal owner terminated during initialization")
        })??;
        Ok((journal, prefix))
    }
    /// Exact process-local writer endpoint identity, independent of file names.
    pub fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
    pub fn owner_status(&self) -> tokio::sync::watch::Receiver<JournalOwnerStatus> {
        self.status.clone()
    }
    async fn request<T: Send>(
        &self,
        command: impl FnOnce(tokio::sync::oneshot::Sender<std::io::Result<T>>) -> Command + Send,
    ) -> std::io::Result<T> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| std::io::Error::other("actor journal owner unavailable"))?;
        result.await.map_err(|_| {
            std::io::Error::other("actor journal owner terminated without completion")
        })?
    }
    /// Consumes the host's one-shot boot lease and freezes the initial replay prefix.
    pub async fn open_for_boot(
        anchor: &DirectoryAnchor,
        relative_file: impl AsRef<Path>,
        require_existing: bool,
        registration: Box<dyn RecoveryBootRegistration>,
    ) -> std::io::Result<(Arc<Self>, JournalBootRegistration)> {
        let run = registration.into_run();
        let run_root = anchor.path().to_owned();
        let lifetime: Arc<dyn Send + Sync> = run.clone();
        let (journal, actors) = Self::start_bootstrap(
            anchor.clone(),
            relative_file.as_ref().to_owned(),
            require_existing,
            lifetime,
            Some(run.clone()),
            #[cfg(test)]
            None,
        )
        .await?;
        let registration = JournalBootRegistration {
            issuer: journal.identity.clone(),
            actors,
            run_root,
            run,
        };
        Ok((journal, registration))
    }
    pub async fn open(
        anchor: &DirectoryAnchor,
        relative_file: impl AsRef<Path>,
        lifetime: Arc<dyn Send + Sync>,
    ) -> std::io::Result<Arc<Self>> {
        Self::start(
            anchor.clone(),
            relative_file.as_ref().to_owned(),
            false,
            lifetime,
            #[cfg(test)]
            None,
        )
        .await
    }
    pub async fn open_existing(
        anchor: &DirectoryAnchor,
        relative_file: impl AsRef<Path>,
        lifetime: Arc<dyn Send + Sync>,
    ) -> std::io::Result<Arc<Self>> {
        Self::start(
            anchor.clone(),
            relative_file.as_ref().to_owned(),
            true,
            lifetime,
            #[cfg(test)]
            None,
        )
        .await
    }
    /// Blocking offline filesystem observation; this never addresses a live
    /// journal owner or repairs its file.
    pub fn read_observed(path: &std::path::Path) -> std::io::Result<Vec<DurableActorRecord>> {
        let (rows, _torn) = tidepool_repr::jsonl::read_tail(
            path,
            |line| parse_row(line).map_err(|error| error.to_string()),
            TailPolicy::Observe,
        )
        .map_err(journal_read_error)?;
        Ok(replay(rows)?.0.into_values().collect())
    }
    /// Explicit full inventory with linear copying cost. Selected hot-path
    /// queries return only their named actor facts.
    pub async fn records(&self) -> std::io::Result<Vec<DurableActorRecord>> {
        self.request(|reply| Command::Records { reply }).await
    }
    pub async fn validated_records(&self) -> std::io::Result<Vec<DurableActorRecord>> {
        self.request(|reply| Command::ValidatedRecords { reply })
            .await
    }
    pub async fn active_application_conversation(
        &self,
        actor: ActorRef,
    ) -> std::io::Result<Option<ApplicationConversation>> {
        self.request(|reply| Command::Conversation { actor, reply })
            .await
    }
    pub async fn initial_context(
        &self,
        actor: ActorRef,
    ) -> std::io::Result<Option<crate::InitialContextObservation>> {
        self.request(|reply| Command::InitialContext { actor, reply })
            .await
    }
    pub(crate) async fn record_initial_context(
        &self,
        actor: ActorRef,
        change: crate::initial_context::InitialContextEvent,
    ) -> std::io::Result<crate::InitialContextObservation> {
        self.request(|reply| Command::RecordInitialContext {
            actor,
            change,
            reply,
        })
        .await
    }
    pub(crate) async fn initial_context_publication_matches(
        &self,
        actor: ActorRef,
        operation: &tidepool_runtime::session::WorkbenchExecutionId,
        conversation: &ApplicationConversation,
    ) -> std::io::Result<bool> {
        self.request(|reply| Command::PublicationMatches {
            actor,
            operation: operation.clone(),
            conversation: conversation.clone(),
            reply,
        })
        .await
    }
    pub(crate) async fn retire(
        &self,
        actor: ActorRef,
        terminal: crate::ActorTerminal,
    ) -> std::io::Result<()> {
        self.request(|reply| Command::Retire {
            actor,
            terminal,
            reply,
        })
        .await
    }
    pub async fn prepare_application_with_intent(
        &self,
        actor: ActorRef,
        binding_path: PathBuf,
        accepted_source: Option<String>,
        intended_conversation: Option<ApplicationConversation>,
    ) -> std::io::Result<()> {
        self.request(|reply| Command::Prepare {
            actor,
            binding_path,
            accepted_source,
            intended_conversation,
            reply,
        })
        .await
    }
    pub async fn bind_application_conversation(
        &self,
        actor: ActorRef,
        conversation: ApplicationConversation,
    ) -> std::io::Result<JournalApplicationBindingAck> {
        self.request(|reply| Command::Bind {
            actor,
            conversation,
            reply,
        })
        .await
    }
    pub async fn certify_root_successor(
        &self,
        predecessor: ActorRef,
        successor: RootRecoveryPlacement,
        expected_source: Option<&str>,
        canonical_binding_path: &Path,
        claim: &crate::RootBindingOperationClaim,
    ) -> std::io::Result<Arc<DurableRootSuccessorAdmission>> {
        if !claim.journal_matches(self)
            || successor.actor() != claim.successor_actor()
            || successor.placement() != claim.successor_placement()
            || successor.owner() != claim.successor_owner()
        {
            return Err(std::io::Error::other(
                "journal successor differs from its exact lifecycle execution claim",
            ));
        }
        let claimed_predecessor = claim
            .predecessor_owner()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?
            .clone();
        self.request(|reply| Command::Certify {
            claimed_predecessor,
            operation: claim.operation(),
            predecessor,
            successor,
            expected_source: expected_source.map(str::to_owned),
            canonical_binding_path: canonical_binding_path.to_owned(),
            reply,
        })
        .await
    }
    pub(crate) async fn admit(
        &self,
        actor: ActorRef,
        descriptor: &ActorDescriptor,
        launch_worktrees: &[String],
    ) -> std::io::Result<()> {
        self.admit_with_startup(actor, descriptor, launch_worktrees, None, None)
            .await
    }
    pub(crate) async fn admit_with_startup(
        &self,
        actor: ActorRef,
        descriptor: &ActorDescriptor,
        launch_worktrees: &[String],
        startup: Option<RootStartupIntent>,
        admission: Option<&crate::local_actor::run_control::RootStartupAdmission>,
    ) -> std::io::Result<()> {
        if let Some(startup) = startup.as_ref() {
            let permit = admission.ok_or_else(|| {
                std::io::Error::other("startup journal admission requires its root owner permit")
            })?;
            permit
                .validate_journal_admission(actor, startup, self)
                .map_err(std::io::Error::other)?;
        }
        let admission = DurableActorAdmission::capture(actor, descriptor, launch_worktrees);
        self.request(|reply| Command::Admit {
            admission,
            startup,
            reply,
        })
        .await
    }
    pub async fn prepare_application(
        &self,
        actor: ActorRef,
        binding_path: PathBuf,
        accepted_source: Option<String>,
    ) -> std::io::Result<()> {
        self.prepare_application_with_intent(actor, binding_path, accepted_source, None)
            .await
    }
    pub async fn bind_application(
        &self,
        actor: ActorRef,
        thread_id: String,
    ) -> std::io::Result<JournalApplicationBindingAck> {
        self.bind_application_conversation(actor, ApplicationConversation::Codex { thread_id })
            .await
    }
}

enum Command {
    #[cfg(test)]
    Pause {
        entered: tokio::sync::oneshot::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    },
    #[cfg(test)]
    Crash {
        reply: tokio::sync::oneshot::Sender<()>,
    },
    Records {
        reply: tokio::sync::oneshot::Sender<std::io::Result<Vec<DurableActorRecord>>>,
    },
    ValidatedRecords {
        reply: tokio::sync::oneshot::Sender<std::io::Result<Vec<DurableActorRecord>>>,
    },
    Conversation {
        actor: ActorRef,
        reply: tokio::sync::oneshot::Sender<std::io::Result<Option<ApplicationConversation>>>,
    },
    InitialContext {
        actor: ActorRef,
        reply:
            tokio::sync::oneshot::Sender<std::io::Result<Option<crate::InitialContextObservation>>>,
    },
    RecordInitialContext {
        actor: ActorRef,
        change: crate::initial_context::InitialContextEvent,
        reply: tokio::sync::oneshot::Sender<std::io::Result<crate::InitialContextObservation>>,
    },
    PublicationMatches {
        actor: ActorRef,
        operation: tidepool_runtime::session::WorkbenchExecutionId,
        conversation: ApplicationConversation,
        reply: tokio::sync::oneshot::Sender<std::io::Result<bool>>,
    },
    Admit {
        admission: DurableActorAdmission,
        startup: Option<RootStartupIntent>,
        reply: tokio::sync::oneshot::Sender<std::io::Result<()>>,
    },
    Retire {
        actor: ActorRef,
        terminal: crate::ActorTerminal,
        reply: tokio::sync::oneshot::Sender<std::io::Result<()>>,
    },
    Prepare {
        actor: ActorRef,
        binding_path: PathBuf,
        accepted_source: Option<String>,
        intended_conversation: Option<ApplicationConversation>,
        reply: tokio::sync::oneshot::Sender<std::io::Result<()>>,
    },
    Bind {
        actor: ActorRef,
        conversation: ApplicationConversation,
        reply: tokio::sync::oneshot::Sender<std::io::Result<JournalApplicationBindingAck>>,
    },
    Certify {
        claimed_predecessor: tidepool_runtime::session::RecoveryPublicOwner,
        operation: u64,
        predecessor: ActorRef,
        successor: RootRecoveryPlacement,
        expected_source: Option<String>,
        canonical_binding_path: PathBuf,
        reply: tokio::sync::oneshot::Sender<std::io::Result<Arc<DurableRootSuccessorAdmission>>>,
    },
}

struct JournalOwner {
    identity: Arc<()>,
    path: PathBuf,
    state: State,
}
impl JournalOwner {
    fn initialize(
        anchor: &DirectoryAnchor,
        relative_file: &Path,
        require_existing: bool,
        identity: Arc<()>,
    ) -> std::io::Result<Self> {
        let path = anchor
            .resolve(relative_file)
            .map_err(std::io::Error::from)?;
        if let Some(parent) = relative_file.parent() {
            anchor
                .create_dir_all(parent)
                .map_err(std::io::Error::from)?;
        }
        let existed = path.try_exists()?;
        if require_existing && !existed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "recovered host has no actor lifecycle journal",
            ));
        }
        let (rows, torn) = tidepool_repr::jsonl::read_tail(
            &path,
            |line| parse_row(line).map_err(|error| error.to_string()),
            TailPolicy::Repair,
        )
        .map_err(journal_read_error)?;
        if let Some(torn) = torn {
            tracing::warn!(path = %path.display(), line = torn.line_no, reason = %torn.reason,
                "repaired torn actor lifecycle journal tail");
        }
        let (records, expected, created) = replay(rows)?;
        if existed && !created {
            return Err(std::io::Error::other(
                "actor lifecycle journal lacks a durable creation marker",
            ));
        }
        let mut owner = JournalOwner {
            identity,
            path: path.clone(),
            state: State {
                next_sequence: expected,
                records,
                uncertain: false,
            },
        };
        if !existed {
            append(&path, &mut owner.state, EventKind::Created {})?;
        }
        Ok(owner)
    }

    fn certify(
        &mut self,
        claimed_predecessor: tidepool_runtime::session::RecoveryPublicOwner,
        operation: u64,
        predecessor: ActorRef,
        successor: RootRecoveryPlacement,
        source: Option<String>,
        binding_path: PathBuf,
    ) -> std::io::Result<Arc<DurableRootSuccessorAdmission>> {
        self.validate_root_successor(predecessor, &successor, source.as_deref(), &binding_path)?;
        let old = self
            .state
            .records
            .get(&predecessor)
            .expect("validated predecessor");
        let new = self
            .state
            .records
            .get(&successor.actor)
            .expect("validated successor");
        let startup = new.startup.as_ref().expect("validated startup");
        let selected = startup.manifest.as_ref().map(|pin| &pin.public_owner.owner);
        if owner_for_admission(&old.admission).as_ref() != Some(&claimed_predecessor.actor_owner())
            || selected != Some(&claimed_predecessor)
        {
            return Err(std::io::Error::other(
                "journal predecessor differs from its exact lifecycle execution claim",
            ));
        }
        let application = new.application.as_ref().expect("validated application");
        Ok(Arc::new(DurableRootSuccessorAdmission {
            operation,
            path: self.path.clone(),
            predecessor,
            successor,
            predecessor_owner: claimed_predecessor,
            store_predecessor: startup.store_predecessor.clone(),
            intended_conversation: application.intended_conversation.clone(),
            manifest: startup.manifest.clone(),
        }))
    }

    fn validate_root_successor(
        &mut self,
        predecessor: ActorRef,
        successor: &RootRecoveryPlacement,
        expected_source: Option<&str>,
        binding_path: &Path,
    ) -> std::io::Result<()> {
        let records = self.validated_state_records()?;
        let old = records
            .get(&predecessor)
            .ok_or_else(|| std::io::Error::other("root predecessor admission is absent"))?;
        let new = records
            .get(&successor.actor)
            .ok_or_else(|| std::io::Error::other("root successor admission is absent"))?;
        let is_root = |record: &DurableActorRecord| is_independent_root(&record.admission);
        if !is_root(old)
            || !is_root(new)
            || new.terminal.is_some()
            || new.startup.as_ref().is_none_or(|intent| {
                intent.manifest_predecessor != Some(predecessor)
                    || intent.predecessor.is_none_or(|previous| {
                        !startup_chain_contains(&records, previous, predecessor)
                    })
            })
            || owner_for_admission(&new.admission).as_ref() != Some(successor.owner())
            || old.admission.actor_path != new.admission.actor_path
            || records.values().any(|record| {
                record
                    .startup
                    .as_ref()
                    .is_some_and(|intent| intent.predecessor == Some(successor.actor))
            })
        {
            return Err(std::io::Error::other(
                "root successor is not the exact latest admitted application owner",
            ));
        }
        let old_app = old
            .application
            .as_ref()
            .ok_or_else(|| std::io::Error::other("root predecessor has no prepared application"))?;
        let new_app = new.application.as_ref().ok_or_else(|| {
            std::io::Error::other("root successor has no durably prepared application")
        })?;
        if old_app.accepted_source.as_deref() != expected_source
            || new_app.accepted_source.as_deref() != expected_source
            || old_app.binding_path != binding_path
            || new_app.binding_path != binding_path
        {
            return Err(std::io::Error::other(
                "root successor application or accepted source differs from its predecessor",
            ));
        }
        Ok(())
    }
    fn validated_state_records(
        &mut self,
    ) -> std::io::Result<BTreeMap<ActorRef, DurableActorRecord>> {
        ensure_writable(&self.state)?;
        let observed = (|| {
            let (rows, torn) = tidepool_repr::jsonl::read_tail(
                &self.path,
                |line| parse_row(line).map_err(|error| error.to_string()),
                TailPolicy::Observe,
            )
            .map_err(journal_read_error)?;
            let (records, sequence, created) = replay(rows)?;
            if torn.is_some()
                || !created
                || sequence != self.state.next_sequence
                || records != self.state.records
            {
                return Err(std::io::Error::other(
                    "root successor journal changed or has uncertain durable evidence",
                ));
            }
            Ok(records)
        })();
        if observed.is_err() {
            self.state.uncertain = true;
        }
        observed
    }
    fn validated_records(&mut self) -> std::io::Result<Vec<DurableActorRecord>> {
        Ok(self.validated_state_records()?.into_values().collect())
    }
    fn active_application_conversation(
        &mut self,
        actor: ActorRef,
    ) -> std::io::Result<Option<ApplicationConversation>> {
        let state = &self.state;
        ensure_writable(state)?;
        let Some(record) = state.records.get(&actor) else {
            return Ok(None);
        };
        if record.terminal.is_some() {
            return Ok(None);
        }
        Ok(record
            .application
            .as_ref()
            .and_then(|app| app.conversation.clone()))
    }
    fn records(&mut self) -> Vec<DurableActorRecord> {
        self.state.records.values().cloned().collect()
    }
    fn initial_context(
        &mut self,
        actor: ActorRef,
    ) -> std::io::Result<Option<crate::InitialContextObservation>> {
        let state = &self.state;
        ensure_writable(&state)?;
        let mut next = Some(actor);
        while let Some(owner) = next {
            let record = state.records.get(&owner).ok_or_else(|| {
                std::io::Error::other("initial context actor has no durable admission")
            })?;
            if let Some(mut observation) = record.initial_context.clone() {
                if owner != actor && observation.state == crate::InitialContextState::Preparing {
                    observation.state = crate::InitialContextState::RecoveryRequired {
                        detail: "host loss interrupted initialization; retained effects must not be replayed".into(),
                    };
                }
                return Ok(Some(observation));
            }
            next = record
                .startup
                .as_ref()
                .and_then(|intent| intent.predecessor);
        }
        Ok(None)
    }
    fn record_initial_context(
        &mut self,
        actor: ActorRef,
        change: crate::initial_context::InitialContextEvent,
    ) -> std::io::Result<crate::InitialContextObservation> {
        let state = &mut self.state;
        ensure_writable(&state)?;
        validate_initial_context_event(actor, &change)?;
        let record = state.records.get(&actor).ok_or_else(|| {
            std::io::Error::other("initial context actor has no durable admission")
        })?;
        let mut next = record.initial_context.clone();
        crate::initial_context::apply_event(&mut next, change.clone())?;
        if next != record.initial_context {
            append(
                &self.path,
                state,
                EventKind::InitialContext { actor, change },
            )?;
            state
                .records
                .get_mut(&actor)
                .expect("validated actor record")
                .initial_context = next.clone();
        }
        next.ok_or_else(|| {
            std::io::Error::other("initial context transition produced no observation")
        })
    }
    fn initial_context_publication_matches(
        &mut self,
        actor: ActorRef,
        operation: &tidepool_runtime::session::WorkbenchExecutionId,
        conversation: &ApplicationConversation,
    ) -> std::io::Result<bool> {
        let state = &self.state;
        ensure_writable(&state)?;
        let current = state
            .records
            .get(&actor)
            .ok_or_else(|| std::io::Error::other("publication actor has no durable admission"))?;
        let mut next = Some(actor);
        while let Some(owner) = next {
            let record = state.records.get(&owner).ok_or_else(|| {
                std::io::Error::other("publication startup predecessor is absent")
            })?;
            if let Some(initial) = &record.initial_context {
                if initial
                    .identity
                    .as_ref()
                    .is_none_or(|identity| &identity.operation != operation)
                {
                    return Ok(false);
                }
                return Ok(current
                    .application
                    .as_ref()
                    .and_then(|application| application.conversation.as_ref())
                    == Some(conversation)
                    || record
                        .application
                        .as_ref()
                        .and_then(|application| application.conversation.as_ref())
                        == Some(conversation));
            }
            next = record
                .startup
                .as_ref()
                .and_then(|intent| intent.predecessor);
        }
        Ok(false)
    }
    fn retire(&mut self, actor: ActorRef, terminal: crate::ActorTerminal) -> std::io::Result<()> {
        let terminal = terminal.bound_diagnostic();
        let state = &mut self.state;
        ensure_writable(&state)?;
        let record = state.records.get(&actor).ok_or_else(|| {
            std::io::Error::other(format!("actor {actor} retired without durable admission"))
        })?;
        match &record.terminal {
            Some(existing) if existing == &terminal => return Ok(()),
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor {actor} retired with a conflicting terminal disposition"
                )));
            }
            None => {}
        }
        append(
            &self.path,
            state,
            EventKind::Retired {
                actor,
                terminal: terminal.clone(),
            },
        )?;
        state
            .records
            .get_mut(&actor)
            .ok_or_else(|| std::io::Error::other("actor admission disappeared during retirement"))?
            .terminal = Some(terminal);
        Ok(())
    }
    fn prepare_application_with_intent(
        &mut self,
        actor: ActorRef,
        binding_path: PathBuf,
        accepted_source: Option<String>,
        intended_conversation: Option<ApplicationConversation>,
    ) -> std::io::Result<()> {
        let state = &mut self.state;
        ensure_writable(&state)?;
        let record = state.records.get(&actor).ok_or_else(|| {
            std::io::Error::other(format!(
                "application for actor {actor} has no durable admission"
            ))
        })?;
        if record.terminal.is_some() {
            return Err(std::io::Error::other(
                "terminal actor cannot prepare an application",
            ));
        }
        validate_application_intent(&record.admission, intended_conversation.as_ref())?;
        match &record.application {
            Some(existing)
                if existing.binding_path == binding_path
                    && existing.intended_conversation == intended_conversation
                    && existing.accepted_source == accepted_source =>
            {
                return Ok(());
            }
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor {actor} reused its application identity with changed parameters"
                )));
            }
            None => {}
        }
        append(
            &self.path,
            state,
            EventKind::ApplicationPrepared {
                actor,
                binding_path: binding_path.clone(),
                accepted_source: accepted_source.clone(),
                intended_conversation: intended_conversation.clone(),
            },
        )?;
        state
            .records
            .get_mut(&actor)
            .ok_or_else(|| {
                std::io::Error::other("actor admission disappeared during application preparation")
            })?
            .application = Some(DurableActorApplication {
            binding_path,
            conversation: None,
            intended_conversation,
            accepted_source,
        });
        Ok(())
    }
    fn bind_application_conversation(
        &mut self,
        actor: ActorRef,
        conversation: ApplicationConversation,
    ) -> std::io::Result<JournalApplicationBindingAck> {
        let state = &mut self.state;
        ensure_writable(&state)?;
        if state
            .records
            .get(&actor)
            .is_some_and(|record| record.terminal.is_some())
        {
            return Err(std::io::Error::other(
                "terminal actor cannot publish a conversation binding",
            ));
        }
        let application = state
            .records
            .get(&actor)
            .and_then(|record| record.application.as_ref())
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "binding for actor {actor} precedes durable application preparation"
                ))
            })?;
        validate_application_binding(application, &conversation)?;
        match &application.conversation {
            Some(existing) if existing == &conversation => {
                return Ok(JournalApplicationBindingAck {
                    issuer: Arc::clone(&self.identity),
                    actor,
                    conversation,
                    confirmed_sequence: state.next_sequence - 1,
                })
            }
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor {actor} published conflicting conversation identities"
                )));
            }
            None => {}
        }
        append(
            &self.path,
            state,
            EventKind::ApplicationBound {
                actor,
                conversation: conversation.clone(),
            },
        )?;
        state
            .records
            .get_mut(&actor)
            .and_then(|record| record.application.as_mut())
            .ok_or_else(|| {
                std::io::Error::other("application disappeared during binding publication")
            })?
            .conversation = Some(conversation.clone());
        Ok(JournalApplicationBindingAck {
            issuer: Arc::clone(&self.identity),
            actor,
            conversation,
            confirmed_sequence: state.next_sequence - 1,
        })
    }
    fn admit_with_startup(
        &mut self,
        admission: DurableActorAdmission,
        startup: Option<RootStartupIntent>,
    ) -> std::io::Result<()> {
        let actor = admission.actor;
        let state = &mut self.state;
        ensure_writable(&state)?;
        match state.records.get(&actor) {
            Some(existing)
                if existing.admission == admission
                    && existing.startup == startup
                    && existing.terminal.is_none() =>
            {
                return Ok(());
            }
            Some(_) => {
                return Err(std::io::Error::other(format!(
                    "actor identity {actor} was reused with different durable parameters"
                )));
            }
            None => {}
        }
        validate_startup(&state.records, &admission, startup.as_ref())?;
        append(
            &self.path,
            state,
            EventKind::Admitted {
                admission: Box::new(admission.clone()),
                startup: startup.clone(),
            },
        )?;
        state.records.insert(
            actor,
            DurableActorRecord {
                admission,
                application: startup.as_ref().map(RootStartupIntent::application),
                startup,
                terminal: None,
                initial_context: None,
            },
        );
        Ok(())
    }
}
fn journal_read_error(error: JsonlReadError) -> std::io::Error {
    match error {
        JsonlReadError::Io(error) => error,
        malformed @ JsonlReadError::MalformedRow { .. } => {
            std::io::Error::new(std::io::ErrorKind::InvalidData, malformed)
        }
    }
}

fn append(path: &Path, state: &mut State, event: EventKind) -> std::io::Result<()> {
    let row = Row {
        version: VERSION,
        sequence: state.next_sequence,
        event,
    };
    let encoded = serde_json::to_string(&row).map_err(std::io::Error::other)?;
    if let Err(error) = tidepool_repr::jsonl::append_new_line(path, &encoded, SyncPolicy::All) {
        // The row may be visible. This handle cannot safely append again;
        // restart and exclusive replay are the only reconciliation path.
        state.uncertain = true;
        return Err(error);
    }
    state.next_sequence += 1;
    Ok(())
}
fn validate_startup(
    records: &BTreeMap<ActorRef, DurableActorRecord>,
    admission: &DurableActorAdmission,
    startup: Option<&RootStartupIntent>,
) -> std::io::Result<()> {
    let Some(startup) = startup else {
        return Ok(());
    };
    if startup.bootstrap_identity.is_empty() {
        return Err(std::io::Error::other(
            "startup requires its exact compiled bootstrap identity",
        ));
    }
    if startup.manifest_predecessor.is_some() != startup.manifest.is_some() {
        return Err(std::io::Error::other(
            "startup manifest predecessor and exact revision pin must be paired",
        ));
    }
    if !is_independent_root(admission)
        || records
            .values()
            .any(|record| owner_for_admission(&record.admission) == owner_for_admission(admission))
    {
        return Err(std::io::Error::other(
            "startup intent requires a canonical independent root",
        ));
    }
    validate_application_intent(admission, Some(&startup.conversation))?;
    if let Some(previous) = startup.predecessor {
        let prior = records
            .get(&previous)
            .ok_or_else(|| std::io::Error::other("startup predecessor is absent"))?;
        if previous == admission.actor
            || owner_for_admission(&prior.admission) == owner_for_admission(admission)
            || prior.admission.actor_path != admission.actor_path
            || !is_independent_root(&prior.admission)
            || records.values().any(|record| {
                record
                    .startup
                    .as_ref()
                    .is_some_and(|intent| intent.predecessor == Some(previous))
            })
        {
            return Err(std::io::Error::other(
                "startup predecessor is not the exact unsucceeded root",
            ));
        }
        let original_bootstrap = prior
            .startup
            .as_ref()
            .map(|intent| intent.bootstrap_identity.as_str());
        if original_bootstrap != Some(startup.bootstrap_identity.as_str()) {
            return Err(std::io::Error::other(format!(
                "startup compiled bootstrap differs from original predecessor: original={original_bootstrap:?}, observed={}",
                startup.bootstrap_identity,
            )));
        }
        let prior_application = prior
            .application
            .as_ref()
            .ok_or_else(|| std::io::Error::other("startup predecessor application is absent"))?;
        if prior_application.conversation.is_none() {
            if let Some(original) = prior
                .startup
                .as_ref()
                .and_then(|intent| intent.manifest.as_ref())
            {
                if startup
                    .manifest
                    .as_ref()
                    .is_none_or(|manifest| !manifest.same_public_content(original))
                {
                    return Err(std::io::Error::other(
                        "unactivated startup rollforward changed original public content",
                    ));
                }
            }
        }
        if prior_application.binding_path != startup.binding_path
            || prior_application.accepted_source != startup.accepted_source
        {
            return Err(std::io::Error::other(
                "startup successor changed binding or source",
            ));
        }
        if let Some(owner) = startup.manifest_predecessor {
            if records
                .get(&owner)
                .and_then(|record| owner_for_admission(&record.admission))
                .as_ref()
                != startup
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.public_owner.owner.actor_owner())
                    .as_ref()
            {
                return Err(std::io::Error::other(
                    "startup manifest pin owner differs from admitted predecessor",
                ));
            }
            if !startup_chain_contains(records, previous, owner) {
                return Err(std::io::Error::other(
                    "manifest owner is outside the exact startup chain",
                ));
            }
        }
        if let Some(owner) = &startup.store_predecessor {
            if !startup_chain_conversations(records, previous)
                .iter()
                .any(|candidate| *candidate == owner)
            {
                return Err(std::io::Error::other(
                    "Store owner is outside the exact startup chain",
                ));
            }
        }
    } else if startup.manifest_predecessor.is_some()
        || startup.store_predecessor.is_some()
        || records.values().any(|record| record.startup.is_some())
    {
        return Err(std::io::Error::other(
            "initial startup cannot replace existing startup evidence",
        ));
    }
    Ok(())
}

fn startup_chain_contains(
    records: &BTreeMap<ActorRef, DurableActorRecord>,
    mut from: ActorRef,
    target: ActorRef,
) -> bool {
    loop {
        if from == target {
            return true;
        }
        let Some(record) = records.get(&from) else {
            return false;
        };
        // A bound root may have run effects: never walk past it.
        if record
            .application
            .as_ref()
            .is_some_and(|application| application.conversation.is_some())
        {
            return false;
        }
        let Some(previous) = record
            .startup
            .as_ref()
            .and_then(|startup| startup.predecessor)
        else {
            return false;
        };
        from = previous;
    }
}

fn startup_chain_conversations(
    records: &BTreeMap<ActorRef, DurableActorRecord>,
    mut from: ActorRef,
) -> Vec<&ApplicationConversation> {
    let mut result = Vec::new();
    while let Some(record) = records.get(&from) {
        if let Some(application) = &record.application {
            if let Some(conversation) = application
                .conversation
                .as_ref()
                .or(application.intended_conversation.as_ref())
            {
                result.push(conversation);
            }
            if application.conversation.is_some() {
                break;
            }
        }
        let Some(previous) = record
            .startup
            .as_ref()
            .and_then(|startup| startup.predecessor)
        else {
            break;
        };
        from = previous;
    }
    result
}

fn validate_application_intent(
    admission: &DurableActorAdmission,
    intent: Option<&ApplicationConversation>,
) -> std::io::Result<()> {
    if let Some(ApplicationConversation::Embedded {
        run,
        agent_path,
        incarnation,
    }) = intent
    {
        let root = is_independent_root(admission);
        let child_suffix = format!(
            "/a{}_i{}",
            admission.actor.id.0, admission.actor.incarnation.0
        );
        if run.is_empty()
            || incarnation != &admission.actor.incarnation.0.to_string()
            || if root {
                agent_path != "/root"
            } else {
                !agent_path.starts_with('/') || !agent_path.ends_with(&child_suffix)
            }
        {
            return Err(std::io::Error::other(
                "embedded application intent differs from admitted actor identity",
            ));
        }
    }
    Ok(())
}

fn validate_application_binding(
    application: &DurableActorApplication,
    conversation: &ApplicationConversation,
) -> std::io::Result<()> {
    if application
        .intended_conversation
        .as_ref()
        .is_some_and(|expected| expected != conversation)
        || matches!(conversation, ApplicationConversation::Embedded { .. })
            && application.intended_conversation.is_none()
    {
        return Err(std::io::Error::other(
            "conversation binding differs from durable prepared intent",
        ));
    }
    Ok(())
}

fn ensure_writable(state: &State) -> std::io::Result<()> {
    if state.uncertain {
        Err(std::io::Error::other(
            "actor lifecycle journal has an uncertain append; reopen it exclusively",
        ))
    } else {
        Ok(())
    }
}

type Replayed = (BTreeMap<ActorRef, DurableActorRecord>, u64, bool);

/// Validates row order and the creation marker, returning the records, the
/// next sequence, and whether the creation marker was present.
fn replay(rows: Vec<Row>) -> std::io::Result<Replayed> {
    let mut expected = 1;
    let mut records = BTreeMap::new();
    let mut created = false;
    for row in rows {
        if row.sequence != expected {
            return Err(std::io::Error::other(format!(
                "actor lifecycle journal sequence mismatch: expected {expected}, found {}",
                row.sequence
            )));
        }
        expected += 1;
        match row.event {
            EventKind::Created {} if !created => created = true,
            EventKind::Created {} => {
                return Err(std::io::Error::other(
                    "duplicate actor lifecycle journal creation marker",
                ));
            }
            event if created => apply_event(&mut records, event)?,
            _ => {
                return Err(std::io::Error::other(
                    "actor lifecycle event precedes durable creation marker",
                ));
            }
        }
    }
    Ok((records, expected, created))
}

fn apply_event(
    records: &mut BTreeMap<ActorRef, DurableActorRecord>,
    event: EventKind,
) -> std::io::Result<()> {
    match event {
        EventKind::Created {} => {
            return Err(std::io::Error::other(
                "actor lifecycle creation marker reached record replay",
            ));
        }
        EventKind::Admitted { admission, startup } => {
            let admission = *admission;
            let actor = admission.actor;
            validate_startup(records, &admission, startup.as_ref())?;
            if records
                .insert(
                    actor,
                    DurableActorRecord {
                        admission,
                        application: startup.as_ref().map(RootStartupIntent::application),
                        startup,
                        terminal: None,
                        initial_context: None,
                    },
                )
                .is_some()
            {
                return Err(std::io::Error::other(format!(
                    "duplicate durable admission for actor {actor}"
                )));
            }
        }
        EventKind::ApplicationPrepared {
            actor,
            binding_path,
            accepted_source,
            intended_conversation,
        } => {
            let record = records.get_mut(&actor).ok_or_else(|| {
                std::io::Error::other(format!(
                    "application row precedes admission for actor {actor}"
                ))
            })?;
            validate_application_intent(&record.admission, intended_conversation.as_ref())?;
            if record.terminal.is_some() {
                return Err(std::io::Error::other(
                    "terminal actor has prepared application row",
                ));
            }
            if record
                .application
                .replace(DurableActorApplication {
                    binding_path,
                    conversation: None,
                    intended_conversation,
                    accepted_source,
                })
                .is_some()
            {
                return Err(std::io::Error::other(format!(
                    "duplicate application row for actor {actor}"
                )));
            }
        }
        EventKind::ApplicationBound {
            actor,
            conversation,
        } => {
            if records
                .get(&actor)
                .is_some_and(|record| record.terminal.is_some())
            {
                return Err(std::io::Error::other(
                    "terminal actor has conversation binding row",
                ));
            }
            let application = records
                .get_mut(&actor)
                .and_then(|record| record.application.as_mut())
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "binding row precedes application preparation for actor {actor}"
                    ))
                })?;
            validate_application_binding(application, &conversation)?;
            if application.conversation.replace(conversation).is_some() {
                return Err(std::io::Error::other(format!(
                    "duplicate binding row for actor {actor}"
                )));
            }
        }
        EventKind::Retired { actor, terminal } => {
            let record = records.get_mut(&actor).ok_or_else(|| {
                std::io::Error::other(format!("terminal row precedes admission for actor {actor}"))
            })?;
            if record.terminal.replace(terminal).is_some() {
                return Err(std::io::Error::other(format!(
                    "duplicate terminal row for actor {actor}"
                )));
            }
        }
        EventKind::InitialContext { actor, change } => {
            validate_initial_context_event(actor, &change)?;
            let record = records.get_mut(&actor).ok_or_else(|| {
                std::io::Error::other("initial context event precedes actor admission")
            })?;
            crate::initial_context::apply_event(&mut record.initial_context, change)?;
        }
    }
    Ok(())
}

fn validate_initial_context_event(
    actor: ActorRef,
    change: &crate::initial_context::InitialContextEvent,
) -> std::io::Result<()> {
    if let crate::initial_context::InitialContextEvent::Begin { identity, .. } = change {
        if identity.actor != actor
            || identity.operation.as_str().is_empty()
            || identity.spec_install == 0
            || identity.spec_source.is_empty()
        {
            return Err(std::io::Error::other(
                "initial context identity differs from its issuing actor/install",
            ));
        }
    }
    Ok(())
}

fn parse_row(line: &str) -> Result<Row, Box<dyn std::error::Error + Send + Sync>> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(VERSION)) {
        return Err("unsupported actor journal version; use its matching binary".into());
    }
    Ok(serde_json::from_value(value)?)
}

#[cfg(test)]
pub(crate) mod recovery_test_fixture {
    use super::*;
    use tidepool_runtime::session::{
        RecoveryDomainId, RecoveryRunAuthority, RunRecoveryStore, SessionLib,
    };

    pub(super) struct LockedRun {
        root: PathBuf,
        _lock: std::fs::File,
    }
    impl RecoveryRunAuthority for LockedRun {
        fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
            Ok(root.canonicalize()? == self.root)
        }
    }

    impl RecoveryBootRegistration for LockedRun {
        fn into_run(self: Box<Self>) -> Arc<dyn RecoveryRunAuthority> {
            Arc::new(*self)
        }
    }
    pub(super) fn boot_registration(root: &Path) -> Box<dyn RecoveryBootRegistration> {
        let root = root.canonicalize().unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("recovery-owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        Box::new(LockedRun { root, _lock: lock })
    }

    #[derive(Clone)]
    pub(crate) struct RecoveryFixture {
        pub(crate) store: Arc<RunRecoveryStore>,
        pub(super) authority: Arc<LockedRun>,
    }
    impl RecoveryFixture {
        pub(crate) fn create(root: &Path) -> Self {
            let root = root.canonicalize().unwrap();
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(root.join("recovery-owner.lock"))
                .unwrap();
            lock.try_lock().unwrap();
            let authority = Arc::new(LockedRun {
                root: root.clone(),
                _lock: lock,
            });
            assert!(authority.owns_run(&root).unwrap());
            Self {
                store: Arc::new(RunRecoveryStore::create(&root.join("recovery.sqlite")).unwrap()),
                authority,
            }
        }
        pub(crate) fn reopen(&self) -> Self {
            assert!(self.authority.owns_run(&self.authority.root).unwrap());
            Self {
                store: Arc::new(RunRecoveryStore::open(self.store.path()).unwrap()),
                authority: Arc::clone(&self.authority),
            }
        }
        pub(crate) fn attach_root(
            &self,
            lib: &mut SessionLib,
            settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
        ) {
            self.attach(lib, RecoveryDomainId::Root, settlement);
        }
        pub(crate) fn attach(
            &self,
            lib: &mut SessionLib,
            domain: RecoveryDomainId,
            settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
        ) {
            lib.attach_owned_recovery_domain(
                Arc::clone(&self.store),
                domain,
                lib.include_dir().canonicalize().unwrap(),
                self.authority.clone(),
                settlement,
            )
            .unwrap();
        }
        pub(crate) fn fail_commits(&self) {
            rusqlite::Connection::open(self.store.path()).unwrap().execute_batch(
                // A deferred constraint succeeds during UPDATE and fails at
                // COMMIT, so the real store must fence its held run handle.
                "CREATE TABLE recovery_test_deferred_failure (domain BLOB REFERENCES recovery_domains(domain) DEFERRABLE INITIALLY DEFERRED);
                 CREATE TRIGGER recovery_test_commit_failure BEFORE UPDATE ON recovery_domains BEGIN INSERT INTO recovery_test_deferred_failure(domain) VALUES(X'FFFFFFFFFFFFFFFFFF'); END;"
            ).unwrap();
        }
        pub(crate) fn refuse_writes(&self) {
            rusqlite::Connection::open(self.store.path()).unwrap().execute_batch(
                "CREATE TRIGGER recovery_test_commit_failure BEFORE UPDATE ON recovery_domains BEGIN SELECT RAISE(FAIL, 'injected statement refusal'); END;"
            ).unwrap();
        }
        pub(crate) fn clear_commit_failure(&self) {
            rusqlite::Connection::open(self.store.path()).unwrap().execute_batch(
                "DROP TRIGGER IF EXISTS recovery_test_commit_failure; DROP TABLE IF EXISTS recovery_test_deferred_failure;"
            ).unwrap();
        }
        pub(crate) fn public_owner(
            &self,
            owner: &tidepool_runtime::session::RecoveryPublicOwner,
        ) -> Option<tidepool_runtime::session::RecoveryPublicOwnerState> {
            RunRecoveryStore::read_snapshot_at(
                self.store.path(),
                RecoveryDomainId::Root,
                Some(owner),
            )
            .unwrap()
            .unwrap()
            .public_owner
        }
    }
}

#[cfg(test)]
mod tests {
    use super::recovery_test_fixture::{LockedRun, RecoveryFixture};
    use super::*;
    use crate::{ActorExitKind, ActorId, ActorPlacement, Incarnation};
    use tidepool_codegen::scope::ScopeId;
    use tidepool_codegen::suspension::RealmId;
    use tidepool_repr::SessionId;

    struct LifetimeProbe {
        directory: Option<tempfile::TempDir>,
        closed: Option<tokio::sync::oneshot::Sender<std::io::Result<Vec<u8>>>>,
    }
    impl Drop for LifetimeProbe {
        fn drop(&mut self) {
            let observed =
                std::fs::read(self.directory.as_ref().unwrap().path().join("actors.jsonl"));
            drop(self.directory.take());
            if let Some(reply) = self.closed.take() {
                let _ = reply.send(observed);
            }
        }
    }

    #[test]
    fn recovery_refuses_older_versions_and_missing_model_authority() {
        let admission = DurableActorAdmission::capture(
            ActorRef::first(ActorId(1)),
            &descriptor("authority"),
            &[],
        );
        let row = Row {
            version: VERSION,
            sequence: 1,
            event: EventKind::Admitted {
                admission: Box::new(admission),
                startup: None,
            },
        };
        let current = serde_json::to_value(row).unwrap();
        assert!(parse_row(&current.to_string()).is_ok());
        for version in [10, 11] {
            let mut older = current.clone();
            older["version"] = version.into();
            assert!(parse_row(&older.to_string()).is_err());
        }
        let mut missing = current;
        missing["admission"]
            .as_object_mut()
            .unwrap()
            .remove("model_permissions");
        assert!(parse_row(&missing.to_string()).is_err());
    }

    #[tokio::test]
    async fn created_rows_refuse_surplus_fields_without_rewriting_recovery_bytes() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let path = directory.path().join("actors.jsonl");
        let valid = format!("{{\"version\":{VERSION},\"sequence\":1,\"event\":\"created\"}}");
        let row = parse_row(&valid).unwrap();
        assert_eq!(serde_json::to_string(&row).unwrap(), valid);
        let valid_bytes = format!("{valid}\n");
        std::fs::write(&path, &valid_bytes).unwrap();
        let journal =
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .unwrap();
        assert!(journal.records().await.unwrap().is_empty());
        drop(journal);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), valid_bytes);
        let valid: serde_json::Value = serde_json::from_str(&valid).unwrap();
        for field in ["foreign", "actor", "admission", "terminal", "", "λ"] {
            for value in [
                serde_json::Value::Null,
                serde_json::json!(true),
                serde_json::json!(1),
                serde_json::json!("extra"),
                serde_json::json!([]),
                serde_json::json!({}),
            ] {
                let mut invalid = valid.clone();
                invalid[field] = value;
                let bytes = format!("{invalid}\n");
                std::fs::write(&path, &bytes).unwrap();
                assert!(
                    ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                        .await
                        .is_err(),
                    "surplus field {field:?} accepted"
                );
                assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
            }
        }
        let mut second = valid.clone();
        second["sequence"] = serde_json::json!(2);
        for bytes in [format!("{second}\n"), format!("{valid}\n{second}\n")] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(ActorRecoveryJournal::open_existing(
                &anchor,
                "actors.jsonl",
                directory.clone()
            )
            .await
            .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
    }

    #[tokio::test]
    async fn terminal_without_diagnostic_remains_readable() {
        let line = serde_json::to_string(&Row {
            version: VERSION,
            sequence: 3,
            event: EventKind::Retired {
                actor: ActorRef {
                    id: ActorId(7),
                    incarnation: Incarnation(3),
                },
                terminal: crate::ActorTerminal::new(ActorExitKind::Failed, "original failure"),
            },
        })
        .unwrap();
        let row = parse_row(&line).expect("current terminal row");
        let EventKind::Retired { actor, terminal } = row.event else {
            panic!("retired row must stay retired");
        };
        assert_eq!(
            actor,
            ActorRef {
                id: ActorId(7),
                incarnation: Incarnation(3)
            }
        );
        assert_eq!(
            terminal,
            crate::ActorTerminal::new(ActorExitKind::Failed, "original failure")
        );
    }

    #[tokio::test]
    async fn cold_replay_preserves_optional_authored_labels_and_derives_display() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        for (index, label) in [None, Some(""), Some("duplicate"), Some("duplicate")]
            .into_iter()
            .enumerate()
        {
            let path = tidepool_repr::ActorPath::parse(&format!("spawn-{index}")).unwrap();
            let actor = ActorRef::first(ActorId(index as u64 + 1));
            let descriptor = ActorDescriptor::new_optional(
                label.map(str::to_owned),
                descriptor("placement").placement(),
            )
            .with_actor_path(tidepool_repr::ActorPath::parse("first-assignment").unwrap())
            .with_actor_path(path);
            journal.admit(actor, &descriptor, &[]).await.unwrap();
        }
        drop(journal);
        let records =
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .unwrap()
                .records()
                .await
                .unwrap();
        assert_eq!(records[0].admission.label, None);
        assert_eq!(records[0].admission.display_label(), "spawn-0");
        assert_eq!(records[1].admission.label.as_deref(), Some(""));
        assert_eq!(records[1].admission.display_label(), "");
        assert_eq!(records[2].admission.label.as_deref(), Some("duplicate"));
        assert_eq!(records[3].admission.label, records[2].admission.label);
        assert_ne!(
            records[2].admission.actor_path,
            records[3].admission.actor_path
        );
    }

    #[tokio::test]
    async fn embedded_application_intent_and_binding_survive_cold_reopen() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let actor = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(3),
        };
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        journal
            .admit(actor, &descriptor("worker"), &[])
            .await
            .unwrap();
        let expected = ApplicationConversation::Embedded {
            run: "run".into(),
            agent_path: "/root/a7_i3".into(),
            incarnation: "3".into(),
        };
        journal
            .prepare_application_with_intent(
                actor,
                directory.path().join("binding.json"),
                Some("source".into()),
                Some(expected.clone()),
            )
            .await
            .unwrap();
        assert!(journal
            .bind_application(actor, "fake-codex-thread".into())
            .await
            .is_err());
        let changed = ApplicationConversation::Embedded {
            run: "run".into(),
            agent_path: "/root/a7_i3".into(),
            incarnation: "4".into(),
        };
        assert!(journal
            .bind_application_conversation(actor, changed)
            .await
            .is_err());
        drop(journal);
        let journal =
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .unwrap();
        assert!(journal.records().await.unwrap()[0]
            .application
            .as_ref()
            .unwrap()
            .conversation
            .is_none());
        journal
            .bind_application_conversation(actor, expected.clone())
            .await
            .unwrap();
        journal
            .bind_application_conversation(actor, expected.clone())
            .await
            .unwrap();
        drop(journal);
        let journal =
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .unwrap();
        let record = journal.records().await.unwrap().remove(0);
        let application = record.application.unwrap();
        assert_eq!(application.intended_conversation, Some(expected.clone()));
        assert_eq!(application.conversation, Some(expected));
    }

    #[tokio::test]
    async fn old_journal_versions_are_unsupported_and_remain_untouched() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        for version in [1, 2, 3, 4, 5, 6, 7, 8, 9] {
            let path = directory.path().join(format!("v{version}.jsonl"));
            let bytes = format!("{{\"version\":{version},\"sequence\":1,\"event\":\"created\"}}\n");
            std::fs::write(&path, &bytes).unwrap();
            assert!(ActorRecoveryJournal::open_existing(
                &anchor,
                format!("v{version}.jsonl"),
                directory.clone()
            )
            .await
            .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        }

        let legacy = r#"{"version":5,"sequence":1,"event":"admitted","admission":{"actor":{"id":1,"incarnation":1},"label":"root","creator":null,"supervisor_parent":null,"context_parent":null,"actor_path":"root","role":"root","descendant_depth":8,"descendant_active_children":null,"model":null,"effort":null,"instructions":null,"launch_worktrees":[],"source_layer":[]}}"#;
        let legacy_path = directory.path().join("legacy-role.jsonl");
        std::fs::write(&legacy_path, format!("{legacy}\n")).unwrap();
        assert!(ActorRecoveryJournal::open_existing(
            &anchor,
            "legacy-role.jsonl",
            directory.clone()
        )
        .await
        .is_err());
        assert_eq!(
            std::fs::read_to_string(&legacy_path).unwrap(),
            format!("{legacy}\n")
        );
    }

    #[tokio::test]
    async fn cold_replay_refuses_binding_after_terminal_publication() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let path = directory.path().join("actors.jsonl");
        let actor = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(3),
        };
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        journal
            .admit(actor, &descriptor("worker"), &[])
            .await
            .unwrap();
        journal
            .prepare_application(actor, directory.path().join("binding.json"), None)
            .await
            .unwrap();
        journal
            .retire(
                actor,
                crate::ActorTerminal::new(ActorExitKind::Completed, "done"),
            )
            .await
            .unwrap();
        assert!(journal
            .bind_application(actor, "thread".into())
            .await
            .is_err());
        let row = Row {
            version: VERSION,
            sequence: 5,
            event: EventKind::ApplicationBound {
                actor,
                conversation: ApplicationConversation::Codex {
                    thread_id: "thread".into(),
                },
            },
        };
        tidepool_repr::jsonl::append_new_line(
            &path,
            &serde_json::to_string(&row).unwrap(),
            SyncPolicy::All,
        )
        .unwrap();
        drop(journal);
        assert!(
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .is_err()
        );
    }

    /// Runs the real kernel/coordinator registration protocol without issuing
    /// cleanup or native binding authority. The proof-only fixture refuses its claim.
    struct JournalCoordinatorKernel;
    impl crate::KernelBehavior for JournalCoordinatorKernel {
        fn start<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<()>, crate::KernelBehaviorError>,
        > {
            Box::pin(async { Ok(crate::KernelStep::Continue(())) })
        }
        fn cast<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _sender: ActorRef,
            _request: crate::MailboxValue,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<()>, crate::KernelBehaviorError>,
        > {
            Box::pin(async { panic!("journal coordinator fixture has no casts") })
        }
        fn call<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _caller: ActorRef,
            _ancestry: crate::CallAncestry,
            _request: crate::MailboxValue,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<crate::MailboxValue>, crate::KernelBehaviorError>,
        > {
            Box::pin(async { panic!("journal coordinator fixture has no calls") })
        }
        fn tool<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _invocation: exomonad_tool::ToolInvocation,
            _capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<serde_json::Value>, crate::KernelInvocationFailure>,
        > {
            Box::pin(async { panic!("journal coordinator fixture has no tools") })
        }
        fn dispatch_workbench(
            &mut self,
            _context: &crate::KernelContext,
            _invocation: crate::ActorWorkbenchInvocation,
            _control: Option<Arc<crate::WorkbenchExecutionControl>>,
        ) -> crate::WorkbenchDispatch<Self> {
            panic!("journal coordinator fixture has no workbench")
        }
        fn external_application_failed<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _failure: crate::ExternalApplicationFailure,
        ) -> futures_util::future::BoxFuture<'a, crate::ExternalFailureDisposition> {
            Box::pin(async { panic!("journal coordinator fixture has no external application") })
        }
        fn shutdown<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _terminal: &'a crate::ActorTerminal,
        ) -> futures_util::future::BoxFuture<'a, Result<(), crate::KernelBehaviorError>> {
            Box::pin(async { Ok(()) })
        }
        fn stopped<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _terminal: &'a crate::ActorTerminal,
        ) -> futures_util::future::BoxFuture<'a, ()> {
            Box::pin(async {})
        }
        fn child_exited(&mut self, _notice: crate::ChildExitNotice) {
            panic!("journal coordinator fixture has no children")
        }
    }

    fn descriptor(label: &str) -> ActorDescriptor {
        ActorDescriptor::new(
            label,
            ActorPlacement {
                session: SessionId(1),
                lexical_scope: ScopeId(1),
                resource_scope: RealmId(1),
            },
        )
    }

    #[tokio::test]
    async fn lifecycle_reopens_with_active_and_terminal_records() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let path = directory.path().join("actors.jsonl");
        let actor = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(3),
        };
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        journal
            .admit(actor, &descriptor("worker"), &["tree-1".into()])
            .await
            .unwrap();
        drop(journal);
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.is_empty(), "journal append produced no bytes");
        for row in stored.lines() {
            parse_row(row).unwrap();
        }
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let records = journal.records().await.unwrap();
        assert_eq!(records.len(), 1, "stored rows: {stored}");
        assert_eq!(records[0].admission.actor, actor);
        assert_eq!(records[0].admission.launch_worktrees, ["tree-1"]);
        assert!(records[0].application.is_none());
        assert!(records[0].terminal.is_none());
        journal
            .prepare_application(
                actor,
                directory.path().join("binding.json"),
                Some("source-a".into()),
            )
            .await
            .unwrap();
        journal
            .bind_application(actor, "conversation-7".into())
            .await
            .unwrap();
        journal
            .retire(
                actor,
                crate::ActorTerminal::new(ActorExitKind::Completed, "done"),
            )
            .await
            .unwrap();
        drop(journal);
        let records = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap()
            .records()
            .await
            .unwrap();
        assert_eq!(
            records[0]
                .application
                .as_ref()
                .unwrap()
                .conversation
                .as_ref()
                .and_then(ApplicationConversation::codex_thread),
            Some("conversation-7")
        );
        assert_eq!(records[0].terminal.as_ref().unwrap().summary, "done");
    }

    #[tokio::test]
    async fn observed_read_leaves_a_live_journal_and_its_torn_tail_untouched() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let path = directory.path().join("actors.jsonl");
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        journal
            .admit(ActorRef::first(ActorId(4)), &descriptor("observed"), &[])
            .await
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"version\":2,\"sequence\":3,");
        std::fs::write(&path, &bytes).unwrap();
        let records = ActorRecoveryJournal::read_observed(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].admission.label.as_deref(), Some("observed"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let missing = directory.path().join("missing.jsonl");
        assert!(ActorRecoveryJournal::read_observed(&missing)
            .unwrap()
            .is_empty());
        assert!(!missing.exists());
    }

    #[tokio::test]
    async fn identity_reuse_with_changed_parameters_is_rejected() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let actor = ActorRef::first(ActorId(2));
        journal
            .admit(actor, &descriptor("first"), &[])
            .await
            .unwrap();
        let error = journal
            .admit(actor, &descriptor("changed"), &[])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("reused"));
    }

    #[tokio::test]
    async fn every_application_publication_boundary_reopens_without_inventing_progress() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let binding = directory.path().join("binding.json");
        let actor = ActorRef::first(ActorId(9));

        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        journal
            .admit(actor, &descriptor("worker"), &[])
            .await
            .unwrap();
        drop(journal);
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        assert!(journal.records().await.unwrap()[0].application.is_none());

        journal
            .prepare_application(actor, binding.clone(), Some("source-revision".into()))
            .await
            .unwrap();
        drop(journal);
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let prepared = journal
            .records()
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .application
            .unwrap();
        assert_eq!(prepared.binding_path, binding);
        assert_eq!(prepared.accepted_source.as_deref(), Some("source-revision"));
        assert!(prepared.conversation.is_none());

        journal
            .bind_application(actor, "conversation-9".into())
            .await
            .unwrap();
        drop(journal);
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        assert_eq!(
            journal.records().await.unwrap()[0]
                .application
                .as_ref()
                .unwrap()
                .conversation
                .as_ref()
                .and_then(ApplicationConversation::codex_thread),
            Some("conversation-9")
        );
    }

    #[tokio::test]
    async fn recovered_host_requires_the_original_lifecycle_owner() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        assert_eq!(
            ActorRecoveryJournal::open_existing(&anchor, "missing.jsonl", directory.clone())
                .await
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotFound
        );

        let empty = directory.path().join("empty.jsonl");
        std::fs::write(&empty, b"").unwrap();
        assert!(
            ActorRecoveryJournal::open_existing(&anchor, "empty.jsonl", directory.clone())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("creation marker")
        );

        drop(
            ActorRecoveryJournal::open(&anchor, "initialized.jsonl", directory.clone())
                .await
                .unwrap(),
        );
        ActorRecoveryJournal::open_existing(&anchor, "initialized.jsonl", directory.clone())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn anchored_open_refuses_escape_before_mutation_and_retries_failed_parent_creation() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();

        assert!(
            ActorRecoveryJournal::open(&anchor, "../outside/actors.jsonl", directory.clone())
                .await
                .is_err()
        );
        assert!(!directory.path().parent().unwrap().join("outside").exists());
        assert!(ActorRecoveryJournal::open(
            &anchor,
            directory.path().join("absolute.jsonl"),
            directory.clone()
        )
        .await
        .is_err());
        assert!(!directory.path().join("absolute.jsonl").exists());

        let obstruction = directory.path().join("nested");
        std::fs::write(&obstruction, b"file blocks directory creation").unwrap();
        assert!(
            ActorRecoveryJournal::open(&anchor, "nested/actors.jsonl", directory.clone())
                .await
                .is_err()
        );
        std::fs::remove_file(obstruction).unwrap();

        let journal = ActorRecoveryJournal::open(&anchor, "nested/actors.jsonl", directory.clone())
            .await
            .unwrap();
        assert!(directory.path().join("nested/actors.jsonl").is_file());
        assert_eq!(journal.records().await.unwrap().len(), 0);
    }

    fn recovery_fixture(
        path: &Path,
        owner: &tidepool_runtime::session::RecoveryPublicOwner,
        ceiling: u64,
    ) -> (
        Arc<tidepool_runtime::session::RunRecoveryStore>,
        Arc<LockedRun>,
    ) {
        use tidepool_runtime::session::{
            ModuleEnv, PersistentSession, RecoveryDomainId, SessionLib,
        };
        let root = path.parent().unwrap().canonicalize().unwrap();
        let fixture = RecoveryFixture::create(&root);
        assert_eq!(fixture.store.path(), path);
        let authority = Arc::clone(&fixture.authority);
        let store = Arc::clone(&fixture.store);
        let artifact_root = root.join("haskell-session");
        let mut library = SessionLib::open(
            SessionId(1),
            &artifact_root,
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| {
            library.attach_owned_recovery_domain(
                Arc::clone(&store),
                RecoveryDomainId::Root,
                artifact_root,
                authority.clone(),
                settlement,
            )
        })
        .unwrap();
        library
            .initialize_captured_declaration_high_water(tidepool_repr::Generation(ceiling))
            .unwrap();
        let mut session = PersistentSession::new(Some(library), 1024);
        let public = session.mint_isolated_scope();
        session
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap();
        (store, authority)
    }

    fn startup_intent(
        actor: ActorRef,
        binding: &Path,
        predecessor: Option<ActorRef>,
        manifest_predecessor: Option<ActorRef>,
        manifest: Option<RootStartupManifestPin>,
        store_predecessor: Option<ApplicationConversation>,
    ) -> RootStartupIntent {
        RootStartupIntent {
            bootstrap_identity: "compiled-bootstrap".into(),
            predecessor,
            manifest_predecessor,
            manifest,
            store_predecessor,
            binding_path: binding.to_owned(),
            accepted_source: Some("source".into()),
            conversation: ApplicationConversation::Embedded {
                run: "run".into(),
                agent_path: "/root".into(),
                incarnation: actor.incarnation.0.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn root_startup_admission_and_exact_intent_reopen_atomically() {
        let run = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(run.path()).unwrap();
        let path = run.path().join("actors.jsonl");
        let (control, _control_task) = crate::local_actor::run_control::RunControlClient::start()
            .await
            .unwrap();
        let reservation = control.reserve(Incarnation::FIRST).await.unwrap();
        let actor = reservation.identity();
        let root =
            descriptor("root").with_actor_path(tidepool_repr::ActorPath::parse("root").unwrap());
        let intent = startup_intent(
            actor,
            &run.path().join("binding.json"),
            None,
            None,
            None,
            None,
        );
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", run.clone())
            .await
            .unwrap();
        let permit = control
            .authorize_root_startup(&reservation, intent.clone(), journal.clone(), None)
            .await
            .unwrap();
        journal
            .admit_with_startup(actor, &root, &[], Some(intent.clone()), Some(&permit))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        drop(permit);
        let mut stopped = journal.owner_status();
        drop(journal);
        while !matches!(*stopped.borrow(), JournalOwnerStatus::Stopped { .. }) {
            stopped.changed().await.unwrap();
        }
        let journal = ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", run.clone())
            .await
            .unwrap();
        let record = &journal.records().await.unwrap()[0];
        assert_eq!(record.startup.as_ref(), Some(&intent));
        assert_eq!(record.application.as_ref(), Some(&intent.application()));
        assert!(record.application.as_ref().unwrap().conversation.is_none());
        journal
            .bind_application_conversation(actor, intent.conversation.clone())
            .await
            .unwrap();
        drop(journal);
        assert_eq!(
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", run.clone())
                .await
                .unwrap()
                .records()
                .await
                .unwrap()[0]
                .application
                .as_ref()
                .unwrap()
                .conversation
                .as_ref(),
            Some(&intent.conversation)
        );
    }

    #[tokio::test]
    async fn cancelled_bootstrap_retains_actual_exclusive_run_lease_until_exit() {
        struct LeaseLifetime {
            run: Option<Arc<dyn tidepool_runtime::session::RecoveryRunAuthority>>,
            released: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl Drop for LeaseLifetime {
            fn drop(&mut self) {
                drop(self.run.take());
                let _ = self.released.take().unwrap().send(());
            }
        }
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let run = recovery_test_fixture::boot_registration(directory.path()).into_run();
        let (released, exit) = tokio::sync::oneshot::channel();
        let lifetime: Arc<dyn Send + Sync> = Arc::new(LeaseLifetime {
            run: Some(run.clone()),
            released: Some(released),
        });
        let (entered, paused) = tokio::sync::oneshot::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        let opening = tokio::spawn(ActorRecoveryJournal::start_bootstrap(
            anchor,
            "actors.jsonl".into(),
            false,
            lifetime,
            Some(run),
            Some(BootstrapPause {
                entered,
                resume: resumed,
            }),
        ));
        paused.await.unwrap();
        opening.abort();
        assert!(matches!(opening.await, Err(error) if error.is_cancelled()));
        let competing = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.path().join("recovery-owner.lock"))
            .unwrap();
        assert!(competing.try_lock().is_err());
        resume.send(()).unwrap();
        exit.await.unwrap();
        competing.try_lock().unwrap();
        let records =
            ActorRecoveryJournal::read_observed(&directory.path().join("actors.jsonl")).unwrap();
        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn boot_registration_freezes_initial_prefix_and_exact_writer() {
        let run = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(run.path()).unwrap();
        let previous = ActorRecoveryJournal::open(&anchor, "actors.jsonl", run.clone())
            .await
            .unwrap();
        let old = ActorRef::first(ActorId(1));
        let current = ActorRef::first(ActorId(2));
        previous.admit(old, &descriptor("old"), &[]).await.unwrap();
        let mut status = previous.owner_status();
        drop(previous);
        while !matches!(*status.borrow(), JournalOwnerStatus::Stopped { .. }) {
            status.changed().await.unwrap();
        }
        let (journal, boot) = ActorRecoveryJournal::open_for_boot(
            &anchor,
            "actors.jsonl",
            true,
            recovery_test_fixture::boot_registration(run.path()),
        )
        .await
        .unwrap();
        assert!(boot.qualifies(old, &journal));
        assert!(!boot.qualifies(current, &journal));
        journal
            .admit(current, &descriptor("current"), &[])
            .await
            .unwrap();
        assert!(!boot.qualifies(current, &journal));
        let other = ActorRecoveryJournal::open(&anchor, "other.jsonl", run.clone())
            .await
            .unwrap();
        assert!(!boot.qualifies(old, &other));
        assert!(boot.run.owns_run(run.path()).unwrap());
    }

    #[tokio::test]
    async fn root_successor_receipt_requires_latest_prepared_journal_and_owned_placement() {
        root_successor_receipt_history(false).await;
    }

    #[tokio::test]
    async fn root_successor_receipt_retains_selected_generation_from_canonical_admission() {
        root_successor_receipt_history(true).await;
    }

    async fn root_successor_receipt_history(publish_replacement: bool) {
        let run = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(run.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", run.clone())
            .await
            .unwrap();
        let binding = run.path().join("binding.json");
        let manifest = run.path().join("recovery.sqlite");
        let path = tidepool_repr::ActorPath::parse("root/recovered").unwrap();
        let root = descriptor("root").with_actor_path(path.clone());
        let root = root.with_persistence_policy(crate::ActorPersistencePolicy::Durable);
        let (previous_control, previous_task) =
            crate::local_actor::run_control::RunControlClient::start()
                .await
                .unwrap();
        let previous = previous_control.reserve(Incarnation::FIRST).await.unwrap();
        let old = previous.identity();
        let initial = startup_intent(old, &binding, None, None, None, None);
        let permit = previous_control
            .authorize_root_startup(&previous, initial.clone(), journal.clone(), None)
            .await
            .unwrap();
        journal
            .admit_with_startup(old, &root, &[], Some(initial.clone()), Some(&permit))
            .await
            .unwrap();
        journal
            .bind_application_conversation(old, initial.conversation.clone())
            .await
            .unwrap();
        let old_owner = tidepool_runtime::session::RecoveryPublicOwner::new(&path, 1).unwrap();
        let (store, authority) = recovery_fixture(&manifest, &old_owner, 7);
        let selected = if publish_replacement {
            use tidepool_runtime::session::{
                ComposedPromptSnapshot, DurableGenerationReferences, GenerationArtifactReference,
                ModuleEnv, PersistentSession, PublicationDecision, RecoveryDomainId, SessionLib,
            };
            let artifact_root = run.path().join("generation-session");
            let mut library = SessionLib::open(
                SessionId(2),
                &artifact_root,
                ModuleEnv::standalone_default(),
            )
            .unwrap();
            tidepool_testing::with_settlement(|settlement| {
                library.attach_owned_recovery_domain(
                    store.clone(),
                    RecoveryDomainId::Root,
                    artifact_root,
                    authority.clone(),
                    settlement,
                )
            })
            .unwrap();
            let mut session = PersistentSession::new(Some(library), 1024);
            let original = session.recover_public_scope(&old_owner).unwrap();
            let references = |marker| DurableGenerationReferences {
                source: GenerationArtifactReference::capture(vec![marker, 1]),
                installation: GenerationArtifactReference::capture(vec![marker, 2]),
                prompts: ComposedPromptSnapshot::capture(format!("prompt {marker}")),
            };
            let a = session
                .prepare_notebook_generation(original, &old_owner, None, references(1), None)
                .unwrap();
            session
                .commit_notebook_generation(a, &PublicationDecision::new(), |_| {})
                .unwrap();
            let pin_a = RootStartupManifestPin::capture_for_actor(&manifest, &old_owner).unwrap();
            let head_a = session
                .active_notebook_generation(&old_owner)
                .unwrap()
                .unwrap();
            let replacement = session.mint_isolated_scope();
            let b = session
                .prepare_notebook_generation(
                    replacement,
                    &old_owner,
                    Some(head_a),
                    references(2),
                    None,
                )
                .unwrap();
            session
                .commit_notebook_generation(b, &PublicationDecision::new(), |_| {})
                .unwrap();
            assert!(
                !pin_a.validate(run.path()).unwrap(),
                "a genuine B publication makes the previously issued A pin stale"
            );
            Some(
                session
                    .active_notebook_generation(&old_owner)
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        let pinned = RootStartupManifestPin::capture_for_actor(&manifest, &old_owner).unwrap();
        let selected_owner = pinned.public_owner.owner.clone();
        assert_eq!(selected_owner.actor_owner(), old_owner);
        if publish_replacement {
            assert_eq!(selected_owner.notebook_generation().get(), 2);
            assert_ne!(selected_owner, old_owner);
        }
        drop(permit);
        drop(previous);
        drop(previous_control);
        previous_task.abort();
        let _ = previous_task.await;
        let mut stopped = journal.owner_status();
        drop(journal);
        while !matches!(*stopped.borrow(), JournalOwnerStatus::Stopped { .. }) {
            stopped.changed().await.unwrap();
        }
        let lease = Arc::try_unwrap(authority)
            .ok()
            .expect("previous native session released run lease");
        let (journal, boot) =
            ActorRecoveryJournal::open_for_boot(&anchor, "actors.jsonl", true, Box::new(lease))
                .await
                .unwrap();
        let run_authority = boot.run.clone();
        let (control, _task) = crate::local_actor::run_control::RunControlClient::start()
            .await
            .unwrap();
        control
            .register_recovery_boot(journal.clone(), boot)
            .await
            .unwrap();
        let reservation = control.reserve(Incarnation(14)).await.unwrap();
        let next = reservation.identity();
        let placement = RootRecoveryPlacement::new(
            next,
            tidepool_runtime::session::RecoveryPublicOwner::new(&path, next.incarnation.0).unwrap(),
            root.placement(),
        );
        let successor = startup_intent(
            next,
            &binding,
            Some(old),
            Some(old),
            Some(pinned),
            Some(initial.conversation.clone()),
        );
        let permit = control
            .authorize_root_startup(&reservation, successor.clone(), journal.clone(), None)
            .await
            .unwrap();
        journal
            .admit_with_startup(next, &root, &[], Some(successor.clone()), Some(&permit))
            .await
            .unwrap();
        use tidepool_runtime::session::{ModuleEnv, RecoveryDomainId, ResidentSession, SessionLib};
        let artifact_root = run.path().join("recovered-session");
        let mut library = SessionLib::open(
            SessionId(1),
            &artifact_root,
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| {
            library.attach_owned_recovery_domain(
                store.clone(),
                RecoveryDomainId::Root,
                artifact_root,
                run_authority,
                settlement,
            )
        })
        .unwrap();
        let mut machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let target = machine.mint_isolated_scope();
        assert_eq!(target, root.placement().lexical_scope);
        machine.seal_recovery_initialization_scope(target).unwrap();
        let machines = Arc::new(crate::ActorMachineRegistry::new());
        assert!(machines
            .insert_idle(SessionId(1), Box::new(machine))
            .is_none());
        let runner = Arc::new(crate::ResidentActorRunner::new(
            machines,
            crate::ActorWorkbenchSource::new("", vec![])
                .bind_preparation(control.preparation_client()),
        ));
        let lifecycle = reservation.lifecycle().clone();
        lifecycle
            .configure_resident(
                root.session_context(next),
                root.clone(),
                Some(journal.clone()),
                runner,
                Some(successor),
                None,
            )
            .await
            .unwrap();
        let (_kernel, kernel_task) = crate::local_actor::spawn_reserved_local_actor(
            None,
            JournalCoordinatorKernel,
            reservation,
            control.clone(),
        )
        .await
        .unwrap();
        if publish_replacement {
            assert!(
                lifecycle
                    .begin_successor_operation(old_owner.clone())
                    .await
                    .is_err(),
                "canonical A cannot substitute for pinned B"
            );
        }
        let claim = lifecycle
            .begin_successor_operation(selected_owner.clone())
            .await
            .unwrap();
        let proof = journal
            .certify_root_successor(old, placement.clone(), Some("source"), &binding, &claim)
            .await
            .unwrap();
        assert!(proof
            .validate_successor(
                run.path(),
                &selected_owner,
                placement.owner(),
                SessionId(1),
                ScopeId(1)
            )
            .unwrap());
        assert!(proof
            .validate_embedded_binding(
                run.path(),
                &initial.conversation,
                &journal
                    .records()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|record| record.admission.actor == next)
                    .unwrap()
                    .startup
                    .unwrap()
                    .conversation
            )
            .unwrap());
        assert!(!proof
            .validate_successor(
                run.path(),
                &selected_owner,
                placement.owner(),
                SessionId(9),
                ScopeId(1)
            )
            .unwrap());
        assert!(!proof
            .validate_successor(
                tempfile::tempdir().unwrap().path(),
                &selected_owner,
                placement.owner(),
                SessionId(1),
                ScopeId(1)
            )
            .unwrap());
        assert!(journal
            .certify_root_successor(
                old,
                placement.clone(),
                Some("different-source"),
                &binding,
                &claim
            )
            .await
            .is_err());
        if let Some(selected) = selected {
            assert!(!proof
                .validate_successor(
                    run.path(),
                    &old_owner,
                    placement.owner(),
                    SessionId(1),
                    ScopeId(1)
                )
                .unwrap());
            struct CertifiedSuccessor(Arc<DurableRootSuccessorAdmission>);
            impl tidepool_runtime::session::RecoverySuccessorAuthority for CertifiedSuccessor {
                fn validate_successor(
                    &self,
                    run_root: &Path,
                    predecessor: &tidepool_runtime::session::RecoveryPublicOwner,
                    successor: &tidepool_runtime::session::RecoveryPublicOwner,
                    session: SessionId,
                    target: ScopeId,
                ) -> std::io::Result<bool> {
                    self.0
                        .validate_successor(run_root, predecessor, successor, session, target)
                }
            }
            assert!(matches!(
                lifecycle
                    .publish_public_owner(Some((
                        selected_owner.clone(),
                        Arc::new(CertifiedSuccessor(proof.clone()))
                    )))
                    .await
                    .unwrap(),
                tidepool_runtime::session::PublicManifestCommit::Durable
            ));
            let transferred = store
                .active_notebook_generation(
                    tidepool_runtime::session::RecoveryDomainId::Root,
                    placement.owner(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(transferred.references, selected.references);
            assert_eq!(transferred.owner, *placement.owner());
            assert!(store
                .active_notebook_generation(
                    tidepool_runtime::session::RecoveryDomainId::Root,
                    &old_owner
                )
                .unwrap()
                .is_none());
            assert!(proof.validate_binding_claim(run.path(), &claim).unwrap());
            struct JournalBinding {
                journal: Arc<ActorRecoveryJournal>,
                conversation: ApplicationConversation,
            }
            impl crate::RootBindingAdapter for JournalBinding {
                fn bind<'a>(
                    &'a self,
                    claim: &'a crate::RootBindingOperationClaim,
                ) -> futures_util::future::BoxFuture<'a, Result<JournalApplicationBindingAck, String>>
                {
                    Box::pin(async move {
                        self.journal
                            .bind_application_conversation(
                                claim.successor_actor(),
                                self.conversation.clone(),
                            )
                            .await
                            .map_err(|error| error.to_string())
                    })
                }
            }
            let native = lifecycle
                .resident_snapshot()
                .unwrap()
                .public_owner
                .ready()
                .unwrap()
                .native_readiness()
                .unwrap();
            claim
                .finish_binding(
                    native,
                    Arc::new(JournalBinding {
                        journal: journal.clone(),
                        conversation: ApplicationConversation::Embedded {
                            run: "run".into(),
                            agent_path: "/root".into(),
                            incarnation: next.incarnation.0.to_string(),
                        },
                    }),
                )
                .await
                .unwrap();
            kernel_task.abort();
            let _ = kernel_task.await;
            return;
        }
        let revision = store
            .read_domain(tidepool_runtime::session::RecoveryDomainId::Root)
            .unwrap()
            .unwrap()
            .revision;
        assert!(matches!(
            store.reserve_ceiling(
                tidepool_runtime::session::RecoveryDomainId::Root,
                revision,
                tidepool_repr::Generation(8),
            ),
            tidepool_runtime::session::recovery_store::StoreDisposition::Committed(_)
        ));
        assert!(!proof
            .validate_successor(
                run.path(),
                &old_owner,
                placement.owner(),
                SessionId(1),
                ScopeId(1)
            )
            .unwrap());
        assert!(
            proof.validate_binding_claim(run.path(), &claim).unwrap(),
            "binding uses issued identity after the manifest advances"
        );
        assert!(
            !proof
                .validate_binding_claim(tempfile::tempdir().unwrap().path(), &claim)
                .unwrap(),
            "binding still rejects another run"
        );
        claim.refuse().await.unwrap();
        kernel_task.abort();
        let _ = kernel_task.await;
    }

    #[tokio::test]
    async fn root_startup_rollforward_is_linear_and_cannot_cross_a_bound_owner() {
        for interrupted in [false, true] {
            root_startup_context_history(interrupted).await;
        }
    }

    async fn root_startup_context_history(interrupted: bool) {
        use crate::initial_context::{
            InitialContextEvent, InitialContextIdentity, InitialContextObservation,
            InitialContextPublication, InitialContextState, RetainedContextSeed,
        };
        let run = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(run.path()).unwrap();
        let binding = run.path().join("binding.json");
        let manifest = run.path().join("recovery.sqlite");
        let root =
            descriptor("root").with_actor_path(tidepool_repr::ActorPath::parse("root").unwrap());
        let old_owner = tidepool_runtime::session::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root").unwrap(),
            1,
        )
        .unwrap();
        let (_store, authority) = recovery_fixture(&manifest, &old_owner, 0);
        let pin = RootStartupManifestPin::capture_for_actor(&manifest, &old_owner).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", run.clone())
            .await
            .unwrap();
        let (control, task) = crate::local_actor::run_control::RunControlClient::start()
            .await
            .unwrap();
        let reservation = control.reserve(Incarnation::FIRST).await.unwrap();
        let a = reservation.identity();
        let initial = startup_intent(a, &binding, None, None, None, None);
        let permit = control
            .authorize_root_startup(&reservation, initial.clone(), journal.clone(), None)
            .await
            .unwrap();
        journal
            .admit_with_startup(a, &root, &[], Some(initial.clone()), Some(&permit))
            .await
            .unwrap();
        journal
            .bind_application_conversation(a, initial.conversation.clone())
            .await
            .unwrap();
        let identity = InitialContextIdentity {
            operation: tidepool_runtime::session::WorkbenchExecutionId::from_digest([47; 16]),
            actor: a,
            spec_install: 1,
            spec_revision: Some("source-revision".into()),
            spec_entry: Some("AgentSpec.agentSpec".into()),
            spec_source: "startup-source".into(),
        };
        journal
            .record_initial_context(
                a,
                InitialContextEvent::Begin {
                    identity: identity.clone(),
                    caller_seed: None,
                },
            )
            .await
            .unwrap();
        let result = RetainedContextSeed {
            sections: vec![],
            rendered: "retained startup".into(),
        };
        let receipts = serde_json::json!({"observed": "startup effect"});
        let publication = InitialContextPublication {
            conversation: initial.conversation.clone(),
            request: "startup-publication".into(),
            caller_reference: None,
            authored_reference: Some("authored-startup".into()),
        };
        if !interrupted {
            journal
                .record_initial_context(
                    a,
                    InitialContextEvent::Retain {
                        result: result.clone(),
                        receipts: receipts.clone(),
                    },
                )
                .await
                .unwrap();
            journal
                .record_initial_context(
                    a,
                    InitialContextEvent::Publish {
                        publication: publication.clone(),
                    },
                )
                .await
                .unwrap();
        }
        let inherited = InitialContextObservation {
            identity: Some(identity),
            caller_seed: None,
            state: if interrupted {
                InitialContextState::RecoveryRequired {
                    detail: "host loss interrupted initialization; retained effects must not be replayed".into(),
                }
            } else {
                InitialContextState::Published
            },
            result: (!interrupted).then_some(result),
            publication: (!interrupted).then_some(publication),
            receipts: if interrupted {
                serde_json::Value::Null
            } else {
                receipts
            },
        };
        drop(permit);
        drop(reservation);
        drop(control);
        task.abort();
        let _ = task.await;
        let mut stopped = journal.owner_status();
        drop(journal);
        while !matches!(*stopped.borrow(), JournalOwnerStatus::Stopped { .. }) {
            stopped.changed().await.unwrap();
        }
        let lease = Arc::try_unwrap(authority)
            .ok()
            .expect("previous native session released run lease");
        let (journal, boot) =
            ActorRecoveryJournal::open_for_boot(&anchor, "actors.jsonl", true, Box::new(lease))
                .await
                .unwrap();
        let (control, task) = crate::local_actor::run_control::RunControlClient::start()
            .await
            .unwrap();
        control
            .register_recovery_boot(journal.clone(), boot)
            .await
            .unwrap();
        let reservation = control.reserve(Incarnation(4)).await.unwrap();
        let b = reservation.identity();
        let second = startup_intent(
            b,
            &binding,
            Some(a),
            Some(a),
            Some(pin.clone()),
            Some(initial.conversation.clone()),
        );
        let permit = control
            .authorize_root_startup(&reservation, second.clone(), journal.clone(), None)
            .await
            .unwrap();
        journal
            .admit_with_startup(b, &root, &[], Some(second), Some(&permit))
            .await
            .unwrap();
        assert_eq!(
            journal.initial_context(b).await.unwrap(),
            Some(inherited.clone())
        );
        assert!(journal
            .records()
            .await
            .unwrap()
            .iter()
            .find(|record| record.admission.actor == b)
            .unwrap()
            .initial_context
            .is_none());
        drop(permit);
        drop(reservation);
        drop(control);
        task.abort();
        let _ = task.await;
        let mut stopped = journal.owner_status();
        drop(journal);
        while !matches!(*stopped.borrow(), JournalOwnerStatus::Stopped { .. }) {
            stopped.changed().await.unwrap();
        }
        let (journal, boot) = ActorRecoveryJournal::open_for_boot(
            &anchor,
            "actors.jsonl",
            true,
            recovery_test_fixture::boot_registration(run.path()),
        )
        .await
        .unwrap();
        let (control, _task) = crate::local_actor::run_control::RunControlClient::start()
            .await
            .unwrap();
        control
            .register_recovery_boot(journal.clone(), boot)
            .await
            .unwrap();
        let reservation = control.reserve(Incarnation(12)).await.unwrap();
        let c = reservation.identity();
        let third = startup_intent(
            c,
            &binding,
            Some(b),
            Some(a),
            Some(pin.clone()),
            Some(initial.conversation.clone()),
        );
        let permit = control
            .authorize_root_startup(&reservation, third.clone(), journal.clone(), None)
            .await
            .unwrap();
        let mut changed_bootstrap = third.clone();
        changed_bootstrap.bootstrap_identity = "different-bootstrap".into();
        assert!(journal
            .admit_with_startup(c, &root, &[], Some(changed_bootstrap), Some(&permit))
            .await
            .is_err());
        let mut changed_content = third.clone();
        changed_content
            .manifest
            .as_mut()
            .unwrap()
            .domain
            .content_revision = tidepool_runtime::session::StoreRevision(
            pin.domain.content_revision.0.checked_add(1).unwrap(),
        );
        assert!(journal
            .admit_with_startup(c, &root, &[], Some(changed_content), Some(&permit))
            .await
            .is_err());
        journal
            .admit_with_startup(c, &root, &[], Some(third.clone()), Some(&permit))
            .await
            .unwrap();
        let next = control.reserve(Incarnation(28)).await.unwrap();
        let d = next.identity();
        let branch = startup_intent(
            d,
            &binding,
            Some(b),
            Some(a),
            Some(pin.clone()),
            Some(initial.conversation.clone()),
        );
        assert!(control
            .authorize_root_startup(&next, branch.clone(), journal.clone(), None)
            .await
            .is_err());
        let records: BTreeMap<_, _> = journal
            .validated_records()
            .await
            .unwrap()
            .into_iter()
            .map(|record| (record.admission.actor, record))
            .collect();
        assert!(validate_startup(
            &records,
            &DurableActorAdmission::capture(d, &root, &[]),
            Some(&branch)
        )
        .is_err());
        // Resolution follows both unpublished successors, while raw records
        // retain a single issuing owner's facts rather than copied snapshots.
        assert_eq!(
            journal.initial_context(c).await.unwrap(),
            Some(inherited.clone())
        );
        assert!(records[&b].initial_context.is_none());
        assert!(records[&c].initial_context.is_none());
        assert!(startup_chain_contains(&records, c, a));
        journal
            .bind_application_conversation(c, third.conversation.clone())
            .await
            .unwrap();
        let records: BTreeMap<_, _> = journal
            .validated_records()
            .await
            .unwrap()
            .into_iter()
            .map(|record| (record.admission.actor, record))
            .collect();
        assert!(!startup_chain_contains(&records, c, a));
        let crossing = startup_intent(
            d,
            &binding,
            Some(c),
            Some(a),
            Some(pin),
            Some(initial.conversation),
        );
        assert!(validate_startup(
            &records,
            &DurableActorAdmission::capture(d, &root, &[]),
            Some(&crossing)
        )
        .is_err());
        assert_eq!(records.len(), 3);
    }
    #[tokio::test]
    async fn dropped_reply_preserves_admitted_write_and_owner_drains() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        journal
            .commands
            .send(Command::Pause {
                entered: entered_tx,
                resume: paused,
            })
            .unwrap();
        entered.await.unwrap();
        let actor = ActorRef::first(ActorId(51));
        let admission = DurableActorAdmission::capture(actor, &descriptor("owner"), &[]);
        let (reply, abandoned) = tokio::sync::oneshot::channel();
        journal
            .commands
            .send(Command::Admit {
                admission,
                startup: None,
                reply,
            })
            .unwrap();
        drop(abandoned);
        // The admitted write is still queued behind a controlled overlap.
        // A current-thread async caller continues while the blocking owner waits.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        resume.send(()).unwrap();
        assert_eq!(journal.records().await.unwrap()[0].admission.actor, actor);
        let mut status = journal.owner_status();
        drop(journal);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !matches!(*status.borrow(), JournalOwnerStatus::Stopped { .. }) {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *status.borrow(),
            JournalOwnerStatus::Stopped {
                clean: true,
                fenced: false
            }
        );
        let recovered =
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .unwrap();
        assert_eq!(recovered.records().await.unwrap()[0].admission.actor, actor);
    }

    #[tokio::test]
    async fn append_failure_permanently_fences_even_after_path_repair() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let path = directory.path().join("actors.jsonl");
        let retained = directory.path().join("retained.jsonl");
        std::fs::rename(&path, &retained).unwrap();
        std::fs::create_dir(&path).unwrap();
        let actor = ActorRef::first(ActorId(52));
        assert!(journal
            .admit(actor, &descriptor("owner"), &[])
            .await
            .is_err());
        assert_eq!(*journal.owner_status().borrow(), JournalOwnerStatus::Fenced);
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&retained, &path).unwrap();
        assert!(journal
            .admit(actor, &descriptor("owner"), &[])
            .await
            .is_err());
        assert!(journal
            .active_application_conversation(actor)
            .await
            .is_err());
        assert!(journal.validated_records().await.is_err());
        assert!(journal.records().await.unwrap().is_empty());
        drop(journal);
        let recovered =
            ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone())
                .await
                .unwrap();
        recovered
            .admit(actor, &descriptor("owner"), &[])
            .await
            .unwrap();
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(32))]
        #[test]
        fn journal_histories_match_independent_lifecycle_model(steps in proptest::collection::vec(0u8..7, 1..48), terminal_kind in 0u8..4) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let directory = Arc::new(tempfile::tempdir().unwrap());
                let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
                let mut journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone()).await.unwrap();
                let actor = ActorRef::first(ActorId(53));
                let binding = directory.path().join("binding.json");
                let kind = match terminal_kind {
                    0 => ActorExitKind::Completed,
                    1 => ActorExitKind::Failed,
                    2 => ActorExitKind::Cancelled,
                    _ => ActorExitKind::Interrupted,
                };
                let terminal = crate::ActorTerminal::new(kind, "same terminal text");
                let (mut admitted, mut prepared, mut bound, mut retired) = (false, false, false, false);
                let mut durable_events = 1;
                let mut alternate_binding = false;
                for step in steps {
                    let (actual, expected, changed) = match step {
                        0 => {
                            let expected = !retired;
                            let changed = expected && !admitted;
                            let actual = journal.admit(actor, &descriptor("owner"), &[]).await.is_ok();
                            if expected { admitted = true; }
                            (actual, expected, changed)
                        }
                        1 => {
                            let expected = admitted && !retired;
                            let changed = expected && !prepared;
                            let actual = journal.prepare_application(actor, binding.clone(), None).await.is_ok();
                            if expected { prepared = true; }
                            (actual, expected, changed)
                        }
                        2 => {
                            let expected = prepared && !retired && (!bound || !alternate_binding);
                            let changed = expected && !bound;
                            let actual = journal.bind_application(actor, "conversation".into()).await.is_ok();
                            if expected { bound = true; }
                            (actual, expected, changed)
                        }
                        3 => {
                            let expected = admitted;
                            let changed = expected && !retired;
                            let actual = journal.retire(actor, terminal.clone()).await.is_ok();
                            if expected { retired = true; }
                            (actual, expected, changed)
                        }
                        4 => {
                            let expected = prepared && !retired && (!bound || alternate_binding);
                            let changed = expected && !bound;
                            let actual = journal.bind_application(actor, "conflicting".into()).await.is_ok();
                            if expected { bound = true; alternate_binding = true; }
                            (actual, expected, changed)
                        },
                        5 => {
                            drop(journal);
                            journal = ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl", directory.clone()).await.unwrap();
                            (true, true, false)
                        }
                        _ => {
                            let observed = journal.active_application_conversation(actor).await.unwrap();
                            let expected = (bound && !retired).then(|| ApplicationConversation::Codex {
                                thread_id: if alternate_binding { "conflicting" } else { "conversation" }.into(),
                            });
                            assert_eq!(observed, expected);
                            (true, true, false)
                        }
                    };
                    if changed { durable_events += 1; }
                    assert_eq!(actual, expected, "step {step}");
                    let records = journal.records().await.unwrap();
                    assert_eq!(records.len(), usize::from(admitted));
                    if let Some(record) = records.first() {
                        assert_eq!(record.application.is_some(), prepared);
                        assert_eq!(record.application.as_ref().is_some_and(|app| app.conversation.is_some()), bound);
                        assert_eq!(record.terminal.as_ref(), retired.then_some(&terminal));
                        let expected_conversation = bound.then(|| ApplicationConversation::Codex {
                            thread_id: if alternate_binding { "conflicting" } else { "conversation" }.into(),
                        });
                        assert_eq!(record.application.as_ref().and_then(|app| app.conversation.clone()), expected_conversation);
                    }
                    assert_eq!(std::fs::read_to_string(directory.path().join("actors.jsonl")).unwrap().lines().count(), durable_events);
                }
            });
        }
    }
    #[tokio::test]
    async fn owner_crash_reports_termination_and_refuses_later_commands() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let mut status = journal.owner_status();
        let (reply, result) = tokio::sync::oneshot::channel();
        journal.commands.send(Command::Crash { reply }).unwrap();
        assert!(result.await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !matches!(*status.borrow(), JournalOwnerStatus::Stopped { .. }) {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *status.borrow(),
            JournalOwnerStatus::Stopped {
                clean: false,
                fenced: false
            }
        );
        assert!(journal.records().await.is_err());
    }

    #[tokio::test]
    async fn uncertain_readback_permanently_fences_writer() {
        use std::io::Write;
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let path = directory.path().join("actors.jsonl");
        let original = std::fs::read(&path).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{torn").unwrap();
        file.sync_all().unwrap();
        assert!(journal.validated_records().await.is_err());
        assert_eq!(*journal.owner_status().borrow(), JournalOwnerStatus::Fenced);
        std::fs::write(&path, original).unwrap();
        assert!(journal.validated_records().await.is_err());
        assert!(journal
            .admit(ActorRef::first(ActorId(54)), &descriptor("owner"), &[])
            .await
            .is_err());
    }
    #[tokio::test]
    async fn binding_ack_is_exact_issuer_evidence_and_duplicates_do_not_append() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let other = ActorRecoveryJournal::open(&anchor, "other.jsonl", directory.clone())
            .await
            .unwrap();
        let actor = ActorRef::first(ActorId(55));
        journal
            .admit(actor, &descriptor("owner"), &[])
            .await
            .unwrap();
        journal
            .prepare_application(actor, directory.path().join("binding.json"), None)
            .await
            .unwrap();
        let first = journal
            .bind_application(actor, "conversation".into())
            .await
            .unwrap();
        let duplicate = journal
            .bind_application(actor, "conversation".into())
            .await
            .unwrap();
        assert_eq!(first.actor(), actor);
        assert_eq!(
            first.conversation(),
            &ApplicationConversation::Codex {
                thread_id: "conversation".into()
            }
        );
        assert!(first.belongs_to(&journal));
        assert!(!first.belongs_to(&other));
        assert_eq!(first.confirmed_sequence(), duplicate.confirmed_sequence());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("actors.jsonl"))
                .unwrap()
                .lines()
                .count(),
            first.confirmed_sequence() as usize
        );
    }
    #[tokio::test]
    async fn unissued_startup_parameters_do_not_admit_or_append() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", directory.clone())
            .await
            .unwrap();
        let actor = ActorRef::first(ActorId(56));
        let root =
            descriptor("root").with_actor_path(tidepool_repr::ActorPath::parse("root").unwrap());
        let startup = startup_intent(
            actor,
            &directory.path().join("binding.json"),
            None,
            None,
            None,
            None,
        );
        assert!(journal
            .admit_with_startup(actor, &root, &[], Some(startup), None)
            .await
            .is_err());
        assert!(journal.records().await.unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("actors.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn observed_read_preserves_filesystem_kind_and_malformed_data_distinction() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let error = ActorRecoveryJournal::read_observed(directory.path()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::IsADirectory);
        let path = directory.path().join("malformed.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        let error = ActorRecoveryJournal::read_observed(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
    #[tokio::test]
    async fn enclosing_lifetime_survives_last_sender_until_queued_write_drains() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let anchor = DirectoryAnchor::open_existing(&path).unwrap();
        let (drained_tx, drained) = tokio::sync::oneshot::channel();
        let lifetime = Arc::new(LifetimeProbe {
            directory: Some(directory),
            closed: Some(drained_tx),
        });
        let weak = Arc::downgrade(&lifetime);
        let journal = ActorRecoveryJournal::open(&anchor, "actors.jsonl", lifetime.clone())
            .await
            .unwrap();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        journal
            .commands
            .send(Command::Pause {
                entered: entered_tx,
                resume: paused,
            })
            .unwrap();
        entered.await.unwrap();
        let actor = ActorRef::first(ActorId(57));
        let admission = DurableActorAdmission::capture(actor, &descriptor("owner"), &[]);
        let (reply, abandoned) = tokio::sync::oneshot::channel();
        journal
            .commands
            .send(Command::Admit {
                admission,
                startup: None,
                reply,
            })
            .unwrap();
        drop(abandoned);
        drop(journal);
        drop(lifetime);
        assert!(weak.upgrade().is_some());
        assert!(path.exists());
        resume.send(()).unwrap();
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), drained)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().count(), 2);
        let row = parse_row(text.lines().nth(1).unwrap()).unwrap();
        let EventKind::Admitted { admission, .. } = row.event else {
            panic!("queued admission did not drain");
        };
        assert_eq!(admission.actor, actor);
        assert!(weak.upgrade().is_none());
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn cancelled_open_retains_enclosing_lifetime_until_real_bootstrap_exits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let anchor = DirectoryAnchor::open_existing(&path).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let lifetime = Arc::new(LifetimeProbe {
            directory: Some(directory),
            closed: Some(closed_tx),
        });
        let weak = Arc::downgrade(&lifetime);
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let mut opening = Box::pin(ActorRecoveryJournal::start(
            anchor,
            "actors.jsonl".into(),
            false,
            lifetime.clone(),
            Some(BootstrapPause {
                entered: entered_tx,
                resume: paused,
            }),
        ));
        tokio::select! {
            _ = &mut opening => panic!("opening completed before bootstrap overlap was released"),
            entered = entered => entered.unwrap(),
        }
        drop(opening);
        drop(lifetime);
        assert!(weak.upgrade().is_some());
        assert!(!path.join("actors.jsonl").exists());
        resume.send(()).unwrap();
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), closed)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(matches!(
            parse_row(text.lines().next().unwrap()).unwrap().event,
            EventKind::Created {}
        ));
        assert!(weak.upgrade().is_none());
        assert!(!path.exists());
    }
}
