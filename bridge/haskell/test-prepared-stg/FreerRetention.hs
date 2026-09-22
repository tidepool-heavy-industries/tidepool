{-# LANGUAGE DataKinds #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE TypeOperators #-}

module FreerRetention where

import Control.Monad.Freer (Eff, send)

data RetainRequest result where
  RetainRequest :: Int -> RetainRequest Int

freerRequest :: Eff '[RetainRequest] Int
freerRequest = send (RetainRequest 7)
