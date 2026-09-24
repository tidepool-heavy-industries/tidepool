{-# LANGUAGE TemplateHaskellQuotes #-}

-- | Compile-time checked assignment label literals.
module Tidepool.QQ.Label (label) where

import Language.Haskell.TH (litE, stringL)
import Language.Haskell.TH.Quote (QuasiQuoter)
import qualified Tidepool.Data.Text as T

import Tidepool.Agent.Assignment.Internal (IsWatchLabel (..), labelFromText)
import Tidepool.QQ.Validate (mkValidatorQQWith)

-- | @[label|orbit-motif|]@ uses the same policy as 'labelFromText'. The
-- quoted value is 'IsWatchLabel'-polymorphic, not committed to 'Label': at a
-- plain assignment site it resolves as a 'Label', and at 'watch' or
-- 'spawnWatched' it resolves as a 'Tidepool.Agent.Watch.Internal.WatchLabel',
-- so one literal works at either.
label :: QuasiQuoter
label = mkValidatorQQWith "[label|…|]" check $ \source ->
  [| fromValidatedLabelText (T.pack $(litE (stringL source))) |]
  where
    check value = case labelFromText value of
      Right _ -> Right ()
      Left reason -> Left (T.pack (show reason))
