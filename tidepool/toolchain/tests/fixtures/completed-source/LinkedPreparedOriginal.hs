{-# LANGUAGE NoImplicitPrelude #-}
module LinkedPreparedOriginal where

import Tidepool.Prelude
import QuotedOriginal (value)

__prepared :: Int
__prepared = value + length (sort [41, 2, 3])
