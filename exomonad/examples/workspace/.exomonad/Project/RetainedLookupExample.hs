{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
-- | Capture once, then inspect ordinary immutable values. No navigation action
-- rereads source, invokes lookup, or grants the discovered declaration's effects.
module Project.RetainedLookupExample (capture, sections, firstSignatureLine) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Lookup
import Tidepool.Lookup.Navigation

capture :: Member Lookup effects => Eff effects (LookupBatch, [Either LookupSelectionIssue LookupExcerpt])
capture = do
  captured <- lookupRaw (lookupRequest ["Tidepool.File.readSnapshot"])
  pure (captured, sections captured)

-- Display lookupIndex captured first. Result/entry ordinals refer to that exact
-- batch, including duplicate query names. Missing sections remain explicit Lefts.
sections :: LookupBatch -> [Either LookupSelectionIssue LookupExcerpt]
sections captured = selectLookupMany captured
  [ LookupSelection (LookupAddress 1 1 Signature) Nothing
  , LookupSelection (LookupAddress 1 1 Contract) Nothing
  , LookupSelection (LookupAddress 1 1 Manifest) Nothing
  , LookupSelection (LookupAddress 1 1 Example) Nothing
  , LookupSelection (LookupAddress 1 1 Bundle) Nothing
  ]

-- Inclusive line windows preserve the captured line terminator. Repeating this
-- function revisits the same text; expand only consumes a displayed suffix.
firstSignatureLine :: LookupBatch -> Either LookupSelectionIssue LookupExcerpt
firstSignatureLine captured = selectLookup captured
  (LookupSelection (LookupAddress 1 1 Signature) (Just (LookupLines 1 1)))
