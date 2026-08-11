//! Re-export of the per-turn timing module, which now lives in
//! `tidepool-runtime` — this crate already depends on that one, so the
//! module sits there and is re-exported here rather than each crate
//! hand-mirroring its event shape (tracing target `tidepool_harness::timing`,
//! `record_stage`'s field set, the `STAGE_*`/`PHASE_*` vocabulary) by hand
//! across the boundary. `tidepool-runtime` cannot depend the other way (this
//! crate depends on it), so this is the only direction that avoids a cycle.
//!
//! See [`tidepool_runtime::timing`] for the full format contract.

pub use tidepool_runtime::timing::*;
