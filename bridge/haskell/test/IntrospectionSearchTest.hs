{-# LANGUAGE PartialTypeSignatures #-}

module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (nub)
import Data.Map.Strict qualified as Map
import GHC
import GHC.Tc.Types (TcGblEnv, tcg_rdr_env, tcg_type_env)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Types.Name (nameOccName, nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import System.Directory (createDirectoryIfMissing, getTemporaryDirectory)
import System.FilePath ((</>))
import System.Process (callProcess, readProcess)
import Tidepool.ExtractRequest (InspectionRequest (..), StructuredInspection(..), StructuredNameScope(..), StructuredNameNamespace(..), InspectionProvenance(..))
import Tidepool.Introspection

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test"
  [ testCase "compiled introspection searches" scenario,
    testCase "bounded browse dedupes before the budget" browseScenario,
    testCase "compiled scoped identifier selection" structuredScenario
  ]

-- These identities come from an actual checked GHC module. Duplicate record
-- fields deliberately have distinct Names with the same public rendering;
-- repeated qualified imports deliberately reach the same actual Name.
structuredScenario :: IO ()
structuredScenario = do
  root <- (</> "tidepool-introspection-identity-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  writeFile (root </> "QualifiedSource.hs") qualifiedFixture
  let source = root </> "StructuredIdentityFixture.hs"
  writeFile source (unlines
    [ "{-# LANGUAGE DuplicateRecordFields #-}"
    , "module StructuredIdentityFixture where"
    , "import qualified QualifiedSource as Q"
    , "import qualified QualifiedSource as R"
    , "candidatePlain :: a -> a"
    , "candidatePlain value = value"
    , "data LeftRecord = LeftRecord { sharedField :: Int }"
    , "data RightRecord = RightRecord { sharedField :: Bool }"
    ])
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  (answers,fieldNames,qualifiedNames) <- withInspectionWork $ \work -> runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags {importPaths=root:importPaths flags,hiDir=Just root,objectDir=Just root}
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    loaded <- load LoadAllTargets
    case loaded of Failed -> liftIO (fail "structured identity fixture did not load"); Succeeded -> pure ()
    summary <- getModSummary (mkModuleName "StructuredIdentityFixture")
    typed <- typecheckModule =<< parseModule summary
    let (checked,_) = tm_internals_ typed
        reader = tcg_rdr_env checked
        names occurrence = nub [greName entry | entry <- globalRdrEnvElts reader
          , occNameString (nameOccName (greName entry)) == occurrence]
        query namespace name = StructuredInspection StructuredCurrentScope namespace name (InspectionProvenance 17 "checked-identity")
    value <- case [identifier | identifier <- typeEnvIds (tcg_type_env checked)
        , occNameString (nameOccName (getName identifier)) == "candidatePlain"] of
      [identifier] -> pure identifier
      _ -> liftIO (fail "actual value identity was not unique")
    environment <- getSession
    answers <- runWorldInspectionGhc work environment checked reader Map.empty
      (Map.singleton "candidatePlain" value)
      [ InspectStructuredInfoOf (query StructuredValueName "Q.onlyQualified")
      , InspectStructuredTypeOf (query StructuredTypeName "candidatePlain")
      , InspectStructuredInfoOf (query StructuredValueName "Q.%%")
      , InspectKeyedAvailability (query StructuredValueName "onlyQualified") "QualifiedSource"
      , InspectKeyedAvailability (query StructuredFieldName "sharedField") "StructuredIdentityFixture"
      , InspectStructuredInfoOf (query StructuredAnyName "sharedField")
      , InspectStructuredInfoOf (query StructuredValueName "R.onlyQualified")
      , InspectStructuredInfoOf (query StructuredValueName ".")
      , InspectStructuredInfoOf (query StructuredValueName "Q..%")
      ]
    pure (answers,names "sharedField",names "onlyQualified")
  let publicSpelling name = (moduleNameString . moduleName <$> nameModule_maybe name,occNameString (nameOccName name))
  unless (length fieldNames == 2 && length (nub fieldNames) == 2
      && length (nub (map publicSpelling fieldNames)) == 1) $
    fail "duplicate-field control must retain distinct actual Names with equal public spelling"
  unless (length qualifiedNames == 1) $
    fail "repeated imports must reach one actual Name"
  case answers of
    [ InspectionStructuredInfo qualified
      , InspectionStructuredError (StructuredUnknown missing)
      , InspectionStructuredInfo operator
      , InspectionRowFit fit Available
      , InspectionStructuredError (StructuredUnsupported _)
      , InspectionStructuredError (StructuredAmbiguous _ fields)
      , InspectionStructuredInfo repeated
      , InspectionStructuredInfo composition
      , InspectionStructuredInfo dotted
      ] | inspectedIdentifier qualified == IdentifierRef "QualifiedSource" "onlyQualified" ValueIdentifier
          && inspectedIdentifier repeated == inspectedIdentifier qualified
          && structuredNamespace missing == StructuredTypeName
          && inspectedIdentifier operator == IdentifierRef "QualifiedSource" "%%" ValueIdentifier
          && fit == inspectedIdentifier qualified
          && identifierName (inspectedIdentifier composition) == "."
          && identifierNamespace (inspectedIdentifier composition) == ValueIdentifier
          && inspectedIdentifier dotted == IdentifierRef "QualifiedSource" ".%" ValueIdentifier
          && length fields == 2 && head fields == last fields -> pure ()
    other -> fail ("structured identity selection lost namespace/alias/ambiguity: " ++ show other)
  where trim = reverse . dropWhile (`elem` ['\n','\r']) . reverse

scenario :: IO ()
scenario = do
  root <- (</> "tidepool-introspection-search-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "InspectionSearchFixture.hs"
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
      summary <- getModSummary (mkModuleName "InspectionSearchFixture")
      parsed <- normalizeInspectionWildcards <$> parseModule summary
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

-- A budgeted browse reports the retained distinct entries, so a source larger
-- than the budget reads as "at least budget". The browsed module is imported
-- rather than the checked target, exercising `modInfoExports`; the fixture also
-- exports one type twice (through `Wrapper(..)` and `Wrapper(Wrapped)`) so a
-- future regression that counted export items rather than entities would
-- under-fill the budget.
browseScenario :: IO ()
browseScenario = do
  root <- (</> "tidepool-introspection-browse-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "InspectionBrowseHost.hs"
      library = root </> "InspectionBrowseLibrary.hs"
      budget = 3
  writeFile library browseLibrary
  writeFile source browseHost
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  unless (boundedDistinctNames Nothing [3 :: Int, 1, 3, 2, 1] == [3, 1, 2]) $
    fail "names must keep first-seen order"
  -- The budget composes after deduplication. GHC's export APIs currently
  -- dedupe repeated items themselves, so this pure assertion is what pins the
  -- ordering: budgeting first would return [1, 2] for a budget of three.
  unless (boundedDistinctNames (Just 3) [1 :: Int, 1, 2, 2, 3, 3, 4] == [1, 2, 3]) $
    fail "the budget must count distinct candidates"
  results <- runGhc (Just libdir) $ do
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
    summary <- getModSummary (mkModuleName "InspectionBrowseHost")
    typed <- typecheckModule =<< parseModule summary
    let (tcEnv, _) = tm_internals_ typed
    hscEnv <- getSession
    liftIO $
      runInspection hscEnv tcEnv (tcg_rdr_env tcEnv) Map.empty
        [InspectModuleBounded "InspectionBrowseLibrary" False budget]
  case results of
    [InspectionBrowse moduleName False entries] -> do
      unless (moduleName == "InspectionBrowseLibrary") $
        fail ("browse named another module: " ++ moduleName)
      let names = map infoName entries
      unless (names == ["Wrapper", "alpha", "beta"]) $
        fail ("a truncated browse must retain the first budget distinct names: " ++ show entries)
      unless (length (nub names) == budget) $
        fail ("retained names were not distinct: " ++ show names)
    other -> fail ("expected one expanded browse result: " ++ show other)
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse

browseLibrary :: String
browseLibrary =
  unlines
    [ "module InspectionBrowseLibrary",
      "  ( Wrapper (..),",
      "    Wrapper (Wrapped),",
      "    alpha,",
      "    beta",
      "  ) where",
      "",
      "data Wrapper = Wrapped Int",
      "",
      "alpha :: Int",
      "alpha = 1",
      "",
      "beta :: Bool",
      "beta = True",
      ""
    ]

browseHost :: String
browseHost =
  unlines
    [ "module InspectionBrowseHost where",
      "import InspectionBrowseLibrary",
      "",
      "hostValue :: Int",
      "hostValue = alpha"
    ]

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
      "module InspectionSearchFixture where",
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
      "queryCollision :: forall __inspection_w0. __inspection_w0 -> _",
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
    [ "module QualifiedSource (onlyQualified, (%%), (.%)) where",
      "onlyQualified :: Int -> Bool",
      "onlyQualified = undefined",
      "(%%) :: Int -> Bool",
      "(%%) = undefined",
      "(.%) :: Int -> Bool",
      "(.%) = undefined"
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
