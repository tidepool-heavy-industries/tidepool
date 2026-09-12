{-# LANGUAGE LambdaCase #-}

-- Feasibility probe for lookup's first source boundary. This deliberately uses
-- GHC's parsed/typechecked target rather than a second Haskell parser.
module Main (main) where

import Control.Monad (unless)
import Data.List (find)
import GHC
import GHC.Core.Unify (tcMatchTy)
import GHC.Driver.Session (xopt_set)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.Id (idType)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (globalRdrEnvElts, greName)
import GHC.Types.TyThing (TyThing (AnId))
import GHC.Types.Var (varName)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import System.Directory (createDirectoryIfMissing, getTemporaryDirectory)
import System.FilePath ((</>))
import System.Process (readProcess)

main :: IO ()
main = do
  root <- (</> "tidepool-type-query-probe") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "Expr.hs"
  writeFile source $ unlines
    [ "{-# LANGUAGE ExplicitForAll #-}"
    , "module Expr where"
    , "import Data.Maybe"
    , "useful :: a -> Maybe a"
    , "useful = Just"
    ]
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  (queryText, wildcardText, matchName, wildcardMatchName, visible) <-
    runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags
      (xopt_set flags LangExt.PartialTypeSignatures)
        { importPaths = root : importPaths flags
        , hiDir = Just root
        , objectDir = Just root
        }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "Expr")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    let (tcEnv, _) = tm_internals_ typed
        rdrEnv = tcg_rdr_env tcEnv
        names = map greName (globalRdrEnvElts rdrEnv)
    setContext
      [ IIDecl (simpleImportDecl (mkModuleName "Expr"))
      , IIDecl (simpleImportDecl (mkModuleName "Data.Maybe"))
      ]
    (query, _) <- typeKind False "forall a. a -> Maybe a"
    (wildcardQuery, _) <- typeKind False "_ -> Maybe _"
    candidates <- mapM lookupName names
    let ids = [identifier | Just (AnId identifier) <- candidates]
        matches expected identifier =
          let (_, _, queryBody) = tcSplitSigmaTy expected
              (_, _, candidateBody) = tcSplitSigmaTy (idType identifier)
           in case tcMatchTy queryBody candidateBody of
                Just _ -> True
                Nothing -> False
        named identifier =
          occNameString (nameOccName (varName identifier)) == "useful"
        chosen = find
          (\identifier -> named identifier && matches query identifier)
          ids
        wildcardChosen = find
          (\identifier -> named identifier && matches wildcardQuery identifier)
          ids
    pure
      ( renderWithContext defaultSDocContext (ppr query)
      , renderWithContext defaultSDocContext (ppr wildcardQuery)
      , fmap (occNameString . nameOccName . varName) chosen
      , fmap (occNameString . nameOccName . varName) wildcardChosen
      , length names
      )
  putStrLn ("query=" ++ queryText)
  putStrLn ("wildcard-query=" ++ wildcardText)
  putStrLn ("visible=" ++ show visible)
  putStrLn ("match=" ++ show matchName)
  putStrLn ("wildcard-match=" ++ show wildcardMatchName)
  unless (matchName == Just "useful") $
    fail "scoped query did not match the visible useful function"
  unless (wildcardMatchName == Nothing) $
    fail "probe assumption changed: anonymous holes unexpectedly became match variables"
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse
