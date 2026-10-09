//! Source-owned installer preparation; waiters do not own the shared task.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, CompiledTurn, ImageRegistry, PreparedSourceEntry,
    TurnClassification, TurnKind, TurnRequest, TurnResult,
};
use tracing::{instrument::WithSubscriber, Instrument};

use super::ResolvedSpec;

/// The acquisition that issued immutable installer readiness. Ready-cache
/// hits and joined waiters retain this original provenance.
/// Reconstructing this observation grants no source or compiler authority.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolsetAcquisition {
    DeploymentOriginal {
        recipe: String,
        original: uuid::Uuid,
    },
    FreshRunOriginal {
        recipe: String,
        original: uuid::Uuid,
    },
    ExistingRunOriginal {
        recipe: String,
        original: uuid::Uuid,
    },
    UnretainedCompilation {
        recipe: String,
    },
}

impl ToolsetAcquisition {
    #[must_use]
    pub fn recipe(&self) -> &str {
        match self {
            Self::DeploymentOriginal { recipe, .. }
            | Self::FreshRunOriginal { recipe, .. }
            | Self::ExistingRunOriginal { recipe, .. }
            | Self::UnretainedCompilation { recipe } => recipe,
        }
    }

    /// Selection metadata is derived from the issuing acquisition, never an
    /// independent claim that compilation reused a deployment.
    #[must_use]
    pub fn completed_entry_selection(&self) -> Option<(&str, uuid::Uuid)> {
        match self {
            Self::DeploymentOriginal { recipe, original }
            | Self::FreshRunOriginal { recipe, original }
            | Self::ExistingRunOriginal { recipe, original } => Some((recipe, *original)),
            Self::UnretainedCompilation { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub(crate) struct InstallerRecipe {
    /// In-memory coalescing retains the exact issuer and source selection.
    /// Durable addresses are independent of a fresh run's issuer identity.
    #[serde(skip)]
    pub(crate) source_authority: [u8; 32],
    pub(crate) source_revision: String,
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) preamble: String,
    pub(crate) imports: String,
    pub(crate) entry: String,
    pub(crate) effects: Vec<crate::ActorEffectKey>,
}

pub(crate) struct PreparedToolset {
    pub(crate) entry: PreparedSourceEntry,
    pub(crate) entry_name: String,
    pub(crate) resolved: ResolvedSpec,
    pub(crate) source_revision: String,
    pub(crate) acquisition: ToolsetAcquisition,
    /// Retains the published source owner for the complete installer lifetime.
    _source: crate::CheckpointSourceLayer,
    /// Nominal owners include producer and original canonical interface identity.
    _nominal_artifacts: Vec<tidepool_toolchain::artifact_inventory::ArtifactDescriptor>,
}

fn nominal_artifacts(
    compiled: &CompiledTurn,
    effects: &[crate::ActorEffectKey],
) -> Result<Vec<tidepool_toolchain::artifact_inventory::ArtifactDescriptor>, PreparationFailure> {
    use tidepool_toolchain::artifact_inventory::ArtifactKind;
    let descriptors = compiled.source_artifacts();
    let mut modules = vec!["Tidepool.Effects.Core", "Tidepool.Agent.Contract"];
    if effects.contains(&crate::ActorEffectKey::Replies) {
        modules.push("Tidepool.Agent.Reply.Internal");
    }
    if effects.contains(&crate::ActorEffectKey::Watches) {
        modules.push("Tidepool.Agent.Watch.Internal");
    }
    modules
        .into_iter()
        .map(|module| {
            let selected = descriptors
                .iter()
                .filter(|descriptor| {
                    descriptor.owner.module == module
                        && matches!(
                            descriptor.kind,
                            ArtifactKind::OriginalModule | ArtifactKind::CanonicalModuleInterface
                        )
                })
                .collect::<Vec<_>>();
            let Some(first) = selected.first() else {
                return Err(PreparationFailure::Source(format!(
                    "installer has no original nominal artifact for {module}",
                )));
            };
            if selected.iter().any(|descriptor| {
                descriptor.owner != first.owner
                    || descriptor.producer_sha256 != first.producer_sha256
                    || descriptor.interface_sha256 != first.interface_sha256
            }) {
                return Err(PreparationFailure::Source(format!(
                    "installer selects conflicting nominal artifacts for {module}",
                )));
            }
            Ok((**first).clone())
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) enum PreparationFailure {
    AbsentSelection { recipe: String },
    Admission(Arc<crate::ResidentActorWorkbenchError>),
    Source(String),
    Compiler(tidepool_toolchain::failclass::FailureEnvelope),
    Native(String),
}

struct PreparationTask {
    outcome: Mutex<Option<Result<Arc<PreparedToolset>, PreparationFailure>>>,
    completed: tokio::sync::Notify,
}

impl PreparationTask {
    async fn wait(&self) -> Result<Arc<PreparedToolset>, PreparationFailure> {
        loop {
            let completed = self.completed.notified();
            tokio::pin!(completed);
            completed.as_mut().enable();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome;
            }
            completed.await;
        }
    }
}

#[derive(Default)]
pub(crate) struct ToolsetPreparation {
    state: Mutex<PreparationState>,
    compiler_owner: crate::RetainedActorExit,
    #[cfg(test)]
    fresh_launch_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    completed_load_observer: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Default)]
struct PreparationState {
    tasks: HashMap<InstallerRecipe, Arc<PreparationTask>>,
    ready_order: VecDeque<InstallerRecipe>,
}

const RETAINED_TOOLSETS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreparationLookupDisposition {
    New,
    JoinedPending,
    ReadyHit,
}

impl PreparationLookupDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::JoinedPending => "joined_pending",
            Self::ReadyHit => "ready_hit",
        }
    }
}

#[derive(Clone, Copy)]
enum OriginalAcquisition {
    CompileIfAbsent,
    LoadCompleted,
}

impl ToolsetPreparation {
    /// Delay only a winning real compile; the hook grants no compiler authority.
    #[cfg(test)]
    pub(crate) fn observe_fresh_launch(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        let mut slot = self.fresh_launch_observer.lock();
        assert!(slot.is_none(), "fresh launch observer already installed");
        *slot = Some(observer);
    }

    #[cfg(test)]
    pub(crate) fn observe_completed_load(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        let mut slot = self.completed_load_observer.lock();
        assert!(slot.is_none(), "completed load observer already installed");
        *slot = Some(observer);
    }

    fn lookup(
        &self,
        recipe: &InstallerRecipe,
    ) -> (Arc<PreparationTask>, PreparationLookupDisposition) {
        let mut state = self.state.lock();
        match state.tasks.get(recipe).cloned() {
            Some(task) => {
                let disposition = if state.ready_order.contains(recipe) {
                    state.ready_order.retain(|key| key != recipe);
                    state.ready_order.push_back(recipe.clone());
                    PreparationLookupDisposition::ReadyHit
                } else {
                    PreparationLookupDisposition::JoinedPending
                };
                (task, disposition)
            }
            None => {
                let task = Arc::new(PreparationTask {
                    outcome: Mutex::new(None),
                    completed: tokio::sync::Notify::new(),
                });
                state.tasks.insert(recipe.clone(), Arc::clone(&task));
                (task, PreparationLookupDisposition::New)
            }
        }
    }

    pub(crate) async fn prepare(
        self: &Arc<Self>,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        recipe: InstallerRecipe,
        resolved: ResolvedSpec,
        source: crate::CheckpointSourceLayer,
        registry: Arc<ImageRegistry>,
    ) -> Result<Arc<PreparedToolset>, PreparationFailure> {
        let (task, disposition) = self.lookup(&recipe);
        tracing::debug!(
            target: "exomonad_actor::toolset_preparation",
            phase = "lookup",
            disposition = disposition.as_str(),
            source_revision = %recipe.source_revision,
            entry = %recipe.entry,
            "toolset preparation lookup"
        );
        if disposition == PreparationLookupDisposition::New {
            let admitted =
                crate::resident_workbench::CompilerCloseOwner::current().and_then(|owner| {
                    owner
                        .shared_preparation(self.compiler_owner.clone())
                        .register_work()
                });
            let compiler_work = match admitted {
                Ok(ticket) => ticket,
                Err(error) => {
                    self.settle(
                        recipe,
                        &task,
                        Err(PreparationFailure::Admission(Arc::new(error))),
                    );
                    return task.wait().await;
                }
            };
            #[cfg(test)]
            let observer_owner = Arc::clone(self);
            let task = Arc::clone(&task);
            let owner = Arc::clone(self);
            let key = recipe.clone();
            // This task belongs to the preparation owner, independently of any
            // actor waiting for it. Dropping a waiter cannot interrupt its peers.
            tokio::spawn(
                async move {
                    let outcome = tidepool_runtime::spawn_blocking_in_span(move || {
                        compiler_work.run_for_workload(workload, || {
                            tidepool_extract_cmd::compiler_host_checkpoint()
                                .map_err(preparation_io_failure)?;
                            let completed_original = selected_original_present(&recipe, &source)?;
                            let acquisition = if completed_original {
                                OriginalAcquisition::LoadCompleted
                            } else {
                                OriginalAcquisition::CompileIfAbsent
                            };
                            #[cfg(test)]
                            {
                                let observer = if completed_original {
                                    &observer_owner.completed_load_observer
                                } else {
                                    &observer_owner.fresh_launch_observer
                                };
                                let observer = observer.lock().take();
                                if let Some(observer) = observer {
                                    observer();
                                }
                            }
                            compile_installer(recipe, resolved, source, registry, acquisition)
                        })
                    })
                    .await
                    .unwrap_or_else(|error| Err(PreparationFailure::Native(error.to_string())));
                    owner.settle(key, &task, outcome);
                }
                .in_current_span()
                .with_current_subscriber(),
            );
        }
        task.wait().await
    }

    fn settle(
        &self,
        key: InstallerRecipe,
        task: &Arc<PreparationTask>,
        outcome: Result<Arc<PreparedToolset>, PreparationFailure>,
    ) {
        let succeeded = outcome.is_ok();
        let mut state = self.state.lock();
        // A waiter can observe completion while this lock is held, but its next
        // lookup cannot pass the failed-task retirement or ready-cache bound.
        *task.outcome.lock() = Some(outcome);
        if !state
            .tasks
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, task))
        {
            drop(state);
            task.completed.notify_waiters();
            return;
        }
        if !succeeded {
            state.tasks.remove(&key);
            drop(state);
            task.completed.notify_waiters();
            return;
        }
        state.ready_order.push_back(key);
        while state.ready_order.len() > RETAINED_TOOLSETS {
            if let Some(retired) = state.ready_order.pop_front() {
                state.tasks.remove(&retired);
            }
        }
        drop(state);
        task.completed.notify_waiters();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn recipe(entry: &str) -> InstallerRecipe {
        InstallerRecipe {
            source_authority: [0; 32],
            source_revision: "frozen source".into(),
            roots: Vec::new(),
            preamble: String::new(),
            imports: String::new(),
            entry: entry.into(),
            effects: Vec::new(),
        }
    }

    /// The consumer supplies a genuinely issued entry; this exercises cache
    /// retirement without manufacturing source/native authority for a fixture.
    pub(crate) async fn ready_bound_preserves_installed_lease(ready: Arc<PreparedToolset>) {
        let owner = ToolsetPreparation::default();
        let first_key = recipe("oldest");
        let (first, disposition) = owner.lookup(&first_key);
        assert_eq!(disposition, PreparationLookupDisposition::New);
        let (joined, disposition) = owner.lookup(&first_key);
        assert_eq!(disposition, PreparationLookupDisposition::JoinedPending);
        assert!(Arc::ptr_eq(&first, &joined));
        drop(joined);
        let retired = Arc::downgrade(&first);
        owner.settle(first_key.clone(), &first, Ok(Arc::clone(&ready)));
        let (hit, disposition) = owner.lookup(&first_key);
        assert_eq!(disposition, PreparationLookupDisposition::ReadyHit);
        assert!(Arc::ptr_eq(&first, &hit));
        drop(hit);
        drop(first);
        for index in 0..RETAINED_TOOLSETS {
            let key = recipe(&format!("ready-{index}"));
            let (task, disposition) = owner.lookup(&key);
            assert_eq!(disposition, PreparationLookupDisposition::New);
            owner.settle(key, &task, Ok(Arc::clone(&ready)));
        }
        assert_eq!(owner.state.lock().ready_order.len(), RETAINED_TOOLSETS);
        assert_eq!(owner.state.lock().tasks.len(), RETAINED_TOOLSETS);
        assert!(
            retired.upgrade().is_none(),
            "the seventeenth ready entry retires the oldest lookup custody"
        );
        let retained = Arc::downgrade(&ready);
        drop(owner);
        assert!(
            retained.upgrade().is_some(),
            "installed tool leases retain the original entry after cache retirement"
        );
        assert!(ready.entry.compiled().original_compile_input().is_some());
        run_cache_history(ready).await;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ModelPhase {
        Pending,
        Ready,
    }

    #[derive(Clone, Copy)]
    struct ModelEntry {
        task: usize,
        phase: ModelPhase,
    }

    /// Independent cache model: only successful settlement enters the LRU;
    /// failed settlement removes the current task; stale settlement is inert.
    #[derive(Default)]
    struct CacheModel {
        entries: HashMap<InstallerRecipe, ModelEntry>,
        ready_order: VecDeque<InstallerRecipe>,
        next_task: usize,
    }

    struct CacheHistory {
        owner: ToolsetPreparation,
        model: CacheModel,
        handles: HashMap<usize, (InstallerRecipe, Arc<PreparationTask>)>,
        ready: Arc<PreparedToolset>,
        payloads: HashMap<usize, Arc<PreparedToolset>>,
    }

    impl CacheHistory {
        fn new(ready: Arc<PreparedToolset>) -> Self {
            Self {
                owner: ToolsetPreparation::default(),
                model: CacheModel::default(),
                handles: HashMap::new(),
                ready,
                payloads: HashMap::new(),
            }
        }

        fn lookup(&mut self, key: InstallerRecipe) -> usize {
            let (expected, expected_task) = match self.model.entries.get(&key).copied() {
                Some(ModelEntry {
                    task,
                    phase: ModelPhase::Pending,
                }) => (PreparationLookupDisposition::JoinedPending, task),
                Some(ModelEntry {
                    task,
                    phase: ModelPhase::Ready,
                }) => {
                    self.model.ready_order.retain(|candidate| candidate != &key);
                    self.model.ready_order.push_back(key.clone());
                    (PreparationLookupDisposition::ReadyHit, task)
                }
                None => {
                    let task = self.model.next_task;
                    self.model.next_task += 1;
                    self.model.entries.insert(
                        key.clone(),
                        ModelEntry {
                            task,
                            phase: ModelPhase::Pending,
                        },
                    );
                    (PreparationLookupDisposition::New, task)
                }
            };

            let (actual, disposition) = self.owner.lookup(&key);
            assert_eq!(
                disposition, expected,
                "lookup disposition for {}",
                key.entry
            );
            if let Some((_, expected_handle)) = self.handles.get(&expected_task) {
                assert!(Arc::ptr_eq(&actual, expected_handle));
            } else {
                assert!(self
                    .handles
                    .values()
                    .all(|(_, issued)| !Arc::ptr_eq(&actual, issued)));
                self.handles
                    .insert(expected_task, (key.clone(), Arc::clone(&actual)));
            }
            self.assert_matches_model();
            expected_task
        }

        fn lookup_with_payload(
            &mut self,
            key: InstallerRecipe,
            payload: &Arc<PreparedToolset>,
        ) -> usize {
            let task = self.lookup(key);
            if let Some(expected) = self.payloads.get(&task) {
                assert!(Arc::ptr_eq(expected, payload));
            } else {
                self.payloads.insert(task, Arc::clone(payload));
            }
            self.assert_matches_model();
            task
        }

        fn settle_success(&mut self, task: usize) {
            self.settle(task, true);
        }

        fn settle_failure(&mut self, task: usize) {
            self.settle(task, false);
        }

        fn settle(&mut self, task: usize, success: bool) {
            let (key, handle) = self.handles.get(&task).expect("history task exists");
            let key = key.clone();
            let handle = Arc::clone(handle);
            let current = self.model.entries.get(&key).copied();
            let is_current_pending = current
                .is_some_and(|entry| entry.task == task && entry.phase == ModelPhase::Pending);
            let outcome = if success {
                Ok(Arc::clone(self.payloads.get(&task).unwrap_or(&self.ready)))
            } else {
                Err(PreparationFailure::Source(format!(
                    "history failure {task}"
                )))
            };
            self.owner.settle(key.clone(), &handle, outcome);

            if is_current_pending {
                if success {
                    self.model.entries.insert(
                        key.clone(),
                        ModelEntry {
                            task,
                            phase: ModelPhase::Ready,
                        },
                    );
                    self.model.ready_order.retain(|candidate| candidate != &key);
                    self.model.ready_order.push_back(key);
                    while self.model.ready_order.len() > RETAINED_TOOLSETS {
                        if let Some(retired) = self.model.ready_order.pop_front() {
                            self.model.entries.remove(&retired);
                        }
                    }
                } else {
                    self.model.entries.remove(&key);
                }
            }
            self.assert_matches_model();
        }

        fn assert_matches_model(&self) {
            let actual = self.owner.state.lock();
            assert_eq!(actual.ready_order, self.model.ready_order);
            assert_eq!(actual.tasks.len(), self.model.entries.len());
            for (key, expected) in &self.model.entries {
                let task = actual.tasks.get(key).expect("modeled task is retained");
                let (_, issued) = self
                    .handles
                    .get(&expected.task)
                    .expect("issued task handle");
                assert!(Arc::ptr_eq(task, issued));
                assert_eq!(
                    self.model.ready_order.contains(key),
                    expected.phase == ModelPhase::Ready
                );
                let outcome = task.outcome.lock();
                match (expected.phase, outcome.as_ref()) {
                    (ModelPhase::Pending, None) => {}
                    (ModelPhase::Ready, Some(Ok(prepared))) => {
                        assert!(self.payload_matches(expected.task, prepared));
                    }
                    _ => panic!("retained preparation outcome differs from modeled readiness"),
                }
            }
        }

        fn payload_matches(&self, task: usize, prepared: &Arc<PreparedToolset>) -> bool {
            let expected = self.payloads.get(&task).unwrap_or(&self.ready);
            Arc::ptr_eq(prepared, expected)
                && prepared.acquisition == expected.acquisition
                && prepared.source_revision == expected.source_revision
        }
    }

    /// Both products are issued by the real source installer fixture once;
    /// generated histories create fresh cache/task state without compilation.
    pub(crate) fn distinct_ready_payload_histories(payloads: [Arc<PreparedToolset>; 2]) {
        use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
        use std::cell::Cell;

        assert!(!Arc::ptr_eq(&payloads[0], &payloads[1]));
        assert_ne!(payloads[0].acquisition, payloads[1].acquisition);
        assert_ne!(payloads[0].source_revision, payloads[1].source_revision);
        // Mutate only a settled payload, preserving key, task, disposition and
        // retention state. The old shared-payload fixture could not expose this.
        let mut sensitivity = CacheHistory::new(Arc::clone(&payloads[0]));
        let task = sensitivity.lookup_with_payload(recipe("payload-swap"), &payloads[0]);
        sensitivity.settle_success(task);
        let handle = &sensitivity.handles[&task].1;
        *handle.outcome.lock() = Some(Ok(Arc::clone(&payloads[1])));
        let outcome = handle.outcome.lock();
        let Some(Ok(swapped)) = outcome.as_ref() else {
            unreachable!()
        };
        assert!(!sensitivity.payload_matches(task, swapped));
        drop(outcome);
        drop(sensitivity);
        let mut config = Config::default();
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 192;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::distinct_ready_payload_histories"
        ));
        let configured = config.cases;
        let completed = Cell::new(0usize);
        let result = TestRunner::new(config).run(
            &proptest::collection::vec((0usize..23, 0u8..3), 0..64),
            |operations| {
                let mut history = CacheHistory::new(Arc::clone(&payloads[0]));
                let key = |index| recipe(&format!("distinct-history-{index}"));
                let mut issued = Vec::new();
                // Both payloads cross the actual retention bound in every case.
                for index in 0..RETAINED_TOOLSETS + 4 {
                    let task = history.lookup_with_payload(key(index), &payloads[index % 2]);
                    history.settle_success(task);
                    issued.push(task);
                }
                for (index, task) in issued.iter().enumerate() {
                    let outcome = history.handles[task].1.outcome.lock();
                    let Some(Ok(prepared)) = outcome.as_ref() else {
                        panic!("retained waiter lost its original result");
                    };
                    assert!(Arc::ptr_eq(prepared, &payloads[index % 2]));
                }
                // A stale owner cannot replace the new pending task or payload.
                let retry = key(22);
                let old = history.lookup_with_payload(retry.clone(), &payloads[0]);
                history.settle_failure(old);
                let new = history.lookup_with_payload(retry, &payloads[0]);
                assert_ne!(old, new);
                history.settle_failure(old);
                history.settle_success(new);

                for (index, operation) in operations {
                    let task = history.lookup_with_payload(key(index), &payloads[index % 2]);
                    // A producer settles exactly once. Ready hits remain reads.
                    if history.model.entries[&key(index)].phase == ModelPhase::Pending {
                        match operation {
                            0 => { history.lookup_with_payload(key(index), &payloads[index % 2]); }
                            1 => history.settle_success(task),
                            2 => history.settle_failure(task),
                            _ => unreachable!(),
                        }
                    }
                }
                history.assert_matches_model();
                let count = completed.get() + 1;
                completed.set(count);
                if count.is_power_of_two() {
                    eprintln!("distinct cache payload history: immutable_products=2, completed_cases={count}");
                }
                Ok(())
            },
        );
        eprintln!("distinct cache payload history: configured_cases={configured}, completed_cases={}, immutable_products=2", completed.get());
        result.expect("distinct prepared payload histories match independent cache model");
    }

    async fn run_cache_history(ready: Arc<PreparedToolset>) {
        let original_acquisition = ready.acquisition.clone();
        let mut history = CacheHistory::new(Arc::clone(&ready));

        // Populate beyond the real retention bound, then touch an older ready
        // item to check that successful lookup updates the model's LRU order.
        let mut issued = Vec::new();
        for index in 0..(RETAINED_TOOLSETS + 4) {
            let key = recipe(&format!("history-ready-{index}"));
            let task = history.lookup(key);
            assert_eq!(
                history.lookup(recipe(&format!("history-ready-{index}"))),
                task
            );
            history.settle_success(task);
            issued.push(task);
        }
        let recent = history.lookup(recipe("history-ready-5"));
        assert_eq!(recent, issued[5]);
        let next = history.lookup(recipe("history-ready-5"));
        assert_eq!(next, recent);
        assert!(
            !history
                .model
                .entries
                .contains_key(&recipe("history-ready-0")),
            "successes beyond the bound retire the least recently used entry"
        );
        for task in &issued[..4] {
            let outcome = history.handles[task].1.outcome.lock();
            let Some(Ok(prepared)) = outcome.as_ref() else {
                panic!("an evicted lookup retains its admitted waiter's original result");
            };
            assert!(Arc::ptr_eq(prepared, &ready));
            assert_eq!(prepared.acquisition, original_acquisition);
        }

        // A failed owner retires only its task. A later failed settlement from
        // that old task must leave the replacement pending and cached.
        let retry_key = recipe("history-retry");
        let failed = history.lookup(retry_key.clone());
        history.settle_failure(failed);
        let replacement = history.lookup(retry_key.clone());
        assert_ne!(failed, replacement);
        history.settle_failure(failed);
        assert_eq!(
            history
                .model
                .entries
                .get(&retry_key)
                .map(|entry| entry.task),
            Some(replacement)
        );
        history.settle_success(replacement);

        // Deterministic pseudo-random histories interleave pending joins,
        // ready hits, failures, successful publication and LRU retirement.
        let keys = (0..23)
            .map(|index| recipe(&format!("history-seeded-{index}")))
            .collect::<Vec<_>>();
        let mut random = 0x4d59_5df4_d0f3_3173_u64;
        for _ in 0..192 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let key = keys[(random as usize) % keys.len()].clone();
            match history.model.entries.get(&key).copied() {
                None => {
                    history.lookup(key);
                }
                Some(ModelEntry {
                    task,
                    phase: ModelPhase::Pending,
                }) => {
                    if random & 1 == 0 {
                        history.lookup(key);
                    } else if random & 2 == 0 {
                        history.settle_success(task);
                    } else {
                        history.settle_failure(task);
                    }
                }
                Some(ModelEntry {
                    phase: ModelPhase::Ready,
                    ..
                }) => {
                    history.lookup(key);
                }
            }
        }

        // Retain one waiter while cancelling another. Settlement still reaches
        // the live waiter, and the model observes the failed key's retirement.
        let waiter_key = recipe("history-waiter");
        let waiter_task = history.lookup(waiter_key.clone());
        let task = Arc::clone(&history.handles[&waiter_task].1);
        let cancelled = tokio::spawn({
            let task = Arc::clone(&task);
            async move { task.wait().await }
        });
        let retained = tokio::spawn({
            let task = Arc::clone(&task);
            async move { task.wait().await }
        });
        tokio::task::yield_now().await;
        cancelled.abort();
        history.settle_failure(waiter_task);
        match cancelled.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("cancelled preparation waiter unexpectedly completed"),
        }
        assert!(matches!(
            retained.await.unwrap(),
            Err(PreparationFailure::Source(detail)) if detail == format!("history failure {waiter_task}")
        ));
        assert!(!history.model.entries.contains_key(&waiter_key));
        history.assert_matches_model();
    }

    #[test]
    fn failed_preparation_retires_only_its_exact_lookup_task() {
        let owner = ToolsetPreparation::default();
        let key = recipe("failed");
        let (failed, disposition) = owner.lookup(&key);
        assert_eq!(disposition, PreparationLookupDisposition::New);
        owner.settle(
            key.clone(),
            &failed,
            Err(PreparationFailure::Source("transient".into())),
        );
        assert!(!owner.state.lock().tasks.contains_key(&key));
        let (retry, disposition) = owner.lookup(&key);
        assert_eq!(disposition, PreparationLookupDisposition::New);
        owner.settle(
            key.clone(),
            &failed,
            Err(PreparationFailure::Source("late original refusal".into())),
        );
        assert!(Arc::ptr_eq(
            owner.state.lock().tasks.get(&key).unwrap(),
            &retry
        ));
        assert!(
            failed.outcome.lock().is_some(),
            "admitted waiters retain their settled refusal"
        );
    }

    #[tokio::test]
    async fn dropping_one_waiter_preserves_the_other_waiter_and_completion() {
        let task = Arc::new(PreparationTask {
            outcome: Mutex::new(None),
            completed: tokio::sync::Notify::new(),
        });
        let cancelled_task = Arc::clone(&task);
        let cancelled = tokio::spawn(async move { cancelled_task.wait().await });
        let remaining_task = Arc::clone(&task);
        let remaining = tokio::spawn(async move { remaining_task.wait().await });
        tokio::task::yield_now().await;
        cancelled.abort();
        *task.outcome.lock() = Some(Err(PreparationFailure::Source("original refusal".into())));
        task.completed.notify_waiters();
        assert!(
            matches!(remaining.await.unwrap(), Err(PreparationFailure::Source(detail)) if detail == "original refusal")
        );
        assert!(
            matches!(task.wait().await, Err(PreparationFailure::Source(detail)) if detail == "original refusal")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observed_failure_has_already_retired_before_immediate_retry() {
        let owner = Arc::new(ToolsetPreparation::default());
        let key = recipe("retry");
        let (failed, disposition) = owner.lookup(&key);
        assert_eq!(disposition, PreparationLookupDisposition::New);
        let waiter_owner = Arc::clone(&owner);
        let waiter_key = key.clone();
        let waiter_task = Arc::clone(&failed);
        let waiter = tokio::spawn(async move {
            assert!(waiter_task.wait().await.is_err());
            let (retry, disposition) = waiter_owner.lookup(&waiter_key);
            assert_eq!(disposition, PreparationLookupDisposition::New);
            assert!(!Arc::ptr_eq(&retry, &waiter_task));
        });
        owner.settle(
            key,
            &failed,
            Err(PreparationFailure::Source("transient".into())),
        );
        waiter.await.unwrap();
    }

    #[test]
    fn interrupted_original_authentication_preserves_infrastructure_classification() {
        let failure = preparation_auth_failure(tidepool_toolchain::CompileError::Io(
            std::io::Error::from(std::io::ErrorKind::Interrupted),
        ));
        assert!(matches!(failure, PreparationFailure::Compiler(diagnostic)
            if diagnostic.class == tidepool_toolchain::failclass::FailureClass::Infra));
        let ordinary = preparation_auth_failure(tidepool_toolchain::CompileError::ExtractFailed(
            "changed original".into(),
        ));
        assert!(matches!(ordinary, PreparationFailure::Source(_)));
    }

    #[test]
    fn only_an_absent_original_path_can_select_compilation() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("entry");
        assert!(!entry_path_present(&output).unwrap());
        std::os::unix::fs::symlink(root.path().join("missing"), &output).unwrap();
        assert!(entry_path_present(&output).is_err());
        std::fs::remove_file(&output).unwrap();
        std::fs::write(&output, b"invalid original").unwrap();
        assert!(entry_path_present(&output).is_err());
        std::fs::remove_file(&output).unwrap();
        std::fs::create_dir(&output).unwrap();
        assert!(entry_path_present(&output).unwrap());
    }

    #[test]
    fn completed_acquisition_cannot_create_source_after_original_disappears() {
        let root = tempfile::tempdir().unwrap();
        let preparation = uuid::Uuid::new_v4();
        let recipe = recipe("gone");
        let storage = crate::SourceEntryStorage::FreshCompilation {
            directory: root.path().to_owned(),
            preparation,
        };
        let original = tidepool_atomic_write::DirectoryAnchor::open_existing(root.path())
            .unwrap()
            .child(durable_recipe_key(&recipe).unwrap())
            .unwrap()
            .child(preparation.to_string())
            .unwrap();
        let entry = original.child("entry").unwrap();
        assert!(entry_path_present(entry.path()).unwrap());
        std::fs::remove_dir(entry.path()).unwrap();
        let before = tidepool_extract_cmd::extract_spawn_count();
        let result = retained_installer(
            &recipe,
            &storage,
            "module RetainedMissing where\n__prepared = (1 :: Int)\n",
            OriginalAcquisition::LoadCompleted,
        );
        assert!(result.is_err());
        assert!(!original.path().join("RetainedMissing.hs").exists());
        assert!(!entry.path().exists());
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
    }
}

fn compile_installer(
    recipe: InstallerRecipe,
    resolved: ResolvedSpec,
    source: crate::CheckpointSourceLayer,
    registry: Arc<ImageRegistry>,
    acquisition: OriginalAcquisition,
) -> Result<Arc<PreparedToolset>, PreparationFailure> {
    let installation = super::installation_expression(&recipe.entry, &recipe.effects);
    let dispatcher_effects = installation.dispatcher_effect_row();
    let templates =
        resident_workbench_templates(&recipe.preamble, &dispatcher_effects, &recipe.imports);
    let (compiled, acquisition) = if let Some(storage) = source.prepared_entries() {
        let template = templates
            .iter()
            .find(|template| {
                template.kind == tidepool_runtime::session::TemplateSelector::BindDiscard
            })
            .ok_or_else(|| {
                PreparationFailure::Source("installer has no discarded-bind template".into())
            })?;
        let wrapper = tidepool_runtime::session::render_template(
            &template.source,
            &installation.expression,
            &[],
        );
        retained_installer(&recipe, storage, &wrapper, acquisition)?
    } else {
        (
            compile_unprepared_installer(&recipe, &templates, &installation.expression)?,
            ToolsetAcquisition::UnretainedCompilation {
                recipe: durable_recipe_key(&recipe)?,
            },
        )
    };
    let compiled: Arc<CompiledTurn> = Arc::new(compiled);
    let nominal_artifacts = nominal_artifacts(&compiled, &recipe.effects)?;
    let entry = PreparedSourceEntry::prepare(compiled, registry)
        .map_err(|error| PreparationFailure::Native(error.to_string()))?;
    Ok(Arc::new(PreparedToolset {
        entry,
        entry_name: recipe.entry,
        resolved,
        source_revision: recipe.source_revision,
        acquisition,
        _source: source,
        _nominal_artifacts: nominal_artifacts,
    }))
}

pub(crate) fn durable_recipe_key(recipe: &InstallerRecipe) -> Result<String, PreparationFailure> {
    Ok(blake3::hash(
        &serde_json::to_vec(recipe)
            .map_err(|error| PreparationFailure::Source(error.to_string()))?,
    )
    .to_hex()
    .to_string())
}

/// Presence chooses physical compilation or completed-original authentication.
/// Both run inside the shared producer's scope; presence grants no custody.
fn selected_original_present(
    recipe: &InstallerRecipe,
    source: &crate::CheckpointSourceLayer,
) -> Result<bool, PreparationFailure> {
    match source.prepared_entries() {
        Some(crate::SourceEntryStorage::CompletedOriginal { .. }) => Ok(true),
        Some(crate::SourceEntryStorage::FreshCompilation {
            directory,
            preparation,
        }) => entry_path_present(
            &directory
                .join(durable_recipe_key(recipe)?)
                .join(preparation.to_string())
                .join("entry"),
        ),
        None => Ok(false),
    }
}

fn entry_path_present(path: &std::path::Path) -> Result<bool, PreparationFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(PreparationFailure::Source(format!(
            "completed installer path is not an original directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PreparationFailure::Source(error.to_string())),
    }
}

/// This explicit developer/fixture path has no retained source storage.
fn compile_unprepared_installer(
    recipe: &InstallerRecipe,
    templates: &[tidepool_runtime::session::TurnTemplate],
    expression: &str,
) -> Result<CompiledTurn, PreparationFailure> {
    let scratch =
        tempfile::tempdir().map_err(|error| PreparationFailure::Source(error.to_string()))?;
    let include = recipe
        .roots
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let result = run_turn(TurnRequest {
        exact_context: None,
        session_id: None,
        turn_text: expression,
        templates,
        include: &include,
        session_root: scratch.path(),
        inject_modules: &[],
        gen: 1,
        verdict: Some(TurnClassification {
            kind: TurnKind::Bind,
            binders: Vec::new(),
            items: Vec::new(),
        }),
        target: None,
        retained_imports: &[],
    })
    .map_err(|failure| {
        let mut diagnostic = tidepool_runtime::classify_compile(&failure.error);
        diagnostic.message = tidepool_runtime::session::render_turn_compile_error(
            &failure.error,
            failure.attempted_source.as_deref(),
            expression,
            "<agent-spec-installation>",
        );
        PreparationFailure::Compiler(diagnostic)
    })?;
    let TurnResult::Bind {
        bound, compiled, ..
    } = result
    else {
        return Err(PreparationFailure::Source(
            "source installer is not a discarded bind".into(),
        ));
    };
    if !bound.is_empty() {
        return Err(PreparationFailure::Source(
            "source installer exports public notebook bindings".into(),
        ));
    }
    Ok(compiled)
}

fn preparation_io_failure(error: std::io::Error) -> PreparationFailure {
    PreparationFailure::Compiler(tidepool_runtime::classify_compile(
        &tidepool_toolchain::CompileError::Io(error),
    ))
}

fn preparation_auth_failure(error: tidepool_toolchain::CompileError) -> PreparationFailure {
    if matches!(&error, tidepool_toolchain::CompileError::Io(error) if error.kind() == std::io::ErrorKind::Interrupted)
    {
        return PreparationFailure::Compiler(tidepool_runtime::classify_compile(&error));
    }
    if let Err(interrupted) = tidepool_extract_cmd::compiler_host_checkpoint() {
        return preparation_io_failure(interrupted);
    }
    PreparationFailure::Source(error.to_string())
}

fn retained_installer(
    recipe: &InstallerRecipe,
    storage: &crate::SourceEntryStorage,
    wrapper: &str,
    acquisition: OriginalAcquisition,
) -> Result<(CompiledTurn, ToolsetAcquisition), PreparationFailure> {
    use tidepool_toolchain::artifacts::{
        load_selected_production_entry, prepare_frozen_production_entry, FrozenEntrySources,
        ProductionEntrySources,
    };
    use tidepool_toolchain::toolchain::CompilerDeploymentConfiguration;
    let source_error =
        |error: &dyn std::fmt::Display| match tidepool_extract_cmd::compiler_host_checkpoint() {
            Err(interrupted) => preparation_io_failure(interrupted),
            Ok(()) => PreparationFailure::Source(error.to_string()),
        };
    tidepool_extract_cmd::compiler_host_checkpoint().map_err(preparation_io_failure)?;
    let key = durable_recipe_key(recipe)?;
    let directory = storage.directory().join(&key);
    let (original, selected) = match storage {
        crate::SourceEntryStorage::FreshCompilation { preparation, .. } => {
            let root = tidepool_atomic_write::DirectoryAnchor::open_existing(storage.directory())
                .map_err(|error| source_error(&error))?;
            let selected = *preparation;
            let original = root
                .child(&key)
                .and_then(|root| root.child(selected.to_string()))
                .map_err(|error| source_error(&error))?;
            (original.path().to_owned(), selected)
        }
        crate::SourceEntryStorage::CompletedOriginal { selections, .. } => {
            let selected =
                selections
                    .get(&key)
                    .ok_or_else(|| PreparationFailure::AbsentSelection {
                        recipe: key.clone(),
                    })?;
            let original = directory.join(selected.to_string());
            (original, *selected)
        }
    };
    let canonical_root =
        std::fs::canonicalize(storage.directory()).map_err(|error| source_error(&error))?;
    if std::fs::canonicalize(&original).map_err(|error| source_error(&error))?
        != canonical_root.join(&key).join(selected.to_string())
    {
        return Err(PreparationFailure::Source(
            "completed installer escapes its retained source owner".into(),
        ));
    }
    let module = tidepool_toolchain::extract_module_name(wrapper)
        .ok_or_else(|| PreparationFailure::Source("installer wrapper has no module".into()))?;
    let source_path = original.join(format!("{module}.hs"));
    let output = original.join("entry");
    let completed = entry_path_present(&output)?;
    if !completed && matches!(acquisition, OriginalAcquisition::LoadCompleted) {
        return Err(PreparationFailure::Source(
            "selected completed original disappeared before acquisition".into(),
        ));
    }
    if !completed && matches!(storage, crate::SourceEntryStorage::FreshCompilation { .. }) {
        match std::fs::symlink_metadata(&source_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if !FrozenEntrySources::source_file_matches(&source_path, wrapper.as_bytes())
                    .map_err(preparation_auth_failure)?
                {
                    return Err(PreparationFailure::Source(
                        "retained installer wrapper changed before retry".into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tidepool_atomic_write::write_durable(&source_path, wrapper.as_bytes())
                    .map_err(|error| source_error(&error))?;
            }
            Ok(_) => {
                return Err(PreparationFailure::Source(
                    "retained installer wrapper is an alias or special file".into(),
                ))
            }
            Err(error) => return Err(source_error(&error)),
        }
    } else {
        let metadata =
            std::fs::symlink_metadata(&source_path).map_err(|error| source_error(&error))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || !FrozenEntrySources::source_file_matches(&source_path, wrapper.as_bytes())
                .map_err(preparation_auth_failure)?
        {
            return Err(PreparationFailure::Source(
                "completed installer wrapper differs from selected source and row".into(),
            ));
        }
    }
    let sources = FrozenEntrySources::capture(&recipe.roots, &source_path)
        .map_err(preparation_auth_failure)?;
    if sources
        .source_revision(b"exomonad-agent-spec-ordered-source-closure-v1")
        .map_err(|error| source_error(&error))?
        != recipe.source_revision
    {
        return Err(PreparationFailure::Source(
            "source owner snapshot changed before installer acquisition".into(),
        ));
    }
    if !completed && matches!(storage, crate::SourceEntryStorage::FreshCompilation { .. }) {
        prepare_frozen_production_entry(&sources, &original, &output).map_err(|error| {
            PreparationFailure::Compiler(tidepool_runtime::classify_compile(&error))
        })?;
    }
    let CompilerDeploymentConfiguration::Configured(authority) =
        CompilerDeploymentConfiguration::from_env().map_err(|error| source_error(&error))?
    else {
        return Err(PreparationFailure::Source(
            "completed installer requires configured compiler deployment".into(),
        ));
    };
    let loaded = load_selected_production_entry(
        &output,
        &authority,
        &ProductionEntrySources::FrozenWorkspace(sources),
    )
    .map_err(preparation_auth_failure)?;
    // A previous rename may have succeeded before parent fsync failed. This
    // confirms publication of that exact validated original without source.
    tidepool_atomic_write::sync_parent_directory(&output).map_err(|error| source_error(&error))?;
    let compiled =
        CompiledTurn::from_production_entry(&loaded).map_err(|error| source_error(&error))?;
    let acquisition = match storage {
        crate::SourceEntryStorage::CompletedOriginal { .. } => {
            ToolsetAcquisition::DeploymentOriginal {
                recipe: key,
                original: selected,
            }
        }
        crate::SourceEntryStorage::FreshCompilation { .. } if completed => {
            ToolsetAcquisition::ExistingRunOriginal {
                recipe: key,
                original: selected,
            }
        }
        crate::SourceEntryStorage::FreshCompilation { .. } => {
            ToolsetAcquisition::FreshRunOriginal {
                recipe: key,
                original: selected,
            }
        }
    };
    Ok((compiled, acquisition))
}
