//! Shared setup for the effect_stack/ suite.
use tidepool_testing::eval_harness::mock;

/// [`mock::MCP_PREAMBLE`] plus the extra qualified imports and `import
/// Library` line the real MCP server's generated module carries when a
/// verb-library eval pulls in `Library` — for tests that reproduce the exact
/// module shape `tidepool-extract` emits for a `.tidepool/lib`-backed eval.
pub fn mcp_module_with_library(body: &str) -> String {
    let preamble = mock::MCP_PREAMBLE.replacen(
        "import Control.Monad.Freer hiding (run)\n",
        "import qualified Data.Map.Strict as Map\n\
         import qualified Data.Set as Set\n\
         import qualified Data.List as L\n\
         import qualified Tidepool.TextFormat as TF\n\
         import qualified Tidepool.Table as Tab\n\
         import Control.Monad.Freer hiding (run)\n\
         import Library\n",
        1,
    );
    format!("{preamble}\n{body}\n")
}
