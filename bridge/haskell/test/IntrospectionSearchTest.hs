{-# LANGUAGE PartialTypeSignatures #-}

module Main (main) where

import Control.Monad (unless)
import GHC
import GHC.Tc.Types (TcGblEnv, tcg_rdr_env)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import System.Directory (createDirectoryIfMissing, getTemporaryDirectory)
import System.FilePath ((</>))
import System.Process (callProcess, readProcess)
import Tidepool.Introspection

main :: IO ()
main = do
  root <- (</> "tidepool-introspection-search-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "LookupFixture.hs"
  writeFile (root </> "QualifiedSource.hs") qualifiedFixture
  writeFile (root </> "AmbigA.hs") ambiguousAFixture
  writeFile (root </> "AmbigB.hs") ambiguousBFixture
  writeFile source fixture
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  (repeatMatches, wildMatches, anyMatches, collisionMatches, numMatches, plainMatches, rankMatches, qualifiedMatches, ambiguousMatches) <-
    runGhc (Just libdir) $ do
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
      hscEnv <- getSession
      repeatMatches <- matchesFor hscEnv tcEnv rdrEnv "queryRepeat"
      wildMatches <- matchesFor hscEnv tcEnv rdrEnv "queryWild"
      anyMatches <- matchesFor hscEnv tcEnv rdrEnv "queryAny"
      collisionMatches <- matchesFor hscEnv tcEnv rdrEnv "queryCollision"
      numMatches <- matchesFor hscEnv tcEnv rdrEnv "queryNum"
      plainMatches <- matchesFor hscEnv tcEnv rdrEnv "queryPlain"
      rankMatches <- matchesFor hscEnv tcEnv rdrEnv "queryRank"
      qualifiedMatches <- matchesFor hscEnv tcEnv rdrEnv "queryQualified"
      ambiguousMatches <- matchesFor hscEnv tcEnv rdrEnv "queryAmbiguous"
      pure
        ( repeatMatches,
          wildMatches,
          anyMatches,
          collisionMatches,
          numMatches,
          plainMatches,
          rankMatches,
          qualifiedMatches,
          ambiguousMatches
        )
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
  unless ("candidateMismatch" `elem` map typeMatchName anyMatches) $
    fail ("_ -> _ missed Int -> Bool: " ++ show anyMatches)
  unless ("candidateCollision" `elem` map typeMatchName collisionMatches) $
    fail ("fresh wildcard collided with explicit spelling: " ++ show collisionMatches)
  unless (isSorted (map matchKey repeatMatches) && isSorted (map matchKey wildMatches)) $
    fail "matches were not deterministic by quality/name/module/signature"
  unless (isExact "candidateNum" numMatches) $
    fail ("alpha-equivalent constrained signature was not exact: " ++ show numMatches)
  unless (hasAvailability "candidatePlain" Available numMatches) $
    fail ("constrained query lost usable plain candidate: " ++ show numMatches)
  -- `Num b => b -> b` is usable against `a -> a`; the instance comes from the
  -- call site. It ranks below an unconstrained match and above an unknown.
  unless (hasAvailability "candidateNum" Polymorphic plainMatches) $
    fail ("plain query lost call-site constrained candidate: " ++ show plainMatches)
  unless ("candidateMismatch" `notElem` map typeMatchName plainMatches) $
    fail ("repeated query variable accepted Int -> Bool: " ++ show plainMatches)
  unless (isExact "candidateRank" rankMatches) $
    fail ("alpha-equivalent nested forall was not exact: " ++ show rankMatches)
  unless ("candidateMonoRank" `notElem` map typeMatchName rankMatches) $
    fail ("nested forall matched monomorphic argument: " ++ show rankMatches)
  let qualifiedNames =
        filter (== "Q.onlyQualified") (map typeMatchName qualifiedMatches)
      ambiguousNames =
        filter (`elem` ["A.clash", "B.clash"]) (map typeMatchName ambiguousMatches)
  unless (qualifiedNames == ["Q.onlyQualified"]) $
    fail ("qualified-only import did not retain its usable alias: " ++ show qualifiedMatches)
  let operatorNames = filter (== "(Q.%%)") (map typeMatchName qualifiedMatches)
  unless (operatorNames == ["(Q.%%)"]) $
    fail ("qualified operator was not a usable expression: " ++ show qualifiedMatches)
  unless (ambiguousNames == ["A.clash", "B.clash"]) $
    fail ("ambiguous imports did not choose qualified spellings: " ++ show ambiguousMatches)
  unless (any (\result -> typeMatchName result == "candidateNum"
      && any ((== "Num") . identifierName) (typeMatchReferences result)) numMatches) $
    fail "constrained type search lost its compiler-resolved class reference"
  unless (any (\result -> typeMatchName result == "wildUseful"
      && all (`elem` map identifierName (typeMatchReferences result)) ["Int", "Maybe", "Char"]) wildMatches) $
    fail "nested type search lost its compiler-resolved nominal references"
  unless (any (\result -> typeMatchName result == "candidatePromoted"
      && any (\reference -> identifierName reference == "True"
          && identifierNamespace reference == ConstructorIdentifier) (typeMatchReferences result)) anyMatches) $
    fail "promoted constructor reference lost its constructor namespace"
  let returnedUse = root </> "ReturnedUse.hs"
  qualified <- case qualifiedNames of
    [name] -> pure name
    _ -> fail "qualified result changed after validation"
  writeFile returnedUse (returnedFixture qualified (ambiguousNames ++ operatorNames))
  callProcess "ghc" ["-fno-code", "-i" ++ root, returnedUse]
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse
    matchKey result =
      ( typeMatchAvailability result,
        typeMatchQuality result,
        typeMatchName result,
        typeMatchModule result,
        typeMatchSignature result
      )
    isExact wanted =
      any
        (\result -> typeMatchName result == wanted && typeMatchQuality result == TypeMatchExact)
    hasAvailability wanted availability =
      any
        (\result -> typeMatchName result == wanted && typeMatchAvailability result == availability)

matchesFor :: (GhcMonad m) => HscEnv -> TcGblEnv -> GlobalRdrEnv -> String -> m [TypeMatch]
matchesFor hscEnv tcEnv rdrEnv wanted = do
  (binder, query) <- findId rdrEnv wanted
  searchTypeMatches hscEnv tcEnv rdrEnv binder query

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
      "{-# LANGUAGE RankNTypes, DataKinds #-}",
      "module LookupFixture where",
      "import qualified QualifiedSource as Q",
      "import Data.Proxy (Proxy)",
      "import AmbigA",
      "import AmbigB",
      "import qualified AmbigA as A",
      "import qualified AmbigB as B",
      "queryRepeat :: a -> (a, a)",
      "queryRepeat value = (value, value)",
      "same :: b -> (b, b)",
      "same value = (value, value)",
      "differentFixed :: Int -> (Int, String)",
      "differentFixed = undefined",
      "queryWild :: _ -> Maybe _",
      "queryWild = undefined",
      "wildUseful :: Int -> Maybe String",
      "wildUseful = undefined",
      "queryAny :: _ -> _",
      "queryAny = undefined",
      "queryCollision :: forall __lookup_w0. __lookup_w0 -> _",
      "queryCollision = undefined",
      "candidateCollision :: Int -> Bool",
      "candidateCollision = undefined",
      "queryNum :: Num a => a -> a",
      "queryNum = id",
      "candidateNum :: Num b => b -> b",
      "candidateNum = id",
      "candidatePlain :: b -> b",
      "candidatePlain = id",
      "queryPlain :: a -> a",
      "queryPlain = id",
      "candidatePromoted :: Proxy 'True -> Bool",
      "candidatePromoted = undefined",
      "candidateMismatch :: Int -> Bool",
      "candidateMismatch = undefined",
      "queryRank :: (forall a. a -> a) -> Int",
      "queryRank _ = 0",
      "candidateRank :: (forall b. b -> b) -> Int",
      "candidateRank _ = 0",
      "candidateMonoRank :: (Int -> Int) -> Int",
      "candidateMonoRank _ = 0",
      "queryQualified :: Int -> Bool",
      "queryQualified = undefined",
      "queryAmbiguous :: Int -> Bool",
      "queryAmbiguous = undefined"
    ]

qualifiedFixture :: String
qualifiedFixture =
  unlines
    [ "module QualifiedSource (onlyQualified, (%%)) where",
      "onlyQualified :: Int -> Bool",
      "onlyQualified = undefined",
      "(%%) :: Int -> Bool",
      "(%%) = undefined"
    ]

ambiguousAFixture :: String
ambiguousAFixture =
  unlines
    [ "module AmbigA (clash) where",
      "clash :: Int -> Bool",
      "clash = undefined"
    ]

ambiguousBFixture :: String
ambiguousBFixture =
  unlines
    [ "module AmbigB (clash) where",
      "clash :: Int -> Bool",
      "clash = undefined"
    ]

returnedFixture :: String -> [String] -> String
returnedFixture qualified ambiguous =
  unlines $
    [ "module ReturnedUse where",
      "import qualified QualifiedSource as Q",
      "import AmbigA",
      "import AmbigB",
      "import qualified AmbigA as A",
      "import qualified AmbigB as B",
      "qualifiedResult :: Int -> Bool",
      "qualifiedResult = " ++ qualified
    ]
      ++ concat
        [ [ "ambiguousResult" ++ show index ++ " :: Int -> Bool",
            "ambiguousResult" ++ show index ++ " = " ++ spelling
          ]
        | (index, spelling) <- zip [(1 :: Int) ..] ambiguous
        ]
