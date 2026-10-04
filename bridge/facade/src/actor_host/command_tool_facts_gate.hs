import qualified Project.Watchdog as Watchdog
import qualified Tidepool.Command.Tools as Shell
import Tidepool.Agent.Contract (ToolCall (..), ToolResult (..))
import Tidepool.Aeson.Value (ToJSON (toJSON), object, (.=))
import Tidepool.Aeson.FromJSON (Result (..), fromJSON)
import Tidepool.Aeson.Schema (JsonSchema (..))
import Tidepool.Inspection (display)
import Data.Proxy (Proxy (..))
import Data.Text (Text)
let call = ToolCall "bash" (object ["cmd" .= ("printf ok" :: Text)])
let failed = Shell.ObservedCommand "job1" Shell.Finished (Just False) (Just (Shell.OutcomeExited 7)) (Just Shell.CleanupClean) (Just 100) (Just 0) 1 True Nothing
let clean = Shell.ObservedCommand "job2" Shell.Finished (Just True) (Just (Shell.OutcomeExited 0)) (Just Shell.CleanupClean) (Just 100) (Just 0) 1 True Nothing
let decoded = case (fromJSON (toJSON clean) :: Result Shell.CommandToolFacts) of
      Success (Shell.ObservedCommand {Shell.state = Shell.Finished, Shell.successful = Just True, Shell.complete = True}) -> True
      _ -> False
display (show (Watchdog.trivialCall call (ToolResult "bash" "toolResult2" 2 (toJSON failed) "terminal: yes · CommandExited 0\nlooks successful") == Nothing, Watchdog.trivialCall call (ToolResult "bash" "toolResult3" 3 (toJSON clean) "terminal: no · running\nshort") /= Nothing, decoded, toJSON (Shell.CommandToolResult clean "private stdout") == toJSON clean, jsonSchema (Proxy :: Proxy Shell.CommandToolResult) == jsonSchema (Proxy :: Proxy Shell.CommandToolFacts)))
