{-# LANGUAGE PartialTypeSignatures #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE OverloadedStrings #-}

module Main (main) where

import Data.Maybe (isJust)
import GHC
import GHC.Core.Unify (tcMatchTy, tcUnifyTy)
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import System.Directory (createDirectoryIfMissing, getTemporaryDirectory)
import System.FilePath ((</>))
import System.Process (readProcess)
import Tidepool.Introspection (normalizeLookupWildcards)

main :: IO ()
main = do
  root <- (</> "tidepool-matcher-matrix") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "MatcherFixture.hs"
  writeFile source fixture
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  rows <- runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags
      flags
        { importPaths = root : importPaths flags,
          hiDir = Just root,
          objectDir = Just root
        }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "MatcherFixture")
    parsed <- normalizeLookupWildcards <$> parseModule summary
    typed <- typecheckModule parsed
    let (tcEnv, _) = tm_internals_ typed
        rdrEnv = tcg_rdr_env tcEnv
    mapM (row rdrEnv)
      [ ("poly/exact", "queryPoly", "candidatePoly"),
        ("poly/concrete", "queryPoly", "candidateConcrete"),
        ("wild/concrete", "queryWild", "candidateConcrete"),
        ("num/num", "queryNum", "candidateNum"),
        ("num/plain", "queryNum", "candidatePoly"),
        ("plain/num", "queryPoly", "candidateNum"),
        ("rank/exact", "queryRank", "candidateRank"),
        ("rank/monomorphic", "queryRank", "candidateMonoRank")
      ]
  mapM_ print rows
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse

row :: (GhcMonad m) => GlobalRdrEnv -> (String, String, String) -> m (String, Checks, Checks)
row rdrEnv (label, queryName, candidateName) = do
  query <- findType rdrEnv queryName
  candidate <- findType rdrEnv candidateName
  let (_, _, queryBody) = tcSplitSigmaTy query
      (_, _, candidateBody) = tcSplitSigmaTy candidate
  pure
    ( label,
      checks query candidate,
      checks queryBody candidateBody
    )

type Checks = (Bool, Bool, Bool)

checks :: Type -> Type -> Checks
checks query candidate =
  ( isJust (tcMatchTy query candidate),
    isJust (tcMatchTy candidate query),
    isJust (tcUnifyTy query candidate)
  )

findType :: (GhcMonad m) => GlobalRdrEnv -> String -> m Type
findType rdrEnv wanted = do
  let names =
        [ greName entry
        | entry <- globalRdrEnvElts rdrEnv,
          occNameString (nameOccName (greName entry)) == wanted
        ]
  found <- mapM lookupName names
  case [idType identifier | Just (AnId identifier) <- found] of
    value : _ -> pure value
    [] -> error ("missing " ++ wanted)

fixture :: String
fixture =
  unlines
    [ "{-# LANGUAGE PartialTypeSignatures #-}",
      "{-# LANGUAGE RankNTypes #-}",
      "module MatcherFixture where",
      "queryPoly :: a -> a",
      "queryPoly = id",
      "candidatePoly :: b -> b",
      "candidatePoly = id",
      "candidateConcrete :: Int -> Int",
      "candidateConcrete = id",
      "queryWild :: _ -> _",
      "queryWild = undefined",
      "queryNum :: Num a => a -> a",
      "queryNum = id",
      "candidateNum :: Num b => b -> b",
      "candidateNum = id",
      "queryRank :: (forall a. a -> a) -> Int",
      "queryRank _ = 0",
      "candidateRank :: (forall b. b -> b) -> Int",
      "candidateRank _ = 0",
      "candidateMonoRank :: (Int -> Int) -> Int",
      "candidateMonoRank _ = 0"
    ]
