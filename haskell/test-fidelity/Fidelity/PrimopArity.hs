-- | Two fail-loud contracts of the extract pipeline.
--
-- The generic unboxed-tuple fallback can bind at most one result; every higher
-- result arity without a dedicated split fails loud at extract time rather
-- than aliasing several binders onto one node. And a failed module load is a
-- phase barrier — the pipeline stops there instead of continuing into
-- typechecking against an error-recovery environment.
module Fidelity.PrimopArity (checks) where

import Fidelity.Harness (Check, check, extractError)

import System.Directory (createDirectoryIfMissing)

checks :: IO [Check]
checks = sequence [threeResultCheck, twoResultCheck, loadBarrierCheck]

-- | A synthetic 'foreign import prim' with a 3-Int#-plus-state unboxed-tuple
-- return, exercising the stateful arm's @[s', r1, r2, r3]@ case. The extract
-- pipeline only compiles to Core (it never links), so this typechecks with no
-- implementation behind the symbol. 'isPrimOpId_maybe' is 'Nothing' for an
-- FCallId, so this can't land in any of the primop-table-driven multi-return
-- arms above the stateful desugar arm — only 'isFCallId' admits it there —
-- and the raised text names both the arity and (via 'showPprUnsafe' on the
-- FCallId, which renders the ccall spec rather than the Haskell-level name)
-- the @tidepool_test_three_result@ symbol itself, pinning the exact arm.
threeResultCheck :: IO Check
threeResultCheck = do
  (ok, err) <- extractError "primop-arity-3" "ThreeResult" threeResultSrc "go" needle
  pure $ check ("3-result stateful unboxed tuple fails loud: " ++ err) ok
  where
    needle = "Unsupported 3-result stateful unboxed-tuple primop/FFI call: {__ffi_static_ccall_safe main:tidepool_test_three_result"

threeResultSrc :: String
threeResultSrc = unlines
  [ "{-# LANGUAGE MagicHash, UnboxedTuples, GHCForeignImportPrim, UnliftedFFITypes #-}"
  , "module ThreeResult where"
  , ""
  , "import GHC.Exts"
  , ""
  , "foreign import prim \"tidepool_test_three_result\""
  , "  threeResult# :: Int# -> State# RealWorld -> (# State# RealWorld, Int#, Int#, Int# #)"
  , ""
  , "go :: Int"
  , "go = case threeResult# 1# realWorld# of"
  , "  (# _, a, b, c #) -> I# (a +# (b +# c))"
  ]

-- | Companion to 'threeResultCheck': the same trick with one fewer result,
-- pinning that the adjacent 2-result arm ('[_, _, _]') still fails loud with
-- its own policy.
twoResultCheck :: IO Check
twoResultCheck = do
  (ok, err) <- extractError "primop-arity-2" "TwoResult" twoResultSrc "go" needle
  pure $ check ("2-result stateful unboxed tuple fails loud: " ++ err) ok
  where
    needle = "Unsupported 2-result stateful unboxed-tuple primop/FFI call: {__ffi_static_ccall_safe main:tidepool_test_two_result"

twoResultSrc :: String
twoResultSrc = unlines
  [ "{-# LANGUAGE MagicHash, UnboxedTuples, GHCForeignImportPrim, UnliftedFFITypes #-}"
  , "module TwoResult where"
  , ""
  , "import GHC.Exts"
  , ""
  , "foreign import prim \"tidepool_test_two_result\""
  , "  twoResult# :: Int# -> State# RealWorld -> (# State# RealWorld, Int#, Int# #)"
  , ""
  , "go :: Int"
  , "go = case twoResult# 1# realWorld# of"
  , "  (# _, a, b #) -> I# (a +# b)"
  ]

-- | A failed 'load'' is a phase barrier: the target module imports a second
-- home-source module that fails to typecheck, and extraction must stop with
-- the barrier's own message rather than continuing into a confusing
-- downstream failure against a half-populated environment.
-- 'extractBinding'/'extractError' write only the target's own source file, so
-- the broken dependency is written into the same work dir here, before the
-- harness call.
loadBarrierCheck :: IO Check
loadBarrierCheck = do
  let dir = "test-fidelity/work/" ++ tag
  createDirectoryIfMissing True dir
  writeFile (dir ++ "/BrokenDep.hs") brokenDepSrc
  (ok, err) <- extractError tag "LoadBarrierTarget" targetSrc "target" needle
  pure $ check ("failed dependency load stops at the barrier: " ++ err) ok
  where
    tag = "load-barrier"
    needle = "runPipeline: module load failed compiling"

brokenDepSrc :: String
brokenDepSrc = unlines
  [ "module BrokenDep where"
  , ""
  , "brokenValue :: Int"
  , "brokenValue = \"not an Int\""
  ]

targetSrc :: String
targetSrc = unlines
  [ "module LoadBarrierTarget where"
  , ""
  , "import BrokenDep (brokenValue)"
  , ""
  , "target :: Int"
  , "target = brokenValue"
  ]
