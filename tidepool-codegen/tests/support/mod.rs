#![allow(dead_code)]

use tidepool_codegen::jit_machine::{FuncId, JitEffectMachine, JitError};
use tidepool_codegen::old_space::RootSlot;
use tidepool_codegen::suspension::{
    ContinuationId, ParkKind, ParkedOutcome, RealmId, ResumeInput, Suspendable, SuspendableOutcome,
    SuspensionRun,
};
use tidepool_effect::{DispatchEffect, EffectBoundary};
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

/// Concise adapters for integration tests that exercise many suspension
/// layouts. Production callers use `SuspensionRun` directly.
#[allow(dead_code)]
pub trait SuspensionTestExt {
    fn run_suspendable_parked<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        realm: RealmId,
        effect_names: &[String],
    ) -> Result<ParkedOutcome, JitError>;

    #[allow(clippy::too_many_arguments)]
    fn run_fragment_suspendable_parked<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        realm: RealmId,
        completion: ParkKind,
        effect_names: &[String],
    ) -> Result<ParkedOutcome, JitError>;
}

impl SuspensionTestExt for JitEffectMachine {
    fn run_suspendable_parked<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        realm: RealmId,
        effect_names: &[String],
    ) -> Result<ParkedOutcome, JitError> {
        let boundary = EffectBoundary::new(suspend_tag, effect_names);
        self.run_until_suspension(SuspensionRun::main(table, &boundary, realm), handlers, user)
    }

    fn run_fragment_suspendable_parked<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        realm: RealmId,
        completion: ParkKind,
        effect_names: &[String],
    ) -> Result<ParkedOutcome, JitError> {
        let boundary = EffectBoundary::new(suspend_tag, effect_names);
        let run = SuspensionRun::fragment(func_id, table, &boundary, realm, completion);
        self.run_until_suspension(run, handlers, user)
    }
}

/// Test-only adapter for cases whose subject is child execution or
/// materialization rather than continuation identity. It models a
/// capacity-one caller on top of the production registry API; the machine no
/// longer carries this policy itself.
pub struct LinearMachine {
    machine: JitEffectMachine,
    active: Option<ContinuationId>,
}

impl LinearMachine {
    pub fn new(machine: JitEffectMachine) -> Self {
        Self {
            machine,
            active: None,
        }
    }

    pub fn is_suspended(&self) -> bool {
        self.active
            .is_some_and(|id| self.machine.parked_ids().contains(&id))
    }

    pub fn run_suspendable<U, H: DispatchEffect<U>>(
        &mut self,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
    ) -> Result<SuspendableOutcome, JitError> {
        self.assert_idle();
        let boundary = EffectBoundary::new(suspend_tag, &[]);
        let run = SuspensionRun::main(table, &boundary, RealmId(0));
        let outcome = self.machine.run_until_suspension(run, handlers, user)?;
        Ok(self.track_value(outcome))
    }

    pub fn run_fragment_suspendable_projected<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        n_fields: usize,
    ) -> Result<Suspendable<Vec<RootSlot>>, JitError> {
        self.assert_idle();
        let n_fields = std::num::NonZeroUsize::new(n_fields).expect("projected fields");
        let boundary = EffectBoundary::new(suspend_tag, &[]);
        let run = SuspensionRun::fragment(
            func_id,
            table,
            &boundary,
            RealmId(0),
            ParkKind::Project { n_fields },
        );
        let outcome = self.machine.run_until_suspension(run, handlers, user)?;
        Ok(self.track_project(outcome))
    }

    pub fn run_fragment_suspendable_render<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
        suspend_tag: u64,
        field0_forced: bool,
    ) -> Result<Suspendable<(RootSlot, Value)>, JitError> {
        self.assert_idle();
        let boundary = EffectBoundary::new(suspend_tag, &[]);
        let run = SuspensionRun::fragment(
            func_id,
            table,
            &boundary,
            RealmId(0),
            ParkKind::Render { field0_forced },
        );
        let outcome = self.machine.run_until_suspension(run, handlers, user)?;
        Ok(self.track_render(outcome))
    }

    pub fn resume_suspended<U, H: DispatchEffect<U>>(
        &mut self,
        _table: &DataConTable,
        handlers: &mut H,
        user: &U,
        _suspend_tag: u64,
        input: ResumeInput,
    ) -> Result<SuspendableOutcome, JitError> {
        let outcome = self.resume(handlers, user, input)?;
        Ok(self.track_value(outcome))
    }

    pub fn resume_suspended_projected<U, H: DispatchEffect<U>>(
        &mut self,
        _table: &DataConTable,
        handlers: &mut H,
        user: &U,
        _suspend_tag: u64,
        input: ResumeInput,
        _n_fields: usize,
    ) -> Result<Suspendable<Vec<RootSlot>>, JitError> {
        let outcome = self.resume(handlers, user, input)?;
        Ok(self.track_project(outcome))
    }

    pub fn resume_suspended_render<U, H: DispatchEffect<U>>(
        &mut self,
        _table: &DataConTable,
        handlers: &mut H,
        user: &U,
        _suspend_tag: u64,
        input: ResumeInput,
        _field0_forced: bool,
    ) -> Result<Suspendable<(RootSlot, Value)>, JitError> {
        let outcome = self.resume(handlers, user, input)?;
        Ok(self.track_render(outcome))
    }

    pub fn run_child_fragment<U, H: DispatchEffect<U>>(
        &mut self,
        func_id: FuncId,
        table: &DataConTable,
        handlers: &mut H,
        user: &U,
    ) -> Result<Value, JitError> {
        self.machine.run_fragment(func_id, table, handlers, user)
    }

    pub fn run_child_fragment_pure(&mut self, func_id: FuncId) -> Result<Value, JitError> {
        self.machine.run_fragment_pure(func_id)
    }

    fn assert_idle(&self) {
        assert!(
            !self.is_suspended(),
            "capacity-one test adapter already has an active continuation"
        );
    }

    fn resume<U, H: DispatchEffect<U>>(
        &mut self,
        handlers: &mut H,
        user: &U,
        input: ResumeInput,
    ) -> Result<ParkedOutcome, JitError> {
        let id = self.active.expect("active test continuation");
        self.machine.resume_continuation(id, handlers, user, input)
    }

    fn track_value(&mut self, outcome: ParkedOutcome) -> SuspendableOutcome {
        match outcome {
            ParkedOutcome::CompletedValue(value) => {
                self.active = None;
                Suspendable::Completed(value)
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_finalized_closure,
            } => {
                self.active = Some(id);
                Suspendable::Suspended {
                    request,
                    has_finalized_closure,
                }
            }
            other => panic!("expected value completion, got {other:?}"),
        }
    }

    fn track_project(&mut self, outcome: ParkedOutcome) -> Suspendable<Vec<RootSlot>> {
        match outcome {
            ParkedOutcome::CompletedProject { roots } => {
                self.active = None;
                Suspendable::Completed(roots)
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_finalized_closure,
            } => {
                self.active = Some(id);
                Suspendable::Suspended {
                    request,
                    has_finalized_closure,
                }
            }
            other => panic!("expected projected completion, got {other:?}"),
        }
    }

    fn track_render(&mut self, outcome: ParkedOutcome) -> Suspendable<(RootSlot, Value)> {
        match outcome {
            ParkedOutcome::CompletedRender { root, rendered } => {
                self.active = None;
                Suspendable::Completed((root, rendered))
            }
            ParkedOutcome::Suspended {
                id,
                request,
                has_finalized_closure,
            } => {
                self.active = Some(id);
                Suspendable::Suspended {
                    request,
                    has_finalized_closure,
                }
            }
            other => panic!("expected render completion, got {other:?}"),
        }
    }
}

impl std::ops::Deref for LinearMachine {
    type Target = JitEffectMachine;

    fn deref(&self) -> &Self::Target {
        &self.machine
    }
}

impl std::ops::DerefMut for LinearMachine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.machine
    }
}
