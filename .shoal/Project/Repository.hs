{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Choose and execute a read-only investigation against Git or retained docs.
module Project.Repository (answer) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=)))
import qualified Project.History as History
import qualified Project.Search as Search
import Tidepool.Effects.Core (Commands, Jev)

answer :: (Member Jev effects, Member Commands effects)
  => Either Text [Search.Passage] -> Text -> Eff effects Text
answer docs query = do
  decision <- J.ask1 (J.state (#query := query))
    (J.choice "Which available investigation fits the query?"
      (J.alt #history "The query asks which commit introduced, fixed, or changed something"
          (History.inspectHistory query)
        J..| J.alt #documentation "The query asks what the project documentation says or instructs"
          (Search.render <$> Search.grep query docs)
        J..| J.alt #unresolved "Neither commit history nor documentation can address the query"
          (pure "This needs evidence beyond Git history and documentation.")))
  case decision of
    Left err -> pure ("Jev unavailable: " <> T.pack (show err))
    Right chosen -> case J.takenUnder J.lenient chosen of
      Left doubt -> pure ("Which investigation? " <> doubt.why)
      Right (J.Settled action) -> do
        result <- action
        pure ("[" <> chosen.key <> "]\n" <> result)
