{-# LANGUAGE TypeFamilies #-}
module MetadataQuoteSupport (answerValue) where

import MetadataRetainedWitness (RetainedWitness)

type family SupportResult a where
  SupportResult RetainedWitness = Integer

answerValue :: SupportResult RetainedWitness
answerValue = 42
