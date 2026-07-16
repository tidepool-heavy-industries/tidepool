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
    read_metadata(SUITE_META).unwrap().0
}
