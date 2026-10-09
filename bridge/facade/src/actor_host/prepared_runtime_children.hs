import Control.Monad (forM)
import qualified Data.Text as Text
import qualified PreparedRuntimeSpec as PreparedSpec
let preparedChildSpec = PreparedSpec.agentSpec @'[Replies]
preparedReplies <- forM [1..{prepared-child-count} :: Int] $ \ordinal -> do
  let name = "prepared-child-" <> Text.pack (show ordinal)
  spawned <- spawnSubagent (FreshCtx "Execute the prepared probe") SameDir
    ((defaultSpawnOptions preparedChildSpec) { spawnLabel = Just name })
  actor <- case spawned of
    Left failure -> error ("prepared child spawn failed: " <> show failure)
    Right admitted -> pure admitted
  requested <- request @Int actor ("execute the prepared probe" :: Text.Text) defaultRequestOptions
  answer <- case requested of
    Left failure -> error ("prepared child request failed: " <> show failure)
    Right accepted -> pure accepted
  completed <- await (result answer)
  value <- case completed of
    Left failure -> error ("prepared child reply failed: " <> show failure)
    Right resultValue -> pure resultValue
  pure value
display (preparedReplies == replicate {prepared-child-count} (41 :: Int))
