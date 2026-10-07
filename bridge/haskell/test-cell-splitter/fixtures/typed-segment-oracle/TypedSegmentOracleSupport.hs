{-# LANGUAGE DataKinds #-}
module TypedSegmentOracleSupport (record, check) where

import Control.Exception (ErrorCall(ErrorCall), PatternMatchFail(..), SomeException, evaluate, fromException, throwIO, try)
import Control.Monad.Freer (Eff, runM, send)
import Data.IORef (IORef, newIORef, modifyIORef', readIORef, writeIORef)
import System.IO.Unsafe (unsafePerformIO)

-- Shared only by the serial differential leaf. The trace belongs to the
-- oracle's IO interpreter, independently of compiler capture descriptors.
trace :: IORef [Int]
trace = unsafePerformIO (newIORef [])
{-# NOINLINE trace #-}

record :: Int -> Eff '[IO] ()
record value = send (evaluate value >>= \actual -> modifyIORef' trace (++ [actual]))

check :: Eff '[IO] () -> IO ([Int], Maybe (Either String String))
check action = do
  writeIORef trace []
  outcome <- try (runM action) :: IO (Either SomeException ())
  failure <- case outcome of
    Right () -> pure Nothing
    Left exception
      | Just (ErrorCall message) <- fromException exception -> pure (Just (Left message))
      | Just (PatternMatchFail message) <- fromException exception -> pure (Just (Right message))
      | otherwise -> throwIO exception
  values <- readIORef trace
  pure (values, failure)
