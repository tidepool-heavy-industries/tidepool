import Control.Monad (forM)
import qualified Data.Text as Text
preparedReplies <- forM [1..{prepared-child-count} :: Int] $ \ordinal -> do
  let name = "prepared-child-" <> Text.pack (show ordinal)
  Right childLabel <- pure (labelFromText name)
  Right groupLabel <- pure (forkGroupLabel ("probe-" <> Text.pack (show ordinal)))
  answer <- unfold (batch ("prepared-runtime" :: CampaignLabel) groupLabel)
    (child (withLifetime ActorOwned (withContext (selected id)
      (researching @Int projectHead
        (assignment childLabel ("execute the prepared probe" :: Text.Text))))))
  Right value <- waitFor (awaitValue answer)
  pure value
display preparedReplies
