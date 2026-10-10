{-# LANGUAGE OverloadedStrings #-}

-- Schema regressions start from a genuine producer-emitted exact context.
-- They mutate retained evidence; this module never issues module certificates.
module ExactScopeV9Test (exactScopeChecks, nativeOriginChecks, candidateCanonicalChecks) where

import CodecFixtureSupport
  ( PurposeCodecCase(..), readPurposeCodecFixture, readExpressionItemCodecFixture
  , ScopeCodecFixture, ScopeCodecField(..), ScopeCodecAcquisition(..), readScopeCodecFixture
  , scopeCodecField, scopeCodecAcquisition, replaceScopeCodecField, scopeCodecTerm )
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, bracket, finally, try)
import Control.Monad (forM_, unless, when)
import Crypto.Hash.SHA256 qualified as SHA256
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.List (find, isInfixOf)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Numeric (showHex)
import System.Directory (doesFileExist, removeFile)
import System.FilePath (takeDirectory)
import System.IO (hClose, openBinaryTempFile)
import System.IO.Error (isDoesNotExistError, ioeGetFileName, ioeGetErrorString)
import Tidepool.CheckedCell
  ( CellExpressionPlan(..), ExpressionLiftPlan(..), encodeCellExpressionPlan )
import Tidepool.EffectSchema (NominalHead(..))
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.ExactScope
import Tidepool.ModuleCandidates
import Tidepool.Session (Generation(..))

data ScopeFixtureKind = ExecutableOriginalFixture | NativeDeclarationFixture

exactScopeChecks :: FilePath -> IO ()
exactScopeChecks = exactScopeChecksFor ExecutableOriginalFixture

exactScopeChecksFor :: ScopeFixtureKind -> FilePath -> IO ()
exactScopeChecksFor kind manifest = do
  originalBytes <- BS.readFile manifest
  (scope,fixture) <- readScopeCodecFixture manifest
  fields <- values (scopeCodecTerm fixture)
  unless (encode (scopeCodecTerm fixture) == originalBytes)
    (fail "current mutation adapter changed the genuine exact-scope envelope")
  case scopeCodecAcquisition fixture of
    Just FreshCodecInputs -> pure ()
    Just (ContinueCodecOriginals _) -> pure ()
    Nothing -> fail "production exact-scope issuer emitted an unknown acquisition variant"
  unless (Map.keysSet (scopeInterfaceEvidence scope) == Set.fromList
      [(exactUnit iface,exactModule iface) | (iface,_,_) <- scopeInterfaces scope])
    (fail "genuine exact-scope context lost its typed interface evidence")
  let sourceOriginals = scopeSourceOriginalInterfaces scope
      expectedSourceOwners = Map.keysSet (Map.filter
        (isSourceOriginal . canonicalOrigin) (scopeModuleInterfaceProofs scope))
  unless (Map.keysSet sourceOriginals == expectedSourceOwners)
    (fail "source executable selection admitted native declarations or lost source originals")
  let proofs = scopeModuleInterfaceProofs scope
      selectedOwner key proof = case kind of
        ExecutableOriginalFixture -> any
          (\product' -> (originalUnit product',originalModule product') == key) (scopeProducts scope)
        NativeDeclarationFixture -> case canonicalOrigin proof of
          NativeAuthoredDeclaration _ -> True
          SourceOriginal _ -> False
      candidates = [(key,proof) | (key,proof) <- Map.toAscList proofs
        , selectedOwner key proof
        , Just _ <- [canonicalCoreArtifact proof]
        , not (Map.null (canonicalRequirements proof))]
  (key,proof) <- case candidates of
    value : _ -> pure value
    [] -> fail "exact-scope schema fixture requires a genuine native owner with canonical Core and home dependency"
  unless (fst key `Set.member` canonicalHomeUnits proof
      && canonicalRequirements proof == Map.fromList
        [(owner, exactSha256 iface) | owner <- Map.keys (canonicalRequirements proof)
          , (iface,_,_) <- scopeInterfaces scope
          , (exactUnit iface,exactModule iface) == owner])
    (fail "genuine exact-scope context lost full-unit census or exact requirement seals")
  core <- maybe (fail "genuine canonical Core disappeared") pure (canonicalCoreArtifact proof)
  coreBytes <- BS.readFile (canonicalCorePath core)
  unless (sha coreBytes == canonicalCoreSha256 core)
    (fail "genuine exact-scope fixture has substituted Core bytes")
  interfaces <- values (scopeCodecField ScopeCodecInterfaces fixture)
  selected <- maybe (fail "genuine native interface row missing") pure
    (find (matches key) interfaces)
  selectedFields <- row 8 selected
  role <- row 5 (selectedFields !! 7)
  certificateBytes <- BS.readFile (canonicalCertificatePath proof)
  certificateTerm <- decode certificateBytes
  certificateFields <- row 13 certificateTerm
  let replaced value = scopeCodecTerm (replaceScopeCodecField ScopeCodecInterfaces
        (TList (map (replaceOwner key value) interfaces)) fixture)
      evidence value = replaced (TList (replace 7 value selectedFields))
      badCertificate expected value = withTemporary (takeDirectory manifest) "exact-scope-cert.cbor" $ \path -> do
        let bytes = encode value
            replacement = TList (replace 2 (TString (T.pack (sha bytes)))
              (replace 1 (TString (T.pack path)) role))
        BS.writeFile path bytes
        refuse manifest expected (evidence replacement)
  forM_ ["2","4","6","7","8","9","10","11"] $ \version ->
    refuse manifest "unsupported exact scope" (scopeCodecTerm (replaceScopeCodecField ScopeCodecVersion (TString version) fixture))
  forM_ [8,9] $ \count ->
    refuse manifest "unsupported exact scope" (TList (take count fields))
  checkedPurposeCases manifest fixture
  originRoleCases manifest scope fixture interfaces
  refuse manifest "invalid exact scope row" (replaced (TList (take 7 selectedFields)))
  forM_ [TNull,TList [],TList [TString "other"]
        ,TList (tail role),TList (role ++ [TNull])] $ \invalid ->
    refuse manifest "" (evidence invalid)
  -- The source-original fixture must retain its executable product. Its role
  -- cannot be downgraded to metadata without breaking the native owner closure.
  -- Native declarations retain their separate type-only original interface.
  case canonicalOrigin proof of
    SourceOriginal _ -> do
      unless (any (\product' -> (originalUnit product',originalModule product') == key)
          (scopeProducts scope)) (fail "source-original fixture lost its native product")
      forM_ [TList [TString "join"],TList [TString "value"]] $ \invalid ->
        refuse manifest "incomplete or conflicting exact owner closure" (evidence invalid)
    NativeAuthoredDeclaration _ ->
      unless (null (scopeProducts scope)) (fail "type-only declaration fixture acquired native products")
  refuse manifest "" (evidence (TList (replace 3 TNull role)))
  refuse manifest "" (evidence (TList (replace 4 TNull role)))
  refuse manifest "canonical module certificate changed"
    (evidence (TList (replace 2 differentSHA role)))
  forM_ [(0,TString "TPFINALMODULE_OLD","unsupported canonical module certificate")
        ,(1,TInt 2,"unsupported canonical module certificate")
        ,(2,TString "another-profile","unsupported canonical module certificate")
        ,(3,differentSHA,"canonical module certificate differs")
        ,(5,TString "another-home-unit","invalid canonical module requirement inventory")
        ,(6,TString "Another.Module",case canonicalOrigin proof of
            SourceOriginal _ -> "canonical module certificate differs"
            NativeAuthoredDeclaration _ -> "native canonical origin differs")
        ,(7,TString (T.replicate 64 "0"),"invalid canonical digest")
        ,(8,differentSHA,"canonical module certificate differs")
        ,(9,differentSHA,"canonical module certificate differs")
        ,(10,TNull,"canonical module certificate differs")] $ \(index,value,expected) ->
    badCertificate expected (TList (replace index value certificateFields))
  badCertificate "invalid exact scope row" (TList (take 12 certificateFields))
  forM_ [TNull,TList [],TList [TString "unknown-origin"]
        ,TList [TString "source-original",TInt 1]
        ,TList [TString "native-authored-declaration"]] $ \invalid ->
    badCertificate "" (TList (replace 12 invalid certificateFields))
  badCertificate "native canonical origin differs"
    (TList (replace 12 (TList [TString "native-authored-declaration",TInt 0]) certificateFields))
  homes <- values (certificateFields !! 4)
  -- Large inventories still reach their semantic checks. These alterations
  -- remain invalid: passing a former cache-size boundary grants no authority.
  let manyHomes = map TString . Set.toAscList . Set.fromList $
        [home | TString home <- homes] ++
        [T.pack ("inventory-unit-" ++ show index) | index <- [1 .. 129 :: Int]]
      manyRequirements = map (\name -> TList
        [TString (T.pack (fst key)),TString name,differentSHA]) . Set.toAscList . Set.fromList $
        [T.pack ("InventoryMissing" ++ show index) | index <- [1 .. 129 :: Int]]
  badCertificate "canonical module certificate differs"
    (TList (replace 3 differentSHA (replace 4 (TList manyHomes) certificateFields)))
  badCertificate "canonical module requirements differ"
    (TList (replace 11 (TList manyRequirements) certificateFields))
  forM_ [TList [],TList (homes ++ homes)] $ \invalid ->
    badCertificate "invalid complete home unit inventory" (TList (replace 4 invalid certificateFields))
  badCertificate "invalid canonical module requirement inventory"
    (TList (replace 4 (TList [TString "another-home-unit"]) certificateFields))
  let selfRequirement = TList [TString (T.pack (fst key)),TString (T.pack (snd key)),differentSHA]
      foreignRequirement = TList [TString "another-home-unit",TString "Missing",differentSHA]
      missingRequirement = TList [TString (T.pack (fst key)),TString "Missing",differentSHA]
  forM_ [selfRequirement,foreignRequirement] $ \invalid ->
    badCertificate "invalid canonical module requirement inventory"
      (TList (replace 11 (TList [invalid]) certificateFields))
  badCertificate "canonical module requirements differ"
    (TList (replace 11 (TList [missingRequirement]) certificateFields))
  requirements <- values (certificateFields !! 11)
  first <- row 3 (head requirements)
  badCertificate "canonical module requirements differ" (TList (replace 11 (TList []) certificateFields))
  badCertificate "canonical module requirements differ" (TList (replace 11 (TList
    (TList (replace 2 differentSHA first) : tail requirements)) certificateFields))
  forM_ [(2,"canonical interface"),(5,"canonical package imports")] $ \(index,name) ->
    withTemporary (takeDirectory manifest) name $ \path -> do
      BS.writeFile path "substituted payload"
      refuse manifest "canonical module interface or package imports changed"
        (replaced (TList (replace index (TString (T.pack path)) selectedFields)))
  -- Raw proof validation and metadata promotion do not demand Core. A fresh
  -- exact request snapshots every promised companion, and executable promotion
  -- demands the independently validated bytes.
  forM_ [True,False] $ \missing ->
    withTemporary (takeDirectory manifest) "exact-scope-core" $ \path -> do
      if missing then removeFile path else BS.writeFile path "substituted Core"
      metadata <- validateCanonicalInterfaceProof scope key
        (canonicalCertificatePath proof) (canonicalCertificateSha256 proof)
        (Just (path,canonicalCoreSha256 core)) >>= either fail pure
      promoted <- captureCanonicalProofs Nothing [(metadata,MetadataInterfaceUse)]
      unless (length promoted == 1) (fail "metadata-only proof promotion lost its owner")
      demanded <- try (captureCanonicalProofs Nothing [(metadata,ExecutableInterfaceUse)])
        :: IO (Either IOException [CanonicalInterfaceProof])
      case demanded of
        Left failure
          | missing && isDoesNotExistError failure && ioeGetFileName failure == Just path -> pure ()
          | not missing && ioeGetErrorString failure == "admitted defining Core changed during capture: " ++ path -> pure ()
        Left failure -> fail ("executable Core refusal came from another boundary: " ++ show failure)
        Right _ -> fail "executable proof promotion admitted unavailable Core"
      let alteredRole = TList (replace 3 (TString (T.pack path)) role)
      refuse manifest (if missing then path else "admitted defining Core changed during capture")
        (evidence alteredRole)
  -- The already admitted request keeps its captured bytes after disk drift;
  -- a fresh acquisition sees the changed input and refuses it.
  (do BS.writeFile (canonicalCorePath core) "changed after request capture"
      retained <- canonicalProofOriginalBytes proof (canonicalCorePath core) (canonicalCoreSha256 core)
      unless (retained == coreBytes) (fail "admitted scope reopened original Core after disk drift")
      readExactScope manifest >>= \case
        Left reason | "admitted defining Core changed during capture" `isInfixOf` reason -> pure ()
        Left reason -> fail ("fresh snapshot refusal came from another boundary: " ++ reason)
        Right _ -> fail "fresh exact scope admitted changed original Core")
    `finally` BS.writeFile (canonicalCorePath core) coreBytes
  unchanged <- BS.readFile manifest
  unless (unchanged == originalBytes) (fail "exact-scope schema checks changed the producer manifest")
  putStrLn "exact scope: genuine native context, typed roles, certificate seals, metadata promotion and captured Core checks passed"
  where
    -- A fixed altered seal for negative wire cases.
    differentSHA = TString (T.replicate 64 "f")

-- Mutate only request-purpose syntax around the genuine immutable owner
-- closure. These parsing checks do not issue executable admission receipts.
checkedPurposeCases :: FilePath -> ScopeCodecFixture -> IO ()
checkedPurposeCases manifest fixture = do
  let paths = [takeDirectory manifest]
      readPurpose purpose = readPurposeCodecFixture (takeDirectory manifest) paths purpose >>= \case
        TList values -> pure values
        _ -> fail "production purpose encoder returned another record"
  cell <- readPurpose CodecCellPurpose
  item <- readPurpose CodecItemPurpose
  inspection <- readPurpose CodecInspectionPurpose
  let empty = TList []
      acceptsCell purpose = case purpose of ExactCellPurpose _ _ -> True; _ -> False
      acceptsItem purpose = case purpose of ExactItemPurpose _ _ -> True; _ -> False
      acceptsInspection purpose = case purpose of ExactInspectionPurpose _ _ -> True; _ -> False
      envelope purpose = scopeCodecTerm (replaceScopeCodecField ScopeCodecPurpose purpose fixture)
  forM_ [("cell-check2",cell),("cell-program1",cell),("checked-item3",item),("checked-display3",item),("host-activation-input1",item)] $
    \(legacy,purpose) -> refuse manifest "" (envelope (TList (TString legacy : tail purpose)))
  noPurpose <- readCandidate manifest (envelope TNull) >>= either fail pure
  unless (scopePurpose noPurpose == NoCheckedPurpose && scopeIncludePaths noPurpose == Nothing
      && null (scopeValueInterfaces noPurpose))
    (fail "null checked purpose acquired stage or search-input authority")
  forM_ [(cell,acceptsCell),(item,acceptsItem),(inspection,acceptsInspection)] $
    \(purpose,accepts) -> do
      accepted <- readCandidate manifest (envelope (TList purpose)) >>= either fail pure
      unless (accepts (scopePurpose accepted) && scopeIncludePaths accepted == Just paths)
        (fail "checked purpose lost its exclusive stage or ordered search inputs")
      refuse manifest "" (envelope (TList (purpose ++ [TList inspection])))
      refuse manifest "" (envelope (TList (take (length purpose - 1) purpose)))
  forM_ [(ExpressionPure,"pure"),(ExpressionEffectful,"effectful")] $ \(liftPlan,liftName) -> do
    let plan = CellExpressionPlan "__tidepool_cell_expr_0" liftPlan "Int"
          [NominalHead "ghc-prim" "GHC.Types" "Int"]
        expressionBytes = toStrictByteString (encodeCellExpressionPlan plan)
    encoded <- decode expressionBytes
    issuedExpression <- readExpressionItemCodecFixture (takeDirectory manifest) paths expressionBytes
    issuedExpressionFields <- row 20 issuedExpression
    let expressionPurpose value = TList (replace 10 value issuedExpressionFields)
    accepted <- readCandidate manifest (envelope issuedExpression) >>= either fail pure
    case scopeCheckedItem accepted of
      Just admitted | itemExpressionLift admitted == Just liftName -> pure ()
      _ -> fail "compiler expression encoder and exact-item consumer disagree"
    expressionFields <- row 4 encoded
    forM_ [TList (expressionFields ++ [empty]),TList (take 3 expressionFields)
      ,TList (take 2 expressionFields ++ [TString "opaque"] ++ drop 2 expressionFields)
      ,TList (replace 0 (TString "") expressionFields)
      ,TList (replace 1 (TString "unknown") expressionFields)
      ,TList (replace 3 (TList [TList [TString "ghc-prim",TString "GHC.Types"]]) expressionFields)] $ \malformed ->
        refuse manifest "" (envelope (expressionPurpose malformed))
    refuse manifest "" (envelope (TList (replace 10 encoded item)))
  refuse manifest "" (envelope (TList [TList cell,TList inspection]))

-- The positive native origin must come from the protected authored producer,
-- independently of the ordinary machine-code source module fixture.
nativeOriginChecks :: FilePath -> IO ()
nativeOriginChecks manifest = do
  scope <- readExactScope manifest >>= either fail pure
  unless (any (\proof -> case canonicalOrigin proof of
      NativeAuthoredDeclaration _ -> True
      SourceOriginal _ -> False) (Map.elems (scopeModuleInterfaceProofs scope)))
    (fail "exact-scope origin fixture lacks a genuine native authored declaration")
  exactScopeChecksFor NativeDeclarationFixture manifest

-- A role is a claim about authenticated producer origin, not permission to
-- relabel a source proof.
originRoleCases :: FilePath -> ExactScope -> ScopeCodecFixture -> [Term] -> IO ()
originRoleCases manifest scope fixture interfaces = do
  let proofs = Map.toAscList (scopeModuleInterfaceProofs scope)
  forM_ proofs $ \(key,proof) -> do
    selected <- maybe (fail "origin proof lacks its genuine interface row") pure
      (find (matches key) interfaces)
    selectedFields <- row 8 selected
    role <- row 5 (selectedFields !! 7)
    let (expected,contradictory) = case canonicalOrigin proof of
          SourceOriginal _ -> ("module","native-declaration")
          NativeAuthoredDeclaration _ -> ("native-declaration","module")
    unless (head role == TString expected)
      (fail "genuine scope role differs from canonical origin")
    let changed = TList (replace 7 (TList (replace 0 (TString contradictory) role)) selectedFields)
    refuse manifest "canonical module origin differs from its exact interface role"
      (scopeCodecTerm (replaceScopeCodecField ScopeCodecInterfaces
        (TList (map (replaceOwner key changed) interfaces)) fixture))
    case canonicalOrigin proof of
      SourceOriginal _ -> pure ()
      NativeAuthoredDeclaration (Generation generation) -> do
        certificate <- BS.readFile (canonicalCertificatePath proof) >>= decode >>= row 13
        let changedOrigin expected origin = withTemporary (takeDirectory manifest) "native-origin-cert.cbor" $ \path -> do
              let bytes = encode (TList (replace 12 origin certificate))
                  alteredRole = TList (replace 2 (TString (T.pack (sha bytes)))
                    (replace 1 (TString (T.pack path)) role))
                  alteredRow = TList (replace 7 alteredRole selectedFields)
              BS.writeFile path bytes
              refuse manifest expected (scopeCodecTerm (replaceScopeCodecField ScopeCodecInterfaces
                (TList (map (replaceOwner key alteredRow) interfaces)) fixture))
            otherGeneration = if generation == 1 then TInt 2 else TInt 1
        changedOrigin "canonical module origin differs from its exact interface role"
          (TList [TString "source-original",TList []])
        changedOrigin "native canonical origin differs from its reserved identity"
          (TList [TString "native-authored-declaration",otherGeneration])

-- The worker offer and exact context are both emitted by their real Rust owners.
-- This checks durable promotion without minting or rewriting a certificate.
candidateCanonicalChecks :: FilePath -> FilePath -> IO ()
candidateCanonicalChecks manifest candidateManifest = do
  scope <- readExactScope manifest >>= either fail pure
  bytes <- BS.readFile candidateManifest
  fields <- decode bytes >>= row 7
  candidates <- readModuleCandidatesWithGraphs (scopeExecutionGraphs scope) candidateManifest
    >>= either fail pure
  when (null candidates) (fail "canonical candidate fixture must offer a native owner")
  let keys = Set.fromList [(candidateUnit candidate,candidateModule candidate) | candidate <- candidates]
      candidateInterfaces = [(ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
        (candidateInterface candidate) (candidateInterfaceSha256 candidate)
        (candidateInterfaceRequirements candidate),candidatePackageImports candidate,
        candidatePackageImportsSha256 candidate) | candidate <- candidates]
      selected = candidateInterfaces ++
        [(iface,path,seal) | (iface,path,seal) <- scopeInterfaces scope
          , (exactUnit iface,exactModule iface) `Set.notMember` keys]
      refusePromotion candidate = validateCandidateCanonicalInterfaceProof (scopeProducerSha256 scope) selected candidate >>= \result ->
        case result of
          Left _ -> pure ()
          Right _ -> fail "candidate promotion admitted changed source or interface evidence"
  forM_ candidates $ \candidate -> do
    proof <- validateCandidateCanonicalInterfaceProof (scopeProducerSha256 scope) selected candidate >>= either fail pure
    let descriptor = candidateModuleInterface candidate
    unless (Map.keys (canonicalRequirements proof) == candidateInterfaceRequirements candidate
        && canonicalCertificatePath proof == candidateCertificatePath descriptor
        && canonicalCertificateSha256 proof == candidateCertificateSha256 descriptor
        && ((\core -> (canonicalCorePath core,canonicalCoreSha256 core)) <$> canonicalCoreArtifact proof)
          == Just (candidateCoreDescriptor descriptor))
      (fail "candidate promotion changed its canonical descriptor or dependency map")
    refusePromotion candidate {candidateSourceSha256 = alterSeal (candidateSourceSha256 candidate)}
    refusePromotion candidate {candidateProducerSha256 = alterSeal (candidateProducerSha256 candidate)}
    refusePromotion candidate {candidateInterfaceSha256 = alterSeal (candidateInterfaceSha256 candidate)}
    refusePromotion candidate {candidatePackageImportsSha256 = alterSeal (candidatePackageImportsSha256 candidate)}
    refusePromotion candidate {candidateInterfaceRequirements =
      [(candidateUnit candidate,"Missing.Canonical.Dependency")]}
  let refuseWire term = withTemporary (takeDirectory candidateManifest) "invalid-candidate.cbor" $ \path -> do
        BS.writeFile path (encode term)
        readModuleCandidatesWithGraphs (scopeExecutionGraphs scope) path >>= \result -> case result of
          Left _ -> pure ()
          Right _ -> fail "candidate decoder admitted invalid canonical framing"
  forM_ ["2","4","6","7","8","9"] $ \version ->
    refuseWire (TList (replace 1 (TString version) fields))
  refuseWire (TList (take 6 fields))
  candidateRows <- values (fields !! 4)
  firstRow <- row 16 (head candidateRows)
  moduleRole <- row 5 (firstRow !! 15)
  let changedRow value = TList (replace 4 (TList (value : tail candidateRows)) fields)
  refuseWire (changedRow (TList (take 14 firstRow)))
  forM_ [TNull,TList [TString "join"],TList [TString "value"]
      ,TList (replace 3 TNull moduleRole),TList (replace 4 TNull moduleRole)] $ \invalid ->
    refuseWire (changedRow (TList (replace 15 invalid firstRow)))
  unchanged <- BS.readFile candidateManifest
  unless (unchanged == bytes) (fail "candidate checks changed the producer offer")
  putStrLn "canonical candidates: genuine worker10 offer, exact promotion and legacy refusal checks passed"
  where
    alterSeal [] = error "genuine candidate fixture has no seal"
    alterSeal (first:rest) = (if first == '0' then '1' else '0') : rest

refuse :: FilePath -> String -> Term -> IO ()
refuse manifest expected term = do
  result <- readCandidate manifest term
  case result of
    Left message | expected `isInfixOf` message -> pure ()
                 | otherwise -> fail ("exact-scope schema refusal came from another boundary: " ++ message)
    Right _ -> fail "exact-scope schema admitted substituted evidence"

readCandidate :: FilePath -> Term -> IO (Either String ExactScope)
readCandidate manifest term = withTemporary (takeDirectory manifest) "exact-scope.cbor" $ \path -> do
  BS.writeFile path (encode term)
  readExactScope path

withTemporary :: FilePath -> String -> (FilePath -> IO a) -> IO a
withTemporary directory name action = bracket
  (do (path,handle) <- openBinaryTempFile directory name
      hClose handle
      pure path)
  (\path -> do
    present <- doesFileExist path
    when present (removeFile path))
  action

row :: Int -> Term -> IO [Term]
row size (TList fields) | length fields == size = pure fields
row _ _ = fail "genuine exact-scope fixture has another row shape"

values :: Term -> IO [Term]
values (TList fields) = pure fields
values _ = fail "genuine exact-scope fixture has another array shape"

matches :: (String,String) -> Term -> Bool
matches (unit,name) (TList (TString actualUnit : TString actualName : _)) =
  actualUnit == T.pack unit && actualName == T.pack name
matches _ _ = False

replaceOwner :: (String,String) -> Term -> Term -> Term
replaceOwner key replacement current | matches key current = replacement
                                    | otherwise = current

replace :: Int -> a -> [a] -> [a]
replace index replacement = zipWith (\ordinal current ->
  if ordinal == index then replacement else current) [0..]

encode :: Term -> BS.ByteString
encode = toStrictByteString . encodeTerm

decode :: BS.ByteString -> IO Term
decode bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
  Right (remaining,value) | BL.null remaining -> pure value
  _ -> fail "genuine exact-scope fixture has invalid or trailing CBOR"

sha :: BS.ByteString -> String
sha = concatMap (\byte -> let rendered = showHex byte ""
  in replicate (2-length rendered) '0' ++ rendered) . BS.unpack . SHA256.hash
