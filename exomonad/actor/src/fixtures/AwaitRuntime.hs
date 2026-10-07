{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
module AwaitRuntime (nestedNamed, pollNamed) where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Watch

nestedNamed :: Eff '[Watches] Bool
nestedNamed = do
  source <- watch (Just "source") (eitherOf (pure (11 :: Int)) (pure (12 :: Int)))
  nested <- watch (Just "nested") (eitherOf (observed source) (pure (99 :: Int)))
  outcome <- await ((,) <$> observed nested <*> pure (33 :: Int))
  case outcome of
    Right (Left (Left 11), 33) -> pure True
    _ -> error "named watch projection changed the original nested choices"

pollNamed :: Eff '[Watches] Bool
pollNamed = do
  source <- watch (Just "inspected") (pure (11 :: Int))
  outcome <- pollWatch source
  case outcome of
    WatchReady 11 -> pure True
    _ -> error "forgetting a public watch revoked its admitted inspection"
