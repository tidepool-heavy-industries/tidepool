import qualified Tidepool.Agent.Context as C
import qualified Tidepool.Data.Text as ContextText
let retainedHelper value = value + 1 :: Int
retainedValue <- pure (41 :: Int)
display ("context-setup-retained-result" :: Text)
pure True
