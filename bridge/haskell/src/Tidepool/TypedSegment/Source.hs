-- | Parsed-source lowering for compiler-owned typed effect segments.
-- Authored nested do expressions remain opaque to this rewrite.
module Tidepool.TypedSegment.Source
  ( rewriteParsedSegmentRoot, typedSegmentInputOccurrences
  , ParsedSegmentDemand, prepareSegmentDemand, segmentDemandMatchesSource
  , ParsedModuleDemand, prepareModuleDemand, moduleDemandMatchesSource, moduleDemandCapturedImports
  , moduleInputOccurrences, moduleImportRequests, segmentModuleDemand, annotateModuleDemand, ObservationOperand(..), validateObservationContribution
  , SegmentDemandScope, newSegmentDemandScope, updateSegmentDemandScope, associateSegmentDemandName
  ) where

import Control.Exception (throwIO)
import Control.Monad (forM, foldM)
import qualified Data.ByteString as BS
import Data.Data (Data)
import Data.Generics (everything, mkQ)
import Data.List (intersect, nub, sort, sortOn)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC (ParsedSource)
import GHC.Parser.Annotation (getLocA)
import GHC.Data.Strict qualified as Strict
import GHC.Driver.Session (DynFlags, GeneralFlag(Opt_HelpfulErrors,Opt_DeferOutOfScopeVariables), gopt_set, gopt_unset, xopt)
import GHC.Driver.Env (HscEnv, hsc_NC, hscUpdateFlags)
import GHC.Iface.Env (allocateGlobalBinder)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Rename.Module (findSplice, rnSrcDecls)
import GHC.Rename.Names (rnImports)
import GHC.Tc.Types (TcGblEnv(..))
import GHC.Tc.Utils.Monad (initTcInteractive, getGblEnv, setGblEnv, failM)
import GHC.Types.Name (Name)
import GHC.Parser.Header (mkPrelImports)
import GHC.Driver.Ppr (showPpr)
import GHC.Hs
import GHC.Types.Basic (Boxity (Boxed), DoPmc (SkipPmc), GenReason (OtherExpansion), Origin (Generated))
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString, delFromOccEnv)
import GHC.Types.Name.Reader
  ( RdrName, isUnqual, mkRdrQual, mkRdrUnqual, rdrNameOcc
  , GlobalRdrEnv, mkGlobalRdrEnv, mkLocalVanillaGRE, Parent(NoParent)
  , plusGlobalRdrEnv, shadowNames, extendGlobalRdrEnv )
import GHC.Types.SrcLoc (GenLocated (L), Located, getLoc, unLoc, noSrcSpan, getBufSpan, bufSpanStart, bufSpanEnd, bufPos)
import GHC.Unit.Types (interactiveUnit, mkModule)
import GHC.Unit.Module (ModuleName, mkModuleName)
import GHC.Types.PkgQual (RawPkgQual)
import Language.Haskell.Syntax.ImpExp (IsBootInterface)
import GHC.Utils.Outputable (ppr, text, (<+>))
import Tidepool.TypedSegment.Types
import Tidepool.ExactHydration
  ( CapturedGeneratedImports, capturedGeneratedScaffoldBytes, capturedGeneratedScaffoldPath
  , capturedGeneratedImportSource, capturedGeneratedImportFlags, capturedGeneratedImportSyntax )
import Tidepool.ImportSyntax (capturedParsedSource)
import Tidepool.CheckedCell (ResolvedNativeSlots, applyNativeAnnotations)
import Tidepool.CheckedPrefixImports (parsedDeclarationOccurrences)
import Tidepool.QuasiQuoteOccurrences (quasiQuoteOccurrences)

-- Demand analysis borrows the root's already parsed, origin-bound capture.
-- Its private constructor adds the no-quasiquote proof required before the
-- demand renamer can load any imported interface. The source may subsequently
-- receive native type annotations, while the captured imports remain exact.
data ParsedModuleDemand = ParsedModuleDemand
  CapturedGeneratedImports ParsedSource

moduleDemandCapturedImports :: ParsedModuleDemand -> CapturedGeneratedImports
moduleDemandCapturedImports (ParsedModuleDemand captured _) = captured

-- Import selection consumes the same parsed source as the demand renamer.
-- Package qualifiers and SOURCE identity are preserved until the active
-- World environment resolves them; rendering cannot create an import here.
moduleImportRequests :: ParsedModuleDemand -> [(ModuleName, RawPkgQual, IsBootInterface)]
moduleImportRequests (ParsedModuleDemand _ parsed) =
  [(unLoc (ideclName declaration), ideclPkgQual declaration, ideclSource declaration)
  | L _ declaration <- hsmodImports (unLoc parsed)]

-- Typed segments add their actual plan's unique root check to the same parsed
-- module fact. A module fact alone grants no typed-item or declaration authority.
newtype ParsedSegmentDemand = ParsedSegmentDemand ParsedModuleDemand

segmentModuleDemand :: ParsedSegmentDemand -> ParsedModuleDemand
segmentModuleDemand (ParsedSegmentDemand parsed) = parsed

moduleDemandMatchesSource :: ParsedModuleDemand -> FilePath -> BS.ByteString -> Bool
moduleDemandMatchesSource (ParsedModuleDemand captured _) path bytes =
  capturedGeneratedScaffoldPath source == path && capturedGeneratedScaffoldBytes source == bytes
  where source = capturedGeneratedImportSource captured

-- Physical rendered-source boundaries are independent of user LINE pragmas.
-- These closed modes constrain an operand to its generated expression/type
-- slot; no additional top-level declaration can alter the checking scope.
data ObservationOperand
  = ObservationNoOperand
  | ObservationExpressionOperand
  | ObservationTypeOperand Int

validateObservationContribution :: Int -> ObservationOperand -> [String] -> ParsedModuleDemand
  -> Either String ()
validateObservationContribution prefix mode probes (ParsedModuleDemand _ parsed) = do
  suffix <- fmap concat $ mapM classify (hsmodDecls (unLoc parsed))
  let expected = map (False,) probes ++ case mode of
        ObservationNoOperand -> []
        ObservationExpressionOperand -> [(False,"__tidepool_inspect_0")]
        ObservationTypeOperand _ -> [(True,"__tidepool_type_search_query"),(False,"__tidepool_type_search_query")]
  if sort suffix == sort expected then Right ()
    else Left "observation operand changed its generated declaration set"
  where
    issued = Set.fromList (probes ++ case mode of
      ObservationNoOperand -> []
      ObservationExpressionOperand -> ["__tidepool_inspect_0"]
      ObservationTypeOperand _ -> ["__tidepool_type_search_query"])
    physical declaration = case getBufSpan (getLocA declaration) of
      Strict.Just span' -> Right (bufPos (bufSpanStart span'),bufPos (bufSpanEnd span'))
      Strict.Nothing -> Left "observation declaration lacks a physical source boundary"
    classify declaration = do
      (start,end) <- physical declaration
      let signatureNames = case unLoc declaration of
            SigD _ (TypeSig _ names _) -> map (rdrNameOcc . unLoc) names
            _ -> []
          declared = map occNameString (signatureNames ++ parsedDeclarationOccurrences declaration)
      if end <= prefix then
        if any (`Set.member` issued) declared then Left "observation probe collides with a protected declaration"
          else pure []
        else if start < prefix
        then Left "observation declaration crosses its protected source boundary"
        else case unLoc declaration of
          SigD _ (TypeSig _ [name] _) -> pure [(True,occNameString (rdrNameOcc (unLoc name)))]
          ValD _ binding@FunBind{} -> do
            let name = occNameString (rdrNameOcc (unLoc (fun_id binding)))
            case unLoc (mg_alts (fun_matches binding)) of
              [locatedMatch] -> do
                let match = unLoc locatedMatch
                case (unLoc (m_pats match),grhssGRHSs (m_grhss match),grhssLocalBinds (m_grhss match)) of
                  (arguments,[locatedRhs],EmptyLocalBinds _) -> case unLoc locatedRhs of
                    GRHS _ [] body -> do
                      case mode of
                        ObservationExpressionOperand | name == "__tidepool_inspect_0" ->
                          case (arguments,unLoc body) of
                            ([],HsPar {}) -> pure ()
                            _ -> Left "observation expression escaped its generated slot"
                        ObservationTypeOperand valueStart | name == "__tidepool_type_search_query" ->
                          if null arguments && start == valueStart then pure ()
                            else Left "observation type changed its generated value body"
                        _ -> if Set.member name issued && length arguments == 1 then pure ()
                          else Left "observation has an unexpected value declaration"
                      pure [(False,name)]
                    _ -> Left "observation generated binder has guards"
                  _ -> Left "observation generated binder has arguments or local declarations"
              _ -> Left "observation generated binder has extra equations"
          _ -> Left "observation operand introduced a top-level declaration"

-- Native type substitution preserves the source issuer and no-quasiquote proof.
-- The same resolved annotation product must also feed final module checking.
annotateModuleDemand :: ResolvedNativeSlots -> ParsedModuleDemand -> IO ParsedModuleDemand
annotateModuleDemand annotations (ParsedModuleDemand captured parsed) = do
  (annotated, _) <- applyNativeAnnotations annotations parsed
  -- Only declaration types participate in demand. Retain the captured header
  -- and imports by construction, independently of the annotation traversal.
  let L location original = parsed
      declarations = hsmodDecls (unLoc annotated)
  pure (ParsedModuleDemand captured (L location original {hsmodDecls = declarations}))

segmentDemandMatchesSource :: ParsedSegmentDemand -> FilePath -> BS.ByteString -> Bool
segmentDemandMatchesSource = moduleDemandMatchesSource . segmentModuleDemand

prepareModuleDemand :: CapturedGeneratedImports -> Either TypedSegmentFailure ParsedModuleDemand
prepareModuleDemand captured
  | null quotes = Right (ParsedModuleDemand captured parsed)
  | otherwise = Left (ModuleDemandQuasiQuoteRejected path (map (showPpr flags) quotes))
  where
    flags = capturedGeneratedImportFlags captured
    parsed = capturedParsedSource (capturedGeneratedImportSyntax captured)
    path = capturedGeneratedScaffoldPath (capturedGeneratedImportSource captured)
    quotes = quasiQuoteOccurrences flags parsed

prepareSegmentDemand :: TypedSegmentPlan -> CapturedGeneratedImports
  -> Either TypedSegmentFailure ParsedSegmentDemand
prepareSegmentDemand plan captured = do
  parsedModule@(ParsedModuleDemand _ parsed) <- prepareModuleDemand captured
  if roots parsed == [()]
    then Right (ParsedSegmentDemand parsedModule)
    else Left UnprovedRootAbstraction
  where
    roots parsed =
      [ () | L _ (ValD _ FunBind {fun_id=identifier}) <- hsmodDecls (unLoc parsed)
      , occNameString (rdrNameOcc (unLoc identifier)) == typedSegmentPlanRoot plan ]

-- Placeholder Names describe only the world's lexical aliases. Their exact
-- association stays with the supplied admitted binding; they never supply a type,
-- native identity or value certificate. A fixed private interactive module
-- lets NameCache share each alias across worlds without retaining per-cell
-- namespaces. The colon makes this qualifier impossible in authored syntax.
-- GHC gives each query's declarations their own module.
data SegmentDemandScope binding = SegmentDemandScope
  !GlobalRdrEnv !(Map.Map Name (String,binding))

newSegmentDemandScope :: HscEnv -> Map.Map String binding -> IO (SegmentDemandScope binding)
newSegmentDemandScope environment bindings = do
  entries <- forM (Map.toAscList bindings) $ \(alias,binding) -> do
    name <- allocateGlobalBinder (hsc_NC environment)
      (mkModule interactiveUnit (mkModuleName ":TidepoolDemand")) (mkVarOcc alias) noSrcSpan
    pure (name,(alias,binding))
  pure (SegmentDemandScope
    (mkGlobalRdrEnv [mkLocalVanillaGRE NoParent name | (name,_) <- entries])
    (Map.fromList entries))

-- An operation operand can occur through its actual qualified interface Name.
-- This adds only the reverse association; GHC still owns import and local scope.
-- The caller obtains the Name from its admitted interface, never an occurrence
-- spelling or a fabricated GRE. Repeated associations are ambiguous input facts.
associateSegmentDemandName :: Name -> String -> binding -> SegmentDemandScope binding
  -> Either TypedSegmentFailure (SegmentDemandScope binding)
associateSegmentDemandName name alias binding (SegmentDemandScope aliases bindings)
  | null alias || Map.member name bindings = Left ConflictingDemandName
  | otherwise = Right (SegmentDemandScope aliases (Map.insert name (alias,binding) bindings))

-- The successor receiver supplies only authenticated changed aliases. The
-- alias-only GRE and exact reverse associations share every untouched branch;
-- stable NameCache names avoid a namespace per logical world.
updateSegmentDemandScope :: HscEnv -> [(String,Maybe binding)] -> SegmentDemandScope binding
  -> IO (SegmentDemandScope binding)
updateSegmentDemandScope environment changes initial = foldM update initial changes
  where
    update (SegmentDemandScope aliases bindings) (alias,replacement) = do
      let occurrence = mkVarOcc alias
      name <- allocateGlobalBinder (hsc_NC environment)
        (mkModule interactiveUnit (mkModuleName ":TidepoolDemand")) occurrence noSrcSpan
      let removed = delFromOccEnv aliases occurrence
          remaining = Map.delete name bindings
      pure $ case replacement of
        Nothing -> SegmentDemandScope removed remaining
        Just binding -> SegmentDemandScope
          (extendGlobalRdrEnv removed (mkLocalVanillaGRE NoParent name))
          (Map.insert name (alias,binding) remaining)

-- GHC owns the complete module and local lexical scopes, including class
-- defaults, instance methods, constructors and record fields. All imports
-- retain their real identities and qualification. The notebook alias GREs
-- shadow imported values through the same GHC operation used by GHCi; new
-- module declarations then shadow those aliases during rnSrcDecls.
--
-- The owning admission must validate source execution policy before entry and
-- install only its actually selected source interfaces. No compiler environment
-- escapes the probe, and genuine renamer errors are never discarded.
typedSegmentInputOccurrences :: HscEnv -> SegmentDemandScope binding -> ParsedSegmentDemand
  -> IO (Either TypedSegmentFailure [(String,binding)])
typedSegmentInputOccurrences environment scope =
  moduleInputOccurrences environment scope . segmentModuleDemand

moduleInputOccurrences :: HscEnv -> SegmentDemandScope binding -> ParsedModuleDemand
  -> IO (Either TypedSegmentFailure [(String,binding)])
moduleInputOccurrences environment (SegmentDemandScope aliases bindings)
    (ParsedModuleDemand captured parsed) = do
  let flags = capturedGeneratedImportFlags captured
      probeFlags = gopt_set (gopt_unset flags Opt_HelpfulErrors) Opt_DeferOutOfScopeVariables
      probe = hscUpdateFlags (const probeFlags) environment
      syntax = unLoc parsed
      imports = hsmodImports syntax
      owner = maybe (mkModuleName "Main") unLoc (hsmodName syntax)
      allImports = mkPrelImports owner noSrcSpan (xopt LangExt.ImplicitPrelude flags) imports ++ imports
  (diagnostics,renamed) <- initTcInteractive probe $ do
    (_,_,imported,available,_,_) <- rnImports
      [(declaration,text "held-world demand") | declaration <- allImports]
    current <- getGblEnv
    let scope = plusGlobalRdrEnv aliases (shadowNames False imported aliases)
    setGblEnv (current {tcg_rdr_env=scope,tcg_imports=available}) $ do
      (group,splice) <- findSplice (hsmodDecls syntax)
      case splice of
        Just _ -> failM
        Nothing -> pure ()
      (_,result) <- rnSrcDecls group
      pure result
  pure $ case renamed of
    Nothing -> Left (InputDemandRenameRejected (showPpr probeFlags diagnostics))
    Just result ->
      let selected = everything (++) (mkQ [] occurrence) result
          -- Distinct resolved Names can share a presentation label. Only
          -- repeated occurrences of the same Name select the same input fact.
          demanded = Map.fromList
            [ (name,(alias,binding))
            | name <- selected, Just (alias,binding) <- [Map.lookup name bindings] ]
      in Right (sortOn fst (Map.elems demanded))
  where
    occurrence :: HsExpr GhcRn -> [Name]
    occurrence (HsVar _ (L _ name)) = [name]
    occurrence _ = []

rewriteParsedSegmentRoot :: GeneratedSegmentOperations -> TypedSegmentPlan -> DynFlags -> ParsedSource -> IO ParsedSource
rewriteParsedSegmentRoot operations plan flags source = case rewriteModule of
  Left failure -> throwIO failure
  Right rewrittenSource -> pure rewrittenSource
  where
    authoredNames = parsedNames source
    generatedNames = concatMap plannedNames (typedSegmentPlanItems plan)
    collisions = generatedNames `intersect` authoredNames
    rewriteModule
      | length generatedNames /= length (nub generatedNames) = Left InvalidReservedNames
      | not (null collisions) = Left InvalidReservedNames
      | otherwise = case rewriteRoot operations flags plan source of
          Left failure -> Left failure
          Right (source', 1) -> Right source'
          Right _ -> Left UnprovedRootAbstraction

plannedNames :: TypedItemPlan -> [String]
plannedNames item = plannedItemEntry item : case plannedItemBody item of
  ActionItem step probe marker _ -> [step, probe, marker]
  LetItem marker _ -> [marker]
  ObservationItem probe observation -> [probe, observation, observation ++ "_unit"]

parsedNames :: Data source => source -> [String]
parsedNames = everything (++) (mkQ [] (\name -> [occNameString (rdrNameOcc name)] :: [String]))

rewriteRoot
  :: GeneratedSegmentOperations
  -> DynFlags
  -> TypedSegmentPlan
  -> Located (HsModule GhcPs)
  -> Either TypedSegmentFailure (Located (HsModule GhcPs), Int)
rewriteRoot operations flags plan (L moduleLocation hsModule) = do
  (declarations, matches) <- foldl rewriteDeclaration (Right ([], 0)) (hsmodDecls hsModule)
  pure (L moduleLocation (hsModule { hsmodDecls = reverse declarations }), matches)
  where
    root = typedSegmentPlanRoot plan
    rewriteDeclaration result declaration = do
      (rewritten, count) <- result
      case unLoc declaration of
        ValD extension binding@FunBind { fun_id = identifier }
          | occNameString (rdrNameOcc (unLoc identifier)) == root -> do
              binding' <- rewriteBinding binding
              pure (L (getLoc declaration) (ValD extension binding') : rewritten, count + 1)
        _ -> pure (declaration : rewritten, count)

    rewriteBinding binding@FunBind{} = case unLoc (mg_alts (fun_matches binding)) of
      [locatedMatch] -> do
        let match = unLoc locatedMatch
        case (unLoc (m_pats match), grhssGRHSs (m_grhss match)) of
          ([], [locatedRhs]) -> do
            let rhs = unLoc locatedRhs
            case (rhs, grhssLocalBinds (m_grhss match)) of
              (GRHS rhsExtension [] (L _ (HsDo _ (DoExpr Nothing) statements)), EmptyLocalBinds _) -> do
                body <- rewriteStatements operations flags plan (unLoc statements)
                let rhs' = GRHS rhsExtension [] body
                    grhss' = (m_grhss match) { grhssGRHSs = [L (getLoc locatedRhs) rhs'] }
                    match' = match { m_grhss = grhss' }
                    matches' = (fun_matches binding)
                      { mg_alts = noLocA [L (getLoc locatedMatch) match'] }
                pure (binding { fun_matches = matches' })
              _ -> Left UnprovedRootAbstraction
          _ -> Left UnprovedRootAbstraction
      _ -> Left UnprovedRootAbstraction

    rewriteBinding _ = Left UnprovedRootAbstraction

rewriteStatements
  :: GeneratedSegmentOperations
  -> DynFlags
  -> TypedSegmentPlan
  -> [ExprLStmt GhcPs]
  -> Either TypedSegmentFailure (LHsExpr GhcPs)
rewriteStatements operations flags plan statements = case reverse statements of
  [] -> Left UnprovedRootAbstraction
  terminal : reversedItems -> do
    final <- terminalExpression terminal
    let items = reverse reversedItems
        planned = typedSegmentPlanItems plan
    if length items /= length planned
      then Left (UnprovedItemSequence (min (length items) (length planned)))
      else if not (consecutiveOrdinals (map plannedItemOrdinal planned))
        then Left InvalidItemOrdinals
        else lowerItems (zip planned items) final
  where
    terminalExpression (L _ statement) = case statement of
      LastStmt _ expression _ _
        | isPlainPureUnit expression -> Right (segmentPureUnit operations)
      BodyStmt _ expression _ _
        | isPlainPureUnit expression -> Right (segmentPureUnit operations)
      _ -> Left UnprovedRootAbstraction

    lowerItems [] final = Right final
    lowerItems ((item, statement) : rest) final = do
      continuation <- lowerItems rest final
      lowerItem operations flags item statement continuation

consecutiveOrdinals :: [Int] -> Bool
consecutiveOrdinals [] = True
consecutiveOrdinals (_ : []) = True
consecutiveOrdinals (first : second : rest) =
  second == first + 1 && consecutiveOrdinals (second : rest)

lowerItem
  :: GeneratedSegmentOperations
  -> DynFlags
  -> TypedItemPlan
  -> ExprLStmt GhcPs
  -> LHsExpr GhcPs
  -> Either TypedSegmentFailure (LHsExpr GhcPs)
lowerItem operations flags item (L _ statement) continuation =
  case plannedItemBody item of
    ActionItem step probe marker expectedBinders -> case statement of
      BindStmt _ pattern rhs
        | binderNames pattern == expectedBinders ->
            Right (segmentBind operations (wrapActionRhs step probe rhs)
              (lazyLambda marker (patternCase operations flags marker pattern continuation)))
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))
    LetItem marker expectedBinders -> case statement of
      LetStmt _ local
        | localBinderNames local == expectedBinders -> do
            local' <- addUnitMarker (plannedItemOrdinal item) marker local
            Right (noLocA (HsLet noAnn local' continuation))
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))
    ObservationItem probe observation -> case statement of
      BodyStmt _ rhs _ _ ->
        let checkerCall = mkHsApp (lazyLambda probe
              (mkHsApp (unqualifiedVariable "__tidepoolCellExpression") (variable probe))) (mkLHsPar rhs)
            unitContinuation = lazyLambda (observation ++ "_unit") continuation
        in Right (segmentBind operations checkerCall unitContinuation)
      _ -> Left (UnprovedItemSequence (plannedItemOrdinal item))

binderNames :: LPat GhcPs -> [String]
binderNames = map (occNameString . rdrNameOcc) . collectPatBinders CollNoDictBinders

localBinderNames :: HsLocalBinds GhcPs -> [String]
localBinderNames = map (occNameString . rdrNameOcc) . collectLocalBinders CollNoDictBinders

wrapActionRhs :: String -> String -> LHsExpr GhcPs -> LHsExpr GhcPs
wrapActionRhs step probe rhs = identity step (identity probe rhs)
  where
    identity name expression = mkHsApp (lazyLambda name (variable name)) (mkLHsPar expression)

lazyLambda :: String -> LHsExpr GhcPs -> LHsExpr GhcPs
lazyLambda name = mkHsLam (noLocA [lazyVariablePattern name])

lazyVariablePattern :: String -> LPat GhcPs
lazyVariablePattern name = noLocA (LazyPat noAnn (nlVarPat (unqualifiedName name)))

patternCase :: GeneratedSegmentOperations -> DynFlags -> String -> LPat GhcPs -> LHsExpr GhcPs -> LHsExpr GhcPs
patternCase operations flags valueName originalPattern continuation = noLocA
  (HsCase noAnn (variable valueName) alternatives)
  where
    sourceLoc = getLocA originalPattern
    failedPattern = L (getLoc originalPattern) (WildPat noExtField)
    message = showPpr flags
      (text "Pattern match failure in" <+> pprHsDoFlavour (DoExpr Nothing)
        <+> text "at" <+> ppr sourceLoc)
    failure = mkHsApps (qualifiedVariable operations "segmentFail")
      [noLocA (HsLit noExtField (mkHsString message))]
    alternatives = mkMatchGroup (Generated OtherExpansion SkipPmc)
      (noLocA [mkHsCaseAlt originalPattern continuation, mkHsCaseAlt failedPattern failure])

segmentBind :: GeneratedSegmentOperations -> LHsExpr GhcPs -> LHsExpr GhcPs -> LHsExpr GhcPs
-- GHC's parsed-expression printer preserves grouping through HsPar nodes.
-- Generated applications must retain those nodes around argument expressions.
segmentBind operations action continuation = mkHsApps (qualifiedVariable operations "segmentBind")
  [mkLHsPar action, mkLHsPar continuation]

segmentPureUnit :: GeneratedSegmentOperations -> LHsExpr GhcPs
segmentPureUnit operations = mkHsApp (qualifiedVariable operations "segmentPure") unitExpression

unitExpression :: LHsExpr GhcPs
unitExpression = noLocA (ExplicitTuple noAnn [] Boxed)

isPlainPureUnit :: LHsExpr GhcPs -> Bool
isPlainPureUnit expression = case unLoc expression of
  HsPar _ inner -> isPlainPureUnit inner
  HsApp _ function argument -> isPureName function && isUnitExpression argument
  _ -> False
  where
    isPureName function = case unLoc function of
      HsVar _ name -> isUnqual (unLoc name)
        && occNameString (rdrNameOcc (unLoc name)) == "pure"
      _ -> False
    isUnitExpression value = case unLoc value of
      HsPar _ inner -> isUnitExpression inner
      ExplicitTuple _ [] Boxed -> True
      HsVar _ name -> occNameString (rdrNameOcc (unLoc name)) == "()"
      _ -> False

unqualifiedName :: String -> RdrName
unqualifiedName = mkRdrUnqual . mkVarOcc

unqualifiedVariable :: String -> LHsExpr GhcPs
unqualifiedVariable = nlHsVar . unqualifiedName

qualifiedVariable :: GeneratedSegmentOperations -> String -> LHsExpr GhcPs
qualifiedVariable operations name = nlHsVar
  (mkRdrQual (generatedSegmentQualifier operations) (mkVarOcc name))

variable :: String -> LHsExpr GhcPs
variable = unqualifiedVariable

addUnitMarker :: Int -> String -> HsLocalBinds GhcPs -> Either TypedSegmentFailure (HsLocalBinds GhcPs)
addUnitMarker ordinal marker local = case local of
  EmptyLocalBinds _ -> Right (HsValBinds noAnn
    (ValBinds NoAnnSortKey [markerBinding] []))
  HsValBinds extension binds -> Right (HsValBinds extension
    (plusHsValBinds binds (ValBinds NoAnnSortKey [markerBinding] [])))
  HsIPBinds {} -> Left (UnprovedItemSequence ordinal)
  where
    markerBinding = mkHsVarBind noSrcSpan (unqualifiedName marker) unitExpression
