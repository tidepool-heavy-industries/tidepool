-- | The serialized core language consumed by the Rust evaluator.
--
-- Nodes refer to earlier nodes by sequence index; the final node is the root.
-- Variable and constructor identities are stable 64-bit wire ids.
module Tidepool.IR
  ( FlatNode(..)
  , FlatAlt(..)
  , FlatAltCon(..)
  , LitEnc(..)
  ) where

import Data.ByteString (ByteString)
import Data.Int (Int64)
import Data.Text (Text)
import Data.Word (Word32, Word64)

data FlatNode
  = NVar !Word64
  | NLit !LitEnc
  | NApp !Int !Int
  | NLam !Word64 !Int
  | NLetNonRec !Word64 !Int !Int
  | NLetRec ![(Word64, Int)] !Int
  | NCase !Int !Word64 ![FlatAlt]
  | NCon !Word64 ![Int]
  | NJoin !Word64 ![Word64] !Int !Int
  | NJump !Word64 ![Int]
  | NPrimOp !Text ![Int]
  deriving (Eq, Show)

data FlatAlt = FlatAlt !FlatAltCon ![Word64] !Int
  deriving (Eq, Show)

data FlatAltCon = FDataAlt !Word64 | FLitAlt !LitEnc | FDefault
  deriving (Eq, Show)

data LitEnc
  = LEInt !Int64
  | LEWord !Word64
  | LEChar !Word32
  | LEString !ByteString
  | LEByteArray !ByteString
  | LEFloat !Word64
  | LEDouble !Word64
  deriving (Eq, Show)
