{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE MagicHash #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UnboxedSums #-}
{-# LANGUAGE UnboxedTuples #-}

module PreparedStgProbe where

import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Exts
import ProbeDependency (libraryAndLocalDependency)
import RetainedValueShape (RetainedValue(..), importedRetainedValue)
import Tidepool.Effects.Core (runLLMTurn)

-- Strict lifted and unpacked scalar fields must not acquire the same layout.
data StrictAndUnpacked = StrictAndUnpacked
  {-# UNPACK #-} !Int
  !Text

strictAndUnpackedFields :: StrictAndUnpacked -> Int
strictAndUnpackedFields (StrictAndUnpacked machineInt strictText) =
  machineInt + Text.length strictText

multiArgument :: Int -> Int -> Int -> Int
multiArgument x y z = x + y * z

partialCall :: Int -> Int -> Int
partialCall = multiArgument 7

saturatedCall :: Int
saturatedCall = multiArgument 2 3 4

-- The tail-recursive local worker gives the STG pipeline a real join candidate.
recursiveJoin :: Int -> Int
recursiveJoin limit = go limit 0
  where
    go !remaining !total
      | remaining <= 0 = total
      | otherwise = go (remaining - 1) (total + remaining)

unboxedTuple :: Int# -> Int# -> (# Int#, Int# #)
unboxedTuple x y = (# x +# y, x *# y #)

unboxedSum :: Int# -> (# Int# | Int# #)
unboxedSum x = case x <# 0# of
  0# -> (# | x #)
  _ -> (# negateInt# x | #)

-- An unboxed-unit argument contributes semantic arity but no runtime payload.
voidArgument :: (# #) -> Int# -> Int#
voidArgument _ value = value +# 1#

embeddedNulString :: String
embeddedNulString = "left\0right"

characterLiteral :: Char
characterLiteral = '\x10ffff'

textValue :: Text
textValue = Text.pack "prepared STG"

integerValue :: Integer
integerValue = 1234567890123456789012345678901234567890

-- The fixture module has the production module/occurrence identity, including
-- the generated sibling.  Keeping the call concrete also verifies that the
-- prepared sidecar records the answer before types are erased.
typedTidepoolEffectSite :: Maybe Bool
typedTidepoolEffectSite = runLLMTurn @Bool "prepared"

localAndLibraryDependency :: Int
localAndLibraryDependency = libraryAndLocalDependency [1, 2, 3, 4]

importedRetainedValueCall :: Int
importedRetainedValueCall =
  retainedApply importedRetainedValue
    (libraryAndLocalDependency (retainedEnvironment importedRetainedValue))
