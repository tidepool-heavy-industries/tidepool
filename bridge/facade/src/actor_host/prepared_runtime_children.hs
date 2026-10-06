import Control.Monad (forM)
import qualified Data.Text as Text
preparedReplies <- forM [1..20 :: Int] $ \ordinal -> do
  let name = "prepared-child-" <> Text.pack (show ordinal)
  worker <- startAgent (withAgentLifetime ActorOwned (readonlyAgent name))
  answer <- request @Int worker (assignment [label|prepared-native-probe|] ("execute the prepared probe" :: Text.Text))
  Right value <- waitFor (awaitValue answer)
  pure value
display preparedReplies
