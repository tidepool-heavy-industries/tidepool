module ArrayInitialization where

import Data.Array (Array, (!), array, listArray)
import Data.Array.ST (newArray, runSTArray, writeArray)
import Control.Exception (ErrorCall, evaluate, try)
import Numeric (floatToDigits)

{-# NOINLINE singleton #-}
singleton :: Int -> Array Int Int
singleton x = listArray (0,0) [x]

{-# NOINLINE listed #-}
listed :: Int -> Array Int Int
listed x = listArray (0,2) [x, x+1, x+2]

{-# NOINLINE associated #-}
associated :: Int -> Array Int Int
associated x = array (0,2) [(2,x+2),(0,x),(1,x+1)]

{-# NOINLINE initialized #-}
initialized :: Int -> Array Int Int
initialized x = runSTArray (newArray (0,2) x)

{-# NOINLINE written #-}
written :: Int -> Array Int Int
written x = runSTArray $ do
  a <- newArray (0,2) (error "untouched array initializer")
  writeArray a 1 x
  pure a

{-# NOINLINE looped #-}
looped :: Int -> Array Int Int
looped x = runSTArray $ do
  a <- newArray (0,2) (error "unwritten loop cell")
  mapM_ (\i -> writeArray a i (x+i)) [0..2]
  pure a

singleResult, listResult, associationResult, initialResult, writeResult, loopResult :: String
singleResult = show (singleton 7 ! 0)
listResult = show [listed 7 ! i | i <- [0..2]]
associationResult = show [associated 7 ! i | i <- [0..2]]
initialResult = show [initialized 7 ! i | i <- [0..2]]
writeResult = show (written 7 ! 1)
loopResult = show [looped 7 ! i | i <- [0..2]]

{-# NOINLINE digits #-}
digits :: Double -> ([Int],Int)
digits = floatToDigits 10

digitsResult, finiteShowResult :: String
digitsResult = show (digits 1)
finiteShowResult = show (1 :: Double)

{-# NOINLINE writtenDefined #-}
writtenDefined :: Int -> Array Int Int
writtenDefined x = runSTArray $ do
  a <- newArray (0,2) 99
  writeArray a 1 x
  pure a

writeDefinedResult :: String
writeDefinedResult = show (writtenDefined 7 ! 1)

{-# NOINLINE delayedBottom #-}
delayedBottom :: Int -> Int
delayedBottom n = error ("written bottom " ++ show n)

{-# NOINLINE withWrittenBottom #-}
withWrittenBottom :: Int -> Array Int Int
withWrittenBottom n = runSTArray $ do
  a <- newArray (0,2) 99
  writeArray a 1 (delayedBottom n)
  pure a

unusedWrittenBottomResult, selectedWrittenBottomResult, selectedInitializerResult :: String
unusedWrittenBottomResult = show (withWrittenBottom 7 ! 0)
selectedWrittenBottomResult = show (withWrittenBottom 7 ! 1)
selectedInitializerResult = show (written 7 ! 0)

-- Native errors are caught as language exceptions, never inferred from exit status.
nativeFailure :: String -> IO ()
nativeFailure value = do
  outcome <- try (evaluate (length value)) :: IO (Either ErrorCall Int)
  putStr (case outcome of Left _ -> "error-call"; Right _ -> "success")
