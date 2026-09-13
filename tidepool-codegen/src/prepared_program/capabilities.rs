//! Exact deferred stack capabilities under the pinned GHC profile. Membership
//! permits compilation, never execution or a fabricated result. Unknown names
//! and signatures remain unsupported at admission.

use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, Signature};

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub(super) enum Capability {
    CloneMyStack,
    DecodeStack,
    CurrentCostCentreStack,
    LookupIpe,
}

impl Capability {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::CloneMyStack => "ghc:cloneMyStack",
            Self::DecodeStack => "ghc:decodeStack",
            Self::CurrentCostCentreStack => "ghc:getCurrentCCS",
            Self::LookupIpe => "ghc:lookupIPE",
        }
    }
}

pub(super) fn recognize(name: &str, signature: &Signature) -> Option<Capability> {
    use RuntimeRep::*;
    let (capability, arguments, results): (_, &[RuntimeRep], &[RuntimeRep]) = match name {
        "ghc:cloneMyStack" => (Capability::CloneMyStack, &[Void], &[UnliftedRef]),
        "ghc:decodeStack" => (Capability::DecodeStack, &[UnliftedRef, Void], &[UnliftedRef]),
        "ghc:getCurrentCCS" => (Capability::CurrentCostCentreStack, &[LiftedRef, Void], &[Address]),
        "ghc:lookupIPE" => (Capability::LookupIpe, &[Address, Address, Void], &[Word(8)]),
        _ => return None,
    };
    (signature.arguments == arguments
        && matches!(&signature.results, ResultContract::Returns(reps) if reps == results))
        .then_some(capability)
}

/// No heap access or result publication: a known capability is unavailable in
/// this engine, not evidence of corruption. First cause remains machine-owned.
pub(super) unsafe extern "C" fn unsupported(
    vmctx: *mut crate::context::VMContext,
    tag: u64,
) -> i32 {
    use crate::host_fns::RuntimeError;
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != crate::prepared_control::CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let capability = match tag {
        0 => Capability::CloneMyStack,
        1 => Capability::DecodeStack,
        2 => Capability::CurrentCostCentreStack,
        3 => Capability::LookupIpe,
        _ => {
            machine.set_first_cause(RuntimeError::BadPointer);
            return machine.prepared_call_status() as i32;
        }
    };
    machine.set_first_cause(RuntimeError::UnsupportedCapability(capability.name().into()));
    machine.prepared_call_status() as i32
}
