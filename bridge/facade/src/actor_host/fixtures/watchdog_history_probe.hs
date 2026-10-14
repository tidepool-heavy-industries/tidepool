import Control.Monad (replicateM_)
import qualified Project.Watchdog as Watchdog
import Tidepool.Agent.Contract (ToolCall (..), ToolResult (..))
import Tidepool.Aeson.Value (Value (Null))

replicateM_ 7 $ do
  _ <- Watchdog.watchWith Watchdog.coreHeuristics
    (ToolCall "probe" Null)
    (ToolResult "probe" "toolResult1" 1 Null "current result")
  pure ()
