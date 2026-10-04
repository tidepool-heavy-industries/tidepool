{-# LANGUAGE TypeFamilies #-}
module MetadataQuoteSupport (answerValue) where

type family SupportResult a where
  SupportResult Bool = Integer

answerValue :: SupportResult Bool
answerValue = 42
