//! Machine-wide resolution of a foreign program's callable/enter code,
//! backing the cross-program call and force fallback (a later wave adds
//! the dispatcher-side call sites; this module is the substrate).

use cranelift_module::FuncId;
use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, Signature};

/// One function this program exports as a call target for another
/// installed program: its descriptor's header word identifies the object
/// at runtime, `function` is the compiled callee, `fingerprint` guards
/// against a signature mismatch (see `signature_fingerprint`).
#[allow(
    dead_code,
    reason = "consumed by the install-time registration a later wave adds"
)]
pub(crate) struct CallableExport {
    pub header: usize,
    pub function: FuncId,
    pub fingerprint: u64,
}

/// What `PreparedMachine`'s resolution table stores per exported header:
/// the code pointer (resolved at install from `function` above via
/// `pipeline.get_function_ptr`) and the fingerprint to check against the
/// caller's own expectation before jumping to it.
#[derive(Clone, Copy)]
pub(crate) struct ResolvedEntry {
    pub code: *const u8,
    pub fingerprint: u64,
}

/// FNV-1a: a tiny, fixed-seed, non-cryptographic hash with no per-process
/// randomization. Deliberately NOT `std::collections::hash_map::DefaultHasher`
/// -- its seed comes from `RandomState`, which is randomized per-process by
/// design, so two processes (or two runs) hashing the "same" signature could
/// disagree. A fingerprint that must compare equal across independently
/// compiled programs cannot rely on that.
struct Fnv1a(u64);

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Fnv1a {
    fn new() -> Self {
        Self(FNV_OFFSET_BASIS)
    }

    fn write_u8(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u8(byte);
        }
    }

    fn write_tag(&mut self, tag: u8) {
        self.write_u8(tag);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn hash_runtime_rep(hasher: &mut Fnv1a, rep: &RuntimeRep) {
    match rep {
        RuntimeRep::Void => hasher.write_tag(0),
        RuntimeRep::LiftedRef => hasher.write_tag(1),
        RuntimeRep::UnliftedRef => hasher.write_tag(2),
        RuntimeRep::Address => hasher.write_tag(3),
        RuntimeRep::Int(width) => {
            hasher.write_tag(4);
            hasher.write_u8(*width);
        }
        RuntimeRep::Word(width) => {
            hasher.write_tag(5);
            hasher.write_u8(*width);
        }
        RuntimeRep::Float(width) => {
            hasher.write_tag(6);
            hasher.write_u8(*width);
        }
    }
}

fn hash_runtime_reps(hasher: &mut Fnv1a, reps: &[RuntimeRep]) {
    hasher.write_bytes(&(reps.len() as u64).to_le_bytes());
    for rep in reps {
        hash_runtime_rep(hasher, rep);
    }
}

fn hash_result_contract(hasher: &mut Fnv1a, results: &ResultContract) {
    match results {
        ResultContract::Returns(reps) => {
            hasher.write_tag(0);
            hash_runtime_reps(hasher, reps);
        }
        ResultContract::NoSuccess => hasher.write_tag(1),
    }
}

/// A stable, deterministic (same-process, same build) hash of a call
/// signature's shape: argument representations in order, plus the result
/// contract. Two programs declaring the "same" function must agree on
/// this value; a real ABI mismatch must not collide by chance with
/// anything realistic, so this folds every argument rep and the full
/// result shape, not just arity.
pub(crate) fn signature_fingerprint(signature: &Signature) -> u64 {
    let mut hasher = Fnv1a::new();
    hash_runtime_reps(&mut hasher, &signature.arguments);
    hash_result_contract(&mut hasher, &signature.results);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::signature_fingerprint;
    use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, Signature};

    #[test]
    fn differing_result_contract_does_not_collide() {
        let a = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        let b = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::NoSuccess,
        };
        assert_ne!(signature_fingerprint(&a), signature_fingerprint(&b));
    }

    #[test]
    fn structurally_identical_signatures_agree() {
        let a = Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Word(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(32), RuntimeRep::Address]),
        };
        let b = Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Word(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(32), RuntimeRep::Address]),
        };
        assert_eq!(signature_fingerprint(&a), signature_fingerprint(&b));
    }

    #[test]
    fn differing_argument_reps_do_not_collide() {
        let a = Signature {
            arguments: vec![RuntimeRep::Int(32)],
            results: ResultContract::NoSuccess,
        };
        let b = Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: ResultContract::NoSuccess,
        };
        assert_ne!(signature_fingerprint(&a), signature_fingerprint(&b));
    }
}
