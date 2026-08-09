//! `build_gc_forcing_fragment` — standalone, no dependency on
//! `session_scaffold.rs`'s `C1`.

use tidepool_repr::CoreExpr;

/// A heavy allocator that overflows a small session nursery, forcing a real
/// minor GC — same shape as `tidepool_testing::gen::make_gc_forcing_setup`,
/// built directly against `C1` (`DataConId(1)`) rather than that helper's own
/// table.
pub fn build_gc_forcing_fragment(depth: usize) -> CoreExpr {
    tidepool_testing::gen::make_gc_forcing_setup(depth).0
}
