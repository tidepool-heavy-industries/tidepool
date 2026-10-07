import Control.Monad (forM)
import qualified Data.Text as Text
import qualified PreparedRuntimeSpec as PreparedSpec
let preparedChildSpec = PreparedSpec.agentSpec @'[Replies]
preparedReplies <- forM [1..{prepared-child-count} :: Int] $ \ordinal -> do
  let name = "prepared-child-" <> Text.pack (show ordinal)
  Right actor <- spawnSubagent (FreshCtx "Execute the prepared probe") SameDir
    ((defaultSpawnOptions preparedChildSpec) { spawnLabel = Just name })
  Right answer <- request @Int actor ("execute the prepared probe" :: Text.Text) defaultRequestOptions
  Right value <- await (result answer)
  pure value
display (preparedReplies == replicate {prepared-child-count} (41 :: Int))
