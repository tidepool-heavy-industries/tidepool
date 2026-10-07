{-# LANGUAGE DataKinds #-}
module TypedSegmentOracleSupport (record, check) where

import Control.Exception (ErrorCall, PatternMatchFail, SomeException, fromException, throwIO, try)
import Control.Monad.Freer (Eff, runM, send)
import Data.IORef (IORef, newIORef, modifyIORef', readIORef, writeIORef)
import System.IO.Unsafe (unsafePerformIO)

-- Shared only by the serial differential leaf. The trace belongs to the
-- oracle's IO interpreter, independently of compiler capture descriptors.
trace :: IORef [Int]
trace = unsafePerformIO (newIORef [])
{-# NOINLINE trace #-}

record :: Int -> Eff '[IO] ()
record value = send (modifyIORef' trace (++ [value]))

check :: Eff '[IO] () -> IO ([Int], Bool)
check action = do
  writeIORef trace []
  outcome <- try (runM action) :: IO (Either SomeException ())
  failed <- case outcome of
    Right () -> pure False
    Left exception
      | Just (_ :: ErrorCall) <- fromException exception -> pure True
      | Just (_ :: PatternMatchFail) <- fromException exception -> pure True
      | otherwise -> throwIO exception
  values <- readIORef trace
  pure (values, failed)
