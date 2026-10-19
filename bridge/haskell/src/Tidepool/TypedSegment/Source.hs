-- | Parsed-source lowering for compiler-owned typed effect segments.
-- Authored nested do expressions remain opaque to this rewrite.
module Tidepool.TypedSegment.Source
  ( rewriteParsedSegmentRoot, typedSegmentInputOccurrences
  ) where

import Control.Exception (throwIO)
import Data.Data (Data)
import Data.Generics (everything, mkQ)
import Data.List (intersect, nub)
import qualified Data.Set as Set
import GHC (ParsedModule (..))
import GHC.Driver.Session (DynFlags, GeneralFlag(Opt_HelpfulErrors,Opt_DeferOutOfScopeVariables), gopt_set, gopt_unset)
import GHC.Driver.Env (HscEnv, hsc_dflags, hscUpdateFlags)
import GHC.Rename.Expr (rnLExpr)
import GHC.Rename.Unbound (isUnboundName)
import GHC.Tc.Utils.Monad (initTcInteractive, discardErrs)
import GHC.Types.Name (nameOccName)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Parser qualified as Parser
import GHC.Parser.Lexer (ParseResult(..), initParserState, unP)
import GHC.Data.FastString (mkFastString)
import GHC.Data.StringBuffer (stringToStringBuffer)
import GHC.Driver.Ppr (showPpr)
import GHC.Hs
import GHC.Types.Basic (Boxity (Boxed), DoPmc (SkipPmc), GenReason (OtherExpansion), Origin (Generated))
import GHC.Types.Name.Occurrence (isVarOcc, mkVarOcc, occNameString)
import GHC.Types.Name.Reader (RdrName, isUnqual, mkRdrQual, mkRdrUnqual, rdrNameOcc)
import GHC.Types.SrcLoc (GenLocated (L), Located, getLoc, unLoc, noSrcSpan, mkRealSrcLoc)
import GHC.Unit.Module.ModSummary (ms_hspp_opts)
import GHC.Utils.Outputable (ppr, text, (<+>))
import Tidepool.TypedSegment.Types

-- GHC owns lexical scope here, before any old binding is hydrated. An empty
-- global environment leaves unresolved uses as HsUnboundVar, preserving the
-- original qualifier; let/do/lambda/case binders become renamed local Names.
-- Expected missing-global diagnostics are discarded only for this demand
-- probe. Its syntax is never admitted or typechecked, and the real compiler
-- still validates the complete submitted program through its normal owner.
-- Callers must apply the world effective-flag/execution policy before entry.
typedSegmentInputOccurrences :: HscEnv -> TypedSegmentPlan -> String -> IO (Either TypedSegmentFailure [String])
typedSegmentInputOccurrences environment plan source = case rootExpression of
  Left failure -> pure (Left failure)
  Right expression -> do
    let probeFlags = gopt_set (gopt_unset flags Opt_HelpfulErrors) Opt_DeferOutOfScopeVariables
        probe = hscUpdateFlags (const probeFlags) environment
    (_,renamed) <- initTcInteractive probe (discardErrs (fst <$> rnLExpr expression))
    pure $ case renamed of
      Nothing -> Left UnprovedRootAbstraction
      Just result -> Right (Set.toAscList (Set.fromList
        (everything (++) (mkQ [] selected) result)))
  where
    flags = hsc_dflags environment
    parser = initParserState (initParserOpts flags) (stringToStringBuffer source)
      (mkRealSrcLoc (mkFastString "<held-world-segment>") 1 1)
    rootExpression = case unP Parser.parseModule parser of
      PFailed _ -> Left UnprovedRootAbstraction
      POk _ parsed -> case
          [ expression
          | L _ (ValD _ FunBind {fun_id=identifier,fun_matches=matches}) <- hsmodDecls (unLoc parsed)
          , occNameString (rdrNameOcc (unLoc identifier)) == typedSegmentPlanRoot plan
          , [L _ match] <- [unLoc (mg_alts matches)]
          , [] <- [unLoc (m_pats match)]
          , EmptyLocalBinds _ <- [grhssLocalBinds (m_grhss match)]
          , [L _ (GRHS _ [] expression)] <- [grhssGRHSs (m_grhss match)]
          ] of
        [expression] -> Right expression
        _ -> Left UnprovedRootAbstraction
    selected :: HsExpr GhcRn -> [String]
    selected (HsUnboundVar _ name)
      | isUnqual name && isVarOcc (rdrNameOcc name) = [occNameString (rdrNameOcc name)]
    selected (HsVar _ (L _ name))
      | isUnboundName name && isVarOcc (nameOccName name) = [occNameString (nameOccName name)]
    selected _ = []

rewriteParsedSegmentRoot :: GeneratedSegmentOperations -> TypedSegmentPlan -> ParsedModule -> IO ParsedModule
rewriteParsedSegmentRoot operations plan parsed = case rewriteModule of
  Left failure -> throwIO failure
  Right rewrittenSource -> pure (parsed { pm_parsed_source = rewrittenSource })
  where
    source = pm_parsed_source parsed
    authoredNames = parsedNames source
    generatedNames = concatMap plannedNames (typedSegmentPlanItems plan)
    collisions = generatedNames `intersect` authoredNames
    rewriteModule
      | length generatedNames /= length (nub generatedNames) = Left InvalidReservedNames
      | not (null collisions) = Left InvalidReservedNames
      | otherwise = case rewriteRoot operations (ms_hspp_opts (pm_mod_summary parsed)) plan source of
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
