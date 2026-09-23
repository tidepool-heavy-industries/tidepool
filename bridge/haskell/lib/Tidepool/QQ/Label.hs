{-# LANGUAGE TemplateHaskellQuotes #-}

-- | Compile-time checked assignment label literals.
module Tidepool.QQ.Label (label) where

import Language.Haskell.TH (litE, stringL)
import Language.Haskell.TH.Quote (QuasiQuoter)
import qualified Tidepool.Data.Text as T

import Tidepool.Agent.Assignment.Internal (Label (..), labelFromText)
import Tidepool.QQ.Validate (mkValidatorQQWith)

-- | @[label|orbit-motif|]@ uses the same policy as 'labelFromText'.
label :: QuasiQuoter
label = mkValidatorQQWith "[label|…|]" check $ \source ->
  [| Label (T.pack $(litE (stringL source))) |]
  where
    check value = case labelFromText value of
      Right _ -> Right ()
      Left reason -> Left (T.pack (show reason))
