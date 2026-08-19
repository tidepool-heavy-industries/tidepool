//! Shared access to the pre-compiled Haskell suite fixtures
//! (`haskell/test/suite_cbor/`), used by both the eval oracle suite and the
//! codegen differential suite.

use tidepool_repr::serial::read::read_metadata;
use tidepool_repr::DataConTable;

/// The suite's shared `meta.cbor` (DataConTable metadata), embedded at build
/// time. The fixtures are checked in, regenerated via the haskell/ harness.
pub static SUITE_META: &[u8] = include_bytes!("../../haskell/test/suite_cbor/meta.cbor");

/// The `DataConTable` every suite fixture was compiled against.
pub fn suite_table() -> DataConTable {
    #[allow(
        clippy::unwrap_used,
        reason = "SUITE_META is a checked-in, build-time-included fixture; a decode failure means the fixture is corrupt"
    )]
    read_metadata(SUITE_META).unwrap().0
}

/// Is this binder name a GHC-lifted local helper?
///
/// GHC lifts `where`/`let` helpers to uniquified top-level binders
/// (`go_u6341068275337658369`). They are inlined into the bindings that use
/// them, so running one standalone tests nothing: it is evaluated outside the
/// call site that gives its arguments — and its termination — meaning. A
/// fixture-replay harness skips them rather than reporting their out-of-context
/// behaviour as a finding.
pub fn is_lifted_local(name: &str) -> bool {
    match name.rfind("_u") {
        Some(i) => {
            let tail = &name[i + 2..];
            tail.len() >= 6 && tail.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_lifted_local;

    #[test]
    fn lifted_local_names_are_recognised() {
        assert!(is_lifted_local("go_u6341068275337658369"));
        assert!(is_lifted_local("xs_u8286623314361937397"));
        // A quote before the suffix is part of the Haskell name, not a barrier.
        assert!(is_lifted_local("xs'_u8286623314361937461"));
        // A real top-level binding, even one containing `_u`.
        assert!(!is_lifted_local("convFromRational"));
        assert!(!is_lifted_local("thunk_blackhole"));
        assert!(!is_lifted_local("read_u12"));
    }
}
