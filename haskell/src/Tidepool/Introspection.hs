module Tidepool.Introspection
  ( InspectionResult (..),
    InfoEntry (..),
    TypeMatch (..),
    TypeMatchQuality (..),
    normalizeLookupWildcards,
    searchTypeMatches,
    runInspection,
    encodeInspectionResults,
  )
where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (foldM, forM)
import Control.Monad.IO.Class (liftIO)
import Control.Monad.State.Strict (State, evalState, get, put)
import Data.ByteString qualified as BS
import Data.Generics (everything, everywhereM, mkM, mkQ)
import Data.List (nubBy, sortOn)
import Data.Map.Strict qualified as Map
import Data.Maybe (catMaybes, isJust)
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.Unify (tcMatchTy)
import GHC.Iface.Type (ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (mkTyVarOcc, occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, RdrName (..), globalRdrEnvElts, greName, greRdrNames, mkRdrUnqual, rdrNameOcc)
import GHC.Types.TyThing (tyThingParent_maybe)
import GHC.Types.TyThing.Ppr (pprTyThing, pprTyThingInContext)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import Tidepool.ExtractRequest (InspectionRequest (..))
import Tidepool.ExtractUtil (getLibdir)

data InfoEntry = InfoEntry
  { infoName :: String,
    infoModule :: Maybe String,
    infoKind :: String,
    infoDisplay :: String
  }
  deriving (Eq, Show)

data InspectionResult
  = InspectionType String String
  | InspectionInfo String [InfoEntry]
  | InspectionAmbiguous String [InfoEntry]
  | InspectionNotFound String
  | InspectionModuleNotFound String
  | InspectionRejected String
  | InspectionBrowse String Bool [InfoEntry]
  deriving (Eq, Show)

data TypeMatchQuality
  = TypeMatchExact
  | TypeMatchUsable
  deriving (Eq, Ord, Show)

data TypeMatch = TypeMatch
  { typeMatchName :: String,
    typeMatchModule :: Maybe String,
    typeMatchSignature :: String,
    typeMatchQuality :: TypeMatchQuality
  }
  deriving (Eq, Show)

-- | Replace each parsed anonymous type wildcard with a distinct implicit type
-- variable. GHC then kind-checks and quantifies those variables normally.
-- Rewriting the parsed tree preserves qualification and repeated named
-- variables; rendered source never drives this transformation.
normalizeLookupWildcards :: ParsedModule -> ParsedModule
normalizeLookupWildcards parsed =
  let source = pm_parsed_source parsed
      occupied =
        Set.fromList $
          everything (++) (mkQ [] (\name -> [occNameString (rdrNameOcc name)])) source
   in parsed
        { pm_parsed_source =
            evalState (everywhereM (mkM replaceWildcard) source) (0, occupied)
        }
  where
    replaceWildcard ::
      HsType GhcPs ->
      State (Int, Set.Set String) (HsType GhcPs)
    replaceWildcard (HsWildCardTy _) = do
      (index, occupied) <- get
      let (nextIndex, name) = freshName occupied index
      put (nextIndex, Set.insert name occupied)
      pure $
        HsTyVar
          noAnn
          NotPromoted
          (noLocA (mkRdrUnqual (mkTyVarOcc name)))
    replaceWildcard other = pure other

    freshName occupied index =
      let candidate = "__lookup_w" ++ show index
       in if Set.member candidate occupied
            then freshName occupied (index + 1)
            else (index + 1, candidate)

-- | Match a checked lookup type against every value in the exact reader
-- environment of the inspection module. Matching is entirely in memory:
-- callers compile the query once, never once per candidate.
searchTypeMatches :: (GhcMonad m) => GlobalRdrEnv -> Name -> Type -> m [TypeMatch]
searchTypeMatches rdrEnv queryBinder query = do
  let names =
        filter (/= queryBinder) $
          nubBy (==) (map greName (globalRdrEnvElts rdrEnv))
  things <- fmap catMaybes $ forM names $ \name -> do
    found <- lookupName name
    pure $ case found of
      Just (AnId identifier) -> Just (name, idType identifier)
      _ -> Nothing
  pure . sortOn matchKey . catMaybes $
    [ toMatch name candidate <$> matchQuality query candidate
    | (name, candidate) <- things
    ]
  where
    matchQuality expected candidate
      | eqType expected candidate = Just TypeMatchExact
      | matchesEitherDirection expected candidate = Just TypeMatchUsable
      | otherwise = Nothing

    matchesEitherDirection left right =
      let (_, leftPredicates, leftBody) = tcSplitSigmaTy left
          (_, rightPredicates, rightBody) = tcSplitSigmaTy right
       in null leftPredicates
            && null rightPredicates
            && ( isJust (tcMatchTy leftBody rightBody)
                   || isJust (tcMatchTy rightBody leftBody)
               )

    toMatch name candidate quality =
      TypeMatch
        { typeMatchName = occNameString (nameOccName name),
          typeMatchModule = moduleNameString . moduleName <$> nameModule_maybe name,
          typeMatchSignature =
            renderWithContext defaultSDocContext (ppr candidate),
          typeMatchQuality = quality
        }

    matchKey result =
      ( typeMatchQuality result,
        typeMatchName result,
        typeMatchModule result,
        typeMatchSignature result
      )

runInspection ::
  HscEnv ->
  GlobalRdrEnv ->
  Map.Map String String ->
  [InspectionRequest] ->
  IO [InspectionResult]
runInspection hscEnv rdrEnv capturedTypes requests = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession hscEnv
    snd <$> foldM inspect (0 :: Int, []) requests
  where
    inspect (typeIndex, results) request = case request of
      InspectTypeOf expression ->
        let binder = "__tidepool_inspect_" ++ show typeIndex
         in case Map.lookup binder capturedTypes of
              Just display -> pure (typeIndex + 1, results ++ [InspectionType expression display])
              Nothing -> liftIO (ioError (userError ("inspection module did not expose " ++ binder)))
      InspectNameInfo query -> do
        result <- inspectName rdrEnv query
        pure (typeIndex, results ++ [result])
      InspectModule moduleName expanded -> do
        result <- inspectModule moduleName expanded
        pure (typeIndex, results ++ [result])

inspectName :: (GhcMonad m) => GlobalRdrEnv -> String -> m InspectionResult
inspectName rdrEnv query = do
  let names =
        [ greName gre
        | gre <- globalRdrEnvElts rdrEnv,
          any (matchesQuery query) (greRdrNames gre)
        ]
  entries <- entriesFor (nubBy (==) names)
  let preferred =
        if any ((== "type") . infoKind) entries
          then filter ((/= "constructor") . infoKind) entries
          else entries
  pure $ case preferred of
    [] -> InspectionNotFound query
    [_] -> InspectionInfo query preferred
    _ -> InspectionAmbiguous query preferred

inspectModule :: (GhcMonad m) => String -> Bool -> m InspectionResult
inspectModule requested expanded = handleSourceError
  (\_ -> pure (InspectionModuleNotFound requested))
  $ do
    mdl <- findModule (mkModuleName requested) Nothing
    resolvedInfo <- getModuleInfo mdl
    let names = maybe [] modInfoExports resolvedInfo
    rawEntries <- browseEntries expanded names
    let entries = nubBy sameDisplay (sortOn entryKey rawEntries)
    pure (InspectionBrowse (moduleNameString (moduleName mdl)) expanded entries)
  where
    sameDisplay left right = infoDisplay left == infoDisplay right
    entryKey entry = (infoName entry, infoKind entry, infoDisplay entry)

entriesFor :: (GhcMonad m) => [Name] -> m [InfoEntry]
entriesFor names = fmap concat $ forM names $ \name -> do
  found <- getInfo False name
  pure $ case found of
    Nothing -> []
    Just (thing, _fixity, _instances, _families, _extra) ->
      let display =
            renderWithContext
              defaultSDocContext
              (pprTyThingInContext showEverything thing)
          definingModule = moduleNameString . moduleName <$> nameModule_maybe name
       in [ InfoEntry
              { infoName = occNameString (nameOccName name),
                infoModule = definingModule,
                infoKind = thingKind thing,
                infoDisplay = display
              }
          ]

browseEntries :: (GhcMonad m) => Bool -> [Name] -> m [InfoEntry]
browseEntries expanded names = do
  found <- fmap catMaybes $ forM names $ \name -> do
    thing <- lookupName name
    pure (fmap (\value -> (name, value)) thing)
  let exportedNames = map fst found
      visible =
        if expanded
          then found
          else filter (not . hasExportedParent exportedNames . snd) found
  pure $ flip map visible $ \(name, thing) ->
    let document =
          if expanded
            then pprTyThing showEverything thing
            else pprTyThingInContext showEverything thing
        display = renderWithContext defaultSDocContext document
        definingModule = moduleNameString . moduleName <$> nameModule_maybe name
     in InfoEntry
          { infoName = occNameString (nameOccName name),
            infoModule = definingModule,
            infoKind = thingKind thing,
            infoDisplay = display
          }
  where
    hasExportedParent exported thing = case tyThingParent_maybe thing of
      Just parent -> getName parent `elem` exported
      Nothing -> False

matchesQuery :: String -> RdrName -> Bool
matchesQuery query reader = case reader of
  Unqual occurrence -> qualifier == Nothing && occNameString occurrence == wanted
  Qual alias occurrence -> qualifier == Just (moduleNameString alias) && occNameString occurrence == wanted
  Orig _ _ -> False
  Exact _ -> False
  where
    (qualifier, wanted) = case break (== '.') (reverse query) of
      (reversedOccurrence, []) -> (Nothing, reverse reversedOccurrence)
      (reversedOccurrence, _ : reversedQualifier) ->
        (Just (reverse reversedQualifier), reverse reversedOccurrence)

showEverything :: ShowSub
showEverything = ShowSub ShowIface ShowForAllWhen

thingKind :: TyThing -> String
thingKind thing = case thing of
  AnId identifier
    | isJust (isClassOpId_maybe identifier) -> "class-method"
    | isRecordSelector identifier -> "record-selector"
    | otherwise -> "value"
  AConLike _ -> "constructor"
  ATyCon _ -> "type"
  ACoAxiom _ -> "coercion"

-- | Private V2 batch receipt. The outer list is @['TPINSP002', results]@.
encodeInspectionResults :: [InspectionResult] -> BS.ByteString
encodeInspectionResults results =
  toStrictByteString $
    encodeListLen 2
      <> encodeString "TPINSP002"
      <> encodeListLen (fromIntegral (length results))
      <> foldMap encodeResult results
  where
    encodeResult inspection = case inspection of
      InspectionType expression display ->
        encodeListLen 3 <> encodeString "Type" <> text expression <> text display
      InspectionInfo query entries ->
        encodeListLen 3 <> encodeString "Info" <> text query <> encodeEntries entries
      InspectionAmbiguous query entries ->
        encodeListLen 3 <> encodeString "Ambiguous" <> text query <> encodeEntries entries
      InspectionNotFound query ->
        encodeListLen 2 <> encodeString "NotFound" <> text query
      InspectionModuleNotFound moduleName ->
        encodeListLen 2 <> encodeString "ModuleNotFound" <> text moduleName
      InspectionRejected diagnostic ->
        encodeListLen 2 <> encodeString "Rejected" <> text diagnostic
      InspectionBrowse moduleName expanded entries ->
        encodeListLen 4
          <> encodeString "Browse"
          <> text moduleName
          <> encodeBool expanded
          <> encodeEntries entries
    encodeEntries entries =
      encodeListLen (fromIntegral (length entries)) <> foldMap encodeEntry entries
    encodeEntry entry =
      encodeListLen 4
        <> text (infoName entry)
        <> maybe encodeNull text (infoModule entry)
        <> text (infoKind entry)
        <> text (infoDisplay entry)
    text = encodeString . T.pack
