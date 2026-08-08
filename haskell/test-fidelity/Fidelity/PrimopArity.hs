-- | Two fail-loud contracts of the extract pipeline.
--
-- The generic unboxed-tuple fallback can bind at most one result; every higher
-- result arity without a dedicated split fails loud at extract time rather
-- than aliasing several binders onto one node. And a failed module load is a
-- phase barrier — the pipeline stops there instead of silently returning a
-- 'PipelineResult' built against a half-populated environment. In
-- 'runPipeline' that barrier sits AFTER the per-module compile loop: a
-- compile error in the target or a dependency already throws a spanned
-- 'SourceError' from inside the loop (that span is what lets a user's own
-- type error render as a real diagnostic instead of this module's generic
-- message), and the barrier is a backstop for a 'Failed' load the loop does
-- not independently re-surface.
module Fidelity.PrimopArity (checks) where

import Fidelity.Harness (Check, check, extractBinding, extractError)

import Data.List (isInfixOf)
import System.Directory (createDirectoryIfMissing)

checks :: IO [Check]
checks = sequence
  [ threeResultCheck, twoResultCheck
  , brokenDepCheck, brokenTargetCheck
  ]

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

-- | Case 1 of the load-barrier contract: the target module imports a second
-- home-source module that fails to typecheck. The dependency's own summary
-- goes through the per-module compile loop just like the target's does, so
-- this also surfaces via the loop's spanned 'SourceError' (naming
-- @BrokenDep.hs@) rather than the barrier's generic message — the barrier is
-- reached only when a 'Failed' load leaves nothing for the loop to
-- independently re-fail on. 'extractBinding'/'extractError' write only the
-- target's own source file, so the broken dependency is written into the
-- same work dir here, before the harness call.
brokenDepCheck :: IO Check
brokenDepCheck = do
  let dir = "test-fidelity/work/" ++ tag
  createDirectoryIfMissing True dir
  writeFile (dir ++ "/BrokenDep.hs") brokenDepSrc
  result <- extractBinding tag "LoadBarrierTarget" targetSrc "target"
  let (ok, err) = case result of
        Left e  -> ( "BrokenDep.hs" `isInfixOf` e
                     && "Couldn't match type" `isInfixOf` e
                     && not (barrierMsg `isInfixOf` e)
                   , e )
        Right _ -> (False, "<extraction SUCCEEDED — expected a loud failure>")
  pure $ check ("broken dependency stops extraction with its own spanned error: " ++ err) ok
  where
    tag = "load-barrier-dep"
    barrierMsg = "runPipeline: module load failed compiling"

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

-- | Case 2 of the load-barrier contract, and the one that would have caught
-- the diagnostic-degrading regression: the fixture module ITSELF has a type
-- error. The failure text must carry GHC's own spanned type-error message,
-- not the barrier's generic "module load failed" text — that text only fires
-- for a 'Failed' load the per-module compile loop does not itself
-- re-surface, and a broken target's own 'typecheckModule' call always throws
-- first.
brokenTargetCheck :: IO Check
brokenTargetCheck = do
  result <- extractBinding "broken-target" "BrokenTarget" brokenTargetSrc "brokenTarget"
  let (ok, err) = case result of
        Left e  -> ( "Couldn't match type" `isInfixOf` e
                     && not (barrierMsg `isInfixOf` e)
                   , e )
        Right _ -> (False, "<extraction SUCCEEDED — expected a loud failure>")
  pure $ check ("broken target surfaces GHC's own type error, not the barrier: " ++ err) ok
  where
    barrierMsg = "runPipeline: module load failed compiling"

brokenTargetSrc :: String
brokenTargetSrc = unlines
  [ "module BrokenTarget where"
  , ""
  , "brokenTarget :: Int"
  , "brokenTarget = \"not an Int\""
  ]
