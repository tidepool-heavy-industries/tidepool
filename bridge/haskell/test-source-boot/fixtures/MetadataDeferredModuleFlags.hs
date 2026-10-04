{-# LANGUAGE TypeFamilies #-}
module MetadataDeferredModuleFlags (answer) where

import MetadataQuoteSupport (answerValue)

type family DeferredResult a where
  DeferredResult Bool = Int

answer :: DeferredResult Bool
answer = fromInteger answerValue
