{-# LANGUAGE PartialTypeSignatures #-}

module Main (main) where

import Control.Monad (unless)
import GHC
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import System.Directory (createDirectoryIfMissing, getTemporaryDirectory)
import System.FilePath ((</>))
import System.Process (readProcess)
import Tidepool.Introspection

main :: IO ()
main = do
  root <- (</> "tidepool-introspection-search-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "LookupFixture.hs"
  writeFile source fixture
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  (repeatMatches, wildMatches) <- runGhc (Just libdir) $ do
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
    summary <- getModSummary (mkModuleName "LookupFixture")
    parsed <- normalizeLookupWildcards <$> parseModule summary
    typed <- typecheckModule parsed
    let (tcEnv, _) = tm_internals_ typed
        rdrEnv = tcg_rdr_env tcEnv
    (repeatBinder, queryRepeat) <- findId rdrEnv "queryRepeat"
    (wildBinder, queryWild) <- findId rdrEnv "queryWild"
    (,)
      <$> searchTypeMatches rdrEnv repeatBinder queryRepeat
      <*> searchTypeMatches rdrEnv wildBinder queryWild
  let repeatNames = map typeMatchName repeatMatches
      wildNames = map typeMatchName wildMatches
  unless ("queryRepeat" `notElem` repeatNames && "queryWild" `notElem` wildNames) $
    fail "reserved query binder leaked into its own results"
  unless ("same" `elem` repeatNames) $
    fail ("repeated variable query missed same: " ++ show repeatMatches)
  unless
    ( any
        (\result -> typeMatchName result == "same" && typeMatchQuality result == TypeMatchExact)
        repeatMatches
    )
    (fail ("alpha-equivalent signature was not exact: " ++ show repeatMatches))
  unless ("differentFixed" `notElem` repeatNames) $
    fail ("repeated variable query accepted fixed mismatch: " ++ show repeatMatches)
  unless ("wildUseful" `elem` wildNames) $
    fail ("independent wildcards missed wildUseful: " ++ show wildMatches)
  unless (isSorted (map matchKey repeatMatches) && isSorted (map matchKey wildMatches)) $
    fail "matches were not deterministic by quality/name/module/signature"
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse
    matchKey result =
      ( typeMatchQuality result,
        typeMatchName result,
        typeMatchModule result,
        typeMatchSignature result
      )

findId :: (GhcMonad m) => GlobalRdrEnv -> String -> m (Name, Type)
findId rdrEnv wanted = do
  let names =
        [ greName entry
        | entry <- globalRdrEnvElts rdrEnv,
          occNameString (nameOccName (greName entry)) == wanted
        ]
  found <- mapM lookupName names
  case [(getName identifier, idType identifier) | Just (AnId identifier) <- found] of
    result : _ -> pure result
    [] -> error ("missing fixture identifier " ++ wanted)

isSorted :: (Ord a) => [a] -> Bool
isSorted values = and (zipWith (<=) values (drop 1 values))

fixture :: String
fixture =
  unlines
    [ "{-# LANGUAGE PartialTypeSignatures #-}",
      "module LookupFixture where",
      "queryRepeat :: a -> (a, a)",
      "queryRepeat value = (value, value)",
      "same :: b -> (b, b)",
      "same value = (value, value)",
      "differentFixed :: Int -> (Int, String)",
      "differentFixed = undefined",
      "queryWild :: _ -> Maybe _",
      "queryWild = undefined",
      "wildUseful :: Int -> Maybe String",
      "wildUseful = undefined"
    ]
