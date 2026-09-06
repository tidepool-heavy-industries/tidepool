{-# LANGUAGE OverloadedStrings #-}

-- | Intrinsic recognizers key on the ORIGINAL defining module, so a
-- user-local binding that merely shares an occurrence name with a surface
-- verb lowers as ordinary Core. Each check below defines its own shadow
-- (no Tidepool import at all) and asserts extraction succeeds with the
-- shadow's call site translated as an ordinary Core call — never rewritten
-- to an intrinsic primop, and never head-swapped onto a hidden *Sited
-- helper (which, absent any real sibling in scope, would otherwise emit a
-- lazy 0x45-tagged poison NVar for the call site).
module Fidelity.Recognizers (checks, floatingChecks) where

import Fidelity.Harness (Check, check, extractBinding, nodeList, nvarIds)
import Tidepool.Translate (ClosedModule)
import Tidepool.IR (FlatNode(..))

import Data.Bits (shiftR)
import Data.Word (Word64)
import qualified Data.Text as T

checks :: IO [Check]
checks = concat <$> sequence
  [ floatingChecks
  , shadowCheck "e3-either-decode-value" "UEDV"      eitherDecodeValueSrc "useEDV"      (noPrimOp "JsonDecode")
  , shadowCheck "e3-parse-iso8601"       "UPISO"     parseISO8601Src      "usePISO"     (noPrimOp "ParseISO8601")
  , shadowCheck "e3-run-llm-turn"        "URLT"      runLLMTurnSrc        "useRLT"      noPoison
  , shadowCheck "e3-run-llm-turn-fork"   "URLTF"     runLLMTurnForkSrc    "useRLTF"     noPoison
  , shadowCheck "e3-run-llm-turn-fanout" "URLTO"     runLLMTurnFanoutSrc  "useRLTO"     noPoison
  , shadowCheck "e3-fork"                "UFork"     forkSrc              "useFork"     noPoison
  , shadowCheck "e3-fork-all"            "UForkAll"  forkAllSrc           "useForkAll"  noPoison
  , shadowCheck "e3-fork-map"            "UForkMap"  forkMapSrc           "useForkMap"  noPoison
  , shadowCheck "e3-fork-cata"           "UForkCata" forkCataSrc          "useForkCata" noPoison
  , shadowCheck "e3-finalize"            "UFinalize" finalizeSrc          "useFinalize" noPoison
  ]

-- | Extract @target@ from @src@ (a self-contained fixture with no Tidepool
-- imports, written to its own work-dir @tag@) and check the result both
-- extracts cleanly and isn't rewritten by the intrinsic machinery
-- (@notRewritten@, one of 'noPoison'/'noPrimOp' below).
shadowCheck :: String -> String -> String -> String -> (ClosedModule -> Bool) -> IO [Check]
shadowCheck tag modName src target notRewritten = do
  r <- extractBinding tag modName src target
  pure $ case r of
    Left err -> [ check (modName ++ ": extraction should succeed but errored: " ++ err) False ]
    Right cm -> [ check (modName ++ ": extracts without error") True
                , check (modName ++ ": user-defined binding lowers as ordinary Core") (notRewritten cm)
                ]

-- | No lazy-poison NVar (tag 'E', see 'Tidepool.Translate.emitFfiPoison')
-- anywhere in the translated closure — the shape a wrongly head-swapped
-- runLLMTurn/fork/forkAll/forkMap/forkCata/finalize call falls into when a
-- name-only 'findAuxVarId' scan finds no real *Sited sibling (none is in
-- scope here, since these fixtures import nothing from Tidepool).
noPoison :: ClosedModule -> Bool
noPoison cm = not (any isPoison (nvarIds cm))
  where isPoison v = (v `shiftR` 56) == (0x45 :: Word64)

-- | No 'FlatNode.NPrimOp' node named @needle@ anywhere in the translated
-- closure — the shape a wrongly intercepted eitherDecodeValue/parseISO8601
-- call falls into (the user's real body silently discarded).
noPrimOp :: T.Text -> ClosedModule -> Bool
noPrimOp needle cm = not (any isIt (nodeList cm))
  where
    isIt (NPrimOp name _) = name == needle
    isIt _                = False

eitherDecodeValueSrc :: String
eitherDecodeValueSrc = unlines
  [ "module UEDV (useEDV) where"
  , ""
  , "{-# OPAQUE eitherDecodeValue #-}"
  , "eitherDecodeValue :: String -> Either String Int"
  , "eitherDecodeValue s = if null s then Left \"empty\" else Right (length s)"
  , ""
  , "useEDV :: Either String Int"
  , "useEDV = eitherDecodeValue \"hello\""
  ]

parseISO8601Src :: String
parseISO8601Src = unlines
  [ "module UPISO (usePISO) where"
  , ""
  , "{-# OPAQUE parseISO8601 #-}"
  , "parseISO8601 :: String -> Either String Int"
  , "parseISO8601 s = if null s then Left \"empty\" else Right (length s)"
  , ""
  , "usePISO :: Either String Int"
  , "usePISO = parseISO8601 \"2024-01-01\""
  ]

runLLMTurnSrc :: String
runLLMTurnSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module URLT (useRLT) where"
  , ""
  , "{-# OPAQUE runLLMTurn #-}"
  , "runLLMTurn :: forall a. String -> Maybe a"
  , "runLLMTurn _ = Nothing"
  , ""
  , "useRLT :: Maybe Int"
  , "useRLT = runLLMTurn \"hello\""
  ]

runLLMTurnForkSrc :: String
runLLMTurnForkSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module URLTF (useRLTF) where"
  , ""
  , "{-# OPAQUE runLLMTurnFork #-}"
  , "runLLMTurnFork :: forall a. String -> Maybe a"
  , "runLLMTurnFork _ = Nothing"
  , ""
  , "useRLTF :: Maybe Int"
  , "useRLTF = runLLMTurnFork \"hello\""
  ]

runLLMTurnFanoutSrc :: String
runLLMTurnFanoutSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module URLTO (useRLTO) where"
  , ""
  , "{-# OPAQUE runLLMTurnFanout #-}"
  , "runLLMTurnFanout :: forall a. String -> Maybe a"
  , "runLLMTurnFanout _ = Nothing"
  , ""
  , "useRLTO :: Maybe Int"
  , "useRLTO = runLLMTurnFanout \"hello\""
  ]

forkSrc :: String
forkSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module UFork (useFork) where"
  , ""
  , "{-# OPAQUE fork #-}"
  , "fork :: forall a. String -> Maybe a"
  , "fork _ = Nothing"
  , ""
  , "useFork :: Maybe Int"
  , "useFork = fork \"hello\""
  ]

forkAllSrc :: String
forkAllSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module UForkAll (useForkAll) where"
  , ""
  , "{-# OPAQUE forkAll #-}"
  , "forkAll :: forall a. String -> Maybe a"
  , "forkAll _ = Nothing"
  , ""
  , "useForkAll :: Maybe Int"
  , "useForkAll = forkAll \"hello\""
  ]

forkMapSrc :: String
forkMapSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module UForkMap (useForkMap) where"
  , ""
  , "{-# OPAQUE forkMap #-}"
  , "forkMap :: forall b a. (a -> String) -> [a] -> [b]"
  , "forkMap _ _ = []"
  , ""
  , "useForkMap :: [Bool]"
  , "useForkMap = forkMap show ([1,2,3] :: [Int])"
  ]

forkCataSrc :: String
forkCataSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module UForkCata (useForkCata) where"
  , ""
  , "{-# OPAQUE forkCata #-}"
  , "forkCata :: forall b a. (a -> [b] -> String) -> [a] -> Maybe b"
  , "forkCata _ _ = Nothing"
  , ""
  , "useForkCata :: Maybe Bool"
  , "useForkCata = forkCata (\\_ bs -> show (length bs)) ([1,2,3] :: [Int])"
  ]

finalizeSrc :: String
finalizeSrc = unlines
  [ "{-# LANGUAGE ScopedTypeVariables #-}"
  , "module UFinalize (useFinalize) where"
  , ""
  , "{-# OPAQUE finalize #-}"
  , "finalize :: forall v a. v -> Maybe a"
  , "finalize _ = Nothing"
  , ""
  , "useFinalize :: Maybe Int"
  , "useFinalize = finalize \"hello\""
  ]

-- Real extraction protects the symbol boundary, including package-qualified FFI.
floatingChecks :: IO [Check]
floatingChecks = do
  src <- readFile "test-fidelity/fixtures/FloatClassifiers.hs"
  near <- readFile "test-fidelity/fixtures/NearFfi.hs"
  a <- extractBinding "floating-classifiers" "FloatClassifiers" src "classifiers"
  b <- extractBinding "floating-near-ffi" "NearFfi" near "probe"
  pure $ case (a, b) of
    (Right cm, Right nearCm) ->
      [ check ("classification lowering: " ++ T.unpack name) (not (noPrimOp name cm))
      | name <- ["FfiIsDoubleNaN", "FfiIsDoubleInfinite", "FfiIsDoubleNegativeZero",
                 "FfiIsFloatNaN", "FfiIsFloatInfinite", "FfiIsFloatNegativeZero"] ]
      ++ [check "near-name FFI is not recognized" (noPrimOp "FfiIsDoubleNaN" nearCm),
          check "unsupported near-name FFI remains poison" (not (noPoison nearCm))]
    (Left err, _) -> [check ("classifier extraction: " ++ err) False]
    (_, Left err) -> [check ("near-name extraction: " ++ err) False]
