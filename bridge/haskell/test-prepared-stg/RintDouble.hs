{-# LANGUAGE ForeignFunctionInterface #-}

module RintDouble where

-- An IO foreign call retains the state token even when constant rounding is
-- folded by GHC. This fixture tests the projection ABI, not simplifier policy.
foreign import ccall unsafe "rintDouble"
  rintDoubleState :: Double -> IO Double
