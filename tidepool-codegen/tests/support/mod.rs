use tidepool_codegen::jit_machine::{
    FuncId, JitEffectMachine, JitError, ParkKind, ParkedOutcome, RealmId, SuspensionRun,
};
use tidepool_effect::{DispatchEffect, EffectBoundary};
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
