module Tidepool.Translate
  ( translateBinds
  , translateModule
  , translateModuleClosed
  , collectDataCons
  , collectUsedDataCons
  , collectTransitiveDCons
  , wiredInDataCons
  , FlatNode(..)
  , FlatAlt(..)
  , FlatAltCon(..)
  , LitEnc(..)
  , UnresolvedVar(..)
  ) where

import GHC
import GHC.Core
import GHC.Types.Id
import GHC.Types.Var
import GHC.Types.Unique (getKey)
import GHC.Core.DataCon (DataCon, dataConRepArity, dataConRepArgTys, dataConFullSig, dataConTag, dataConWorkId, dataConName, dataConSrcBangs, dataConOrigArgTys, HsSrcBang(..), HsBang(..), SrcUnpackedness(..), SrcStrictness(..))
import Language.Haskell.Syntax.Basic (Boxity(..))
import GHC.Builtin.Types (consDataCon, nilDataCon, trueDataCon, falseDataCon, charDataCon, unitDataCon, tupleDataCon, ordLTDataCon, ordEQDataCon, ordGTDataCon, intDataCon, wordDataCon, doubleDataCon, floatDataCon)
import GHC.Builtin.PrimOps
import GHC.Types.Literal
import GHC.Types.Name (nameOccName, isExternalName, isSystemName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Core.TyCon
import GHC.Core.Type (splitTyConApp_maybe, isCoercionTy)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Types.Var.Set (VarSet, emptyVarSet, extendVarSet, elemVarSet)
import GHC.Types.Unique.Set as USet (nonDetEltsUniqSet)
import GHC.Types.Unique.Set (UniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet, nonDetEltsUniqSet)
import GHC.Types.Basic (JoinPointHood(..))
import GHC.Utils.Outputable (showPprUnsafe)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import Data.Char (ord)
import Data.List (isPrefixOf)
import Data.Bits ((.&.), (.|.), shiftL)
import Data.Word
import Data.Int
import Data.Text (Text)
import qualified Data.Set as Set
import qualified Data.Text as T
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import Data.Sequence (Seq, (|>))
import qualified Data.Sequence as Seq
import qualified Data.Map.Strict as Map
import Control.Monad.State
import Control.Monad (foldM, forM, when)

import GHC.Driver.Env (HscEnv)
import Tidepool.Resolve (resolveExternals, UnresolvedVar(..))

data FlatNode
  = NVar !Word64
  | NLit !LitEnc
  | NApp !Int !Int
  | NLam !Word64 !Int
  | NLetNonRec !Word64 !Int !Int
  | NLetRec ![(Word64, Int)] !Int
  | NCase !Int !Word64 ![FlatAlt]
  | NCon !Word64 ![Int]
  | NJoin !Word64 ![Word64] !Int !Int
  | NJump !Word64 ![Int]
  | NPrimOp !Text ![Int]
  deriving (Show)

data FlatAlt = FlatAlt !FlatAltCon ![Word64] !Int
  deriving (Show)

data FlatAltCon = FDataAlt !Word64 | FLitAlt !LitEnc | FDefault
  deriving (Show)

data LitEnc
  = LEInt !Int64
  | LEWord !Word64
  | LEChar !Word32
  | LEString !ByteString
  | LEFloat !Word64    -- IEEE 754 bits
  | LEDouble !Word64   -- IEEE 754 bits
  deriving (Show)

data TransState = TransState
  { tsNodes :: !(Seq FlatNode)
  , tsUsedDCs :: !(Map.Map Word64 DataCon)
  , tsRecJoinIds :: !(Set.Set Word64)  -- join IDs from Rec groups (translated as LetRec lambdas)
  , tsSynthCounter :: !Word64          -- counter for synthetic VarIds (tag 'T')
  }

type TransM = State TransState

emitNode :: FlatNode -> TransM Int
emitNode n = do
  s <- get
  let idx = Seq.length (tsNodes s)
  put s { tsNodes = tsNodes s |> n }
  return idx

-- | Generate a fresh synthetic VarId with tag 'T' (Tidepool-generated).
freshSynthVarId :: TransM Word64
freshSynthVarId = do
  s <- get
  let c = tsSynthCounter s
  put s { tsSynthCounter = c + 1 }
  -- Tag 'T' = 0x54, shifted left 56 bits
  return (0x5400000000000000 .|. c)

recordDC :: DataCon -> TransM ()
recordDC dc = modify' $ \s ->
  s { tsUsedDCs = Map.insert (varId (dataConWorkId dc)) dc (tsUsedDCs s) }

translateBinds :: [CoreBind] -> [(String, Seq FlatNode)]
translateBinds binds = concatMap translateBind binds
  where
    translateBind (NonRec b rhs) =
      let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0)
          finalNodes = tsNodes s
          rootIdx = Seq.length finalNodes - 1
      in if idx == rootIdx
         then [(occNameString (nameOccName (idName b)), finalNodes)]
         else error "Root index mismatch in NonRec"
    translateBind (Rec pairs) =
      map (\(b, rhs) ->
        let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0)
            finalNodes = tsNodes s
            rootIdx = Seq.length finalNodes - 1
        in if idx == rootIdx
           then (occNameString (nameOccName (idName b)), finalNodes)
           else error "Root index mismatch in Rec"
      ) pairs

-- | Translate an entire module's bindings into a single self-contained tree.
-- All bindings become nested Let expressions wrapping a Var reference to the
-- target binding. This eliminates cross-binding Var references since all
-- definitions share one flat node array.
translateModule :: [CoreBind] -> String -> (Seq FlatNode, Map.Map Word64 DataCon)
translateModule allBinds targetName =
  let targetId = findTargetId targetName allBinds
      neededBinds = reachableBinds allBinds targetId
      (_, finalState) = runState (wrapAllBinds neededBinds targetId) (TransState Seq.empty Map.empty Set.empty 0)
  in (tsNodes finalState, tsUsedDCs finalState)
  where
    findTargetId name binds =
      case filter isTarget (concatMap bindersOf binds) of
        (b:_) -> b
        []    -> error $ "translateModule: exported top-level binding '" ++ name ++ "' not found"
      where
        isTarget b =
          occNameString (nameOccName (idName b)) == name
          && isExportedId b
          && isExternalName (idName b)
          && not (isSystemName (idName b))

    bindersOf (NonRec b _) = [b]
    bindersOf (Rec pairs)  = map fst pairs

    -- | Skip GHC-generated metadata bindings ($trModule, $krep, $tc*).
    -- These are Typeable / module-info bindings that reference unpackCString#
    -- and are never needed at runtime. Worker-wrappers ($w*) and
    -- specializations ($s*) must be kept.
    isMetadataBinder :: Id -> Bool
    isMetadataBinder b =
      let name = occNameString (nameOccName (idName b))
      in any (`isPrefixOf` name) ["$trModule", "$krep", "$tc", "$cShow"]

    -- | Filter bindings to only those transitively reachable from the target.
    -- BFS/DFS from the target binding's free variables, collecting all binding
    -- groups that are transitively referenced. Preserves original ordering.
    reachableBinds :: [CoreBind] -> Id -> [CoreBind]
    reachableBinds binds target =
      let -- For each binding group, collect binder keys and free variable keys
          bindInfo :: [(CoreBind, Set.Set Word64, Set.Set Word64)]
          bindInfo = map (\bind ->
            let pairs = case bind of
                  NonRec b rhs -> [(b, rhs)]
                  Rec ps       -> ps
                bkeys = Set.fromList [varId b | (b, _) <- pairs]
                fvs = Set.unions [exprFreeVarKeys rhs | (_, rhs) <- pairs]
            in (bind, bkeys, fvs)) binds

          -- Map from binder key -> index into bindInfo
          keyToIdx :: Map.Map Word64 Int
          keyToIdx = Map.fromList
            [(k, i) | (i, (_, bkeys, _)) <- zip [0..] bindInfo, k <- Set.toList bkeys]

          -- DFS collecting reachable bind-group indices
          go :: Set.Set Int -> [Word64] -> Set.Set Int
          go visited [] = visited
          go visited (v:vs) = case Map.lookup v keyToIdx of
            Just idx | not (Set.member idx visited) ->
              let (_, _, fvs) = bindInfo !! idx
              in go (Set.insert idx visited) (Set.toList fvs ++ vs)
            _ -> go visited vs

          targetKey = varId target
          reachable = case Map.lookup targetKey keyToIdx of
            Just idx ->
              let (_, _, fvs) = bindInfo !! idx
              in go (Set.singleton idx) (Set.toList fvs)
            Nothing -> Set.empty
      in [bind | (i, (bind, _, _)) <- zip [0..] bindInfo, Set.member i reachable]

    -- | Extract free variable keys (as Word64) from a Core expression.
    exprFreeVarKeys :: CoreExpr -> Set.Set Word64
    exprFreeVarKeys expr =
      let fvs = exprSomeFreeVars (const True) expr
      in Set.fromList [varId v | v <- nonDetEltsUniqSet fvs]

    wrapAllBinds :: [CoreBind] -> Id -> TransM Int
    wrapAllBinds [] target = emitNode (NVar (varId target))
    wrapAllBinds (NonRec b rhs : rest) target
      | isTyVar b = wrapAllBinds rest target  -- skip type bindings
      | otherwise = do
          rhsIdx <- translate rhs
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetNonRec (varId b) rhsIdx bodyIdx)
    wrapAllBinds (Rec pairs : rest) target = do
      let valPairs = filter (\(b, _) -> not (isTyVar b)) pairs
      if null valPairs
        then wrapAllBinds rest target
        else do
          -- Register rec join IDs so call sites emit App instead of Jump
          let recJoins = [varId b | (b, _) <- valPairs, isJoinId b]
          modify' $ \s -> s { tsRecJoinIds = tsRecJoinIds s `Set.union` Set.fromList recJoins }
          pairIdxs <- forM valPairs $ \(b, rhs) -> do
            rhs' <- case isJoinId_maybe b of
              Just arity -> do
                let (params, joinBody) = collectValueBinders arity rhs
                joinBodyIdx <- translate joinBody
                foldM (\inner p -> emitNode $ NLam (varId p) inner)
                      joinBodyIdx (reverse params)
              Nothing -> translate rhs
            return (varId b, rhs')
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetRec pairIdxs bodyIdx)

-- | Like translateModule, but first resolves cross-module references
-- by inlining unfoldings from the GHC environment. Returns the
-- translated tree, used DataCons, and any variables that could not
-- be resolved (no unfolding available).
translateModuleClosed :: HscEnv -> [CoreBind] -> String -> IO (Seq FlatNode, Map.Map Word64 DataCon, [UnresolvedVar], [CoreBind])
translateModuleClosed hscEnv allBinds targetName = do
  (closedBinds, unresolved) <- resolveExternals hscEnv allBinds
  let filtered = filter (not . isDesugaredVar . uvName) unresolved
      (nodes, usedDCs) = translateModule closedBinds targetName
      referencedIds = collectVarIds nodes
      -- Collect all binder VarIds (Let-bound vars in the CBOR tree)
      definedIds = collectDefinedIds nodes
      -- Find NVar IDs that are not defined by any Let binding
      orphanIds = Set.difference referencedIds definedIds
      trulyUnresolved = filter (\uv -> uvKey uv `Set.member` referencedIds) filtered
  -- Debug: print orphan IDs that might cause SIGSEGV
  when (not (Set.null orphanIds) && targetName == "result") $ do
    putStrLn $ "  [translate] ORPHAN NVar IDs for " ++ targetName ++ ": " ++
           show (Set.toList orphanIds)
    -- Try to find these IDs in the binder list
    let allBinders = [(varId b, occNameString (nameOccName (idName b))) | bind <- closedBinds, b <- case bind of { NonRec b _ -> [b]; Rec ps -> map fst ps }]
    mapM_ (\oid -> case lookup oid allBinders of
      Just name -> putStrLn $ "    " ++ show oid ++ " = " ++ name ++ " (defined but not in CBOR tree)"
      Nothing   -> putStrLn $ "    " ++ show oid ++ " = <not found in any binding>"
      ) (Set.toList orphanIds)
  return (nodes, usedDCs, trulyUnresolved, closedBinds)
  where
    isDesugaredVar name = name `elem`
      [ "unpackCString#", "unpackCStringUtf8#", "unpackAppendCString#"
      , "$wunsafeTake", "unsafeTake"
      , "divZeroError", "overflowError"
      , "error", "undefined"
      , "unsafeEqualityProof"
      ]
      || any (`isPrefixOf` name) ["$trModule", "$krep", "$tc", "krep$"]
    collectVarIds :: Seq FlatNode -> Set.Set Word64
    collectVarIds = foldl' (\acc node -> acc `Set.union` nodeVarIds node) Set.empty
    nodeVarIds :: FlatNode -> Set.Set Word64
    nodeVarIds (NVar v) = Set.singleton v
    nodeVarIds _ = Set.empty
    collectDefinedIds :: Seq FlatNode -> Set.Set Word64
    collectDefinedIds = foldl' (\acc node -> acc `Set.union` nodeDefIds node) Set.empty
    nodeDefIds :: FlatNode -> Set.Set Word64
    nodeDefIds (NLetNonRec v _ _) = Set.singleton v
    nodeDefIds (NLetRec pairs _) = Set.fromList (map fst pairs)
    nodeDefIds (NLam v _) = Set.singleton v
    nodeDefIds (NCase _ _ alts) = Set.fromList [v | FlatAlt _ bs _ <- alts, v <- bs]
    nodeDefIds _ = Set.empty

-- | Collect all DataCons encountered during translation of Core bindings.
-- This includes constructors from imported packages (e.g. freer-simple's
-- Val, E, Leaf, Node, Union) that aren't in the module's mg_tcs.
collectUsedDataCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text])]
collectUsedDataCons binds =
  let allDCs = foldMap collectFromBind binds
  in map dcToMeta (Map.elems allDCs)
  where
    collectFromBind (NonRec _ rhs) =
      let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0)
      in tsUsedDCs s
    collectFromBind (Rec pairs) =
      foldMap (\(_, rhs) ->
        let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0)
        in tsUsedDCs s
      ) pairs

    dcToMeta dc =
      ( varId (dataConWorkId dc)
      , T.pack (occNameString (nameOccName (dataConName dc)))
      , dataConTag dc
      , valueRepArity dc
      , map mapBang (dataConSrcBangs dc)
      )

-- | Compute transitive closure of TyCons reachable from all binder types,
-- expanding through newtypes, then return metadata for all their DataCons.
collectTransitiveDCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text])]
collectTransitiveDCons binds =
  let binderTypes = [ idType b | b <- concatMap bindersOfBind binds ]
      seedTyCons  = foldMap (nonDetEltsUniqSet . tyConsOfType) binderTypes
      allTyCons   = closeTyCons emptyUniqSet seedTyCons
  in  concatMap tyConToDCMeta (nonDetEltsUniqSet allTyCons)
  where
    bindersOfBind (NonRec b _) = [b]
    bindersOfBind (Rec pairs)  = map fst pairs

closeTyCons :: UniqSet TyCon -> [TyCon] -> UniqSet TyCon
closeTyCons visited []     = visited
closeTyCons visited (tc:rest)
  | tc `elementOfUniqSet` visited = closeTyCons visited rest
  | otherwise =
      let visited' = addOneToUniqSet visited tc
          newtypeChildren = case unwrapNewTyCon_maybe tc of
            Just (_tvs, reprTy, _coax) -> nonDetEltsUniqSet (tyConsOfType reprTy)
            Nothing                    -> []
          fieldChildren = case tyConDataCons_maybe tc of
            Just dcs -> [ ftc
                        | dc <- dcs
                        , Scaled _ ft <- dataConOrigArgTys dc
                        , ftc <- nonDetEltsUniqSet (tyConsOfType ft) ]
            Nothing  -> []
      in closeTyCons visited' (newtypeChildren ++ fieldChildren ++ rest)

tyConToDCMeta :: TyCon -> [(Word64, Text, Int, Int, [Text])]
tyConToDCMeta tc = case tyConDataCons_maybe tc of
  Just dcs -> map (\dc ->
    ( varId (dataConWorkId dc)
    , T.pack (occNameString (nameOccName (dataConName dc)))
    , dataConTag dc
    , valueRepArity dc
    , map mapBang (dataConSrcBangs dc)
    )) dcs
  Nothing  -> []

translate :: CoreExpr -> TransM Int
translate expr =
  let (hd, allArgs) = collectArgs expr
      args = filter isValueArg allArgs
  in case hd of
    -- Desugar unpackCString#/unpackCStringUtf8# to cons-cell chain:
    -- GHC represents string literals as (unpackCString# "addr"#) in Core.
    -- We expand to (:) 'c1' ((:) 'c2' ... []) so strings are uniform [Char]
    -- cons cells, enabling case matching and (++) to work correctly.
    Var v | isUnpackCStringVar v
          , [arg] <- args
          , Just bytes <- extractAddrLitBytes arg -> do
        let consId = varId (dataConWorkId consDataCon)
            nilId  = varId (dataConWorkId nilDataCon)
            charId = varId (dataConWorkId charDataCon)
        recordDC consDataCon
        recordDC nilDataCon
        recordDC charDataCon
        nilIdx <- emitNode $ NCon nilId []
        foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            emitNode $ NCon consId [charIdx, acc]
          ) nilIdx (reverse bytes)

    -- Desugar unpackAppendCString# "prefix"# suffix to cons chain:
    -- (:) 'p' ((:) 'r' (... ((:) 'x' suffix)))
    Var v | isUnpackAppendCStringVar v
          , [litArg, suffixArg] <- args
          , Just bytes <- extractAddrLitBytes litArg -> do
        suffixIdx <- translate suffixArg
        let consId = varId (dataConWorkId consDataCon)
            charId = varId (dataConWorkId charDataCon)
        recordDC consDataCon
        recordDC charDataCon
        foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            emitNode $ NCon consId [charIdx, acc]
          ) suffixIdx (reverse bytes)

    -- Desugar (++) xs ys → letrec go = \a -> case a of { [] -> ys; (:) x rest -> (:) x (go rest) } in go xs
    -- GHC.Internal.Base.++ has no unfolding available from the .hi file.
    Var v | isAppendVar v, [xsArg, ysArg] <- args -> do
        ysIdx <- translate ysArg
        xsIdx <- translate xsArg
        goId <- freshSynthVarId
        aId <- freshSynthVarId
        xId <- freshSynthVarId
        restId <- freshSynthVarId
        let consId = varId (dataConWorkId consDataCon)
            nilId  = varId (dataConWorkId nilDataCon)
        recordDC consDataCon
        recordDC nilDataCon
        -- Build the cons alt RHS: (:) x (go rest)
        goRef <- emitNode $ NVar goId
        restRef <- emitNode $ NVar restId
        goRestIdx <- emitNode $ NApp goRef restRef
        xRef <- emitNode $ NVar xId
        consResultIdx <- emitNode $ NCon consId [xRef, goRestIdx]
        -- Build: case a of { [] -> ys; (:) x rest -> (:) x (go rest) }
        aRef <- emitNode $ NVar aId
        let nilAlt  = FlatAlt (FDataAlt nilId) [] ysIdx
            consAlt = FlatAlt (FDataAlt consId) [xId, restId] consResultIdx
        caseIdx <- emitNode $ NCase aRef aId [nilAlt, consAlt]
        -- Build: \a -> case ...
        lamIdx <- emitNode $ NLam aId caseIdx
        -- Build: go xs
        goRef2 <- emitNode $ NVar goId
        appIdx <- emitNode $ NApp goRef2 xsIdx
        -- Build: letrec go = \a -> ... in go xs
        emitNode $ NLetRec [(goId, lamIdx)] appIdx

    -- Desugar $wunsafeTake n# xs → recursive list take with unboxed counter.
    -- GHC worker-wrappers `take` at -O2; the worker $wunsafeTake has no unfolding.
    Var v | isUnsafeTakeVar v, [nArg, xsArg] <- args -> do
        nIdx <- translate nArg
        xsIdx <- translate xsArg
        goId <- freshSynthVarId
        iId <- freshSynthVarId
        aId <- freshSynthVarId
        xId <- freshSynthVarId
        restId <- freshSynthVarId
        let consId = varId (dataConWorkId consDataCon)
            nilId  = varId (dataConWorkId nilDataCon)
        recordDC consDataCon
        recordDC nilDataCon
        -- Build: (:) x (go (IntSub i 1) rest)
        goRef1 <- emitNode $ NVar goId
        iRef1 <- emitNode $ NVar iId
        lit1 <- emitNode $ NLit (LEInt 1)
        iSub1 <- emitNode $ NPrimOp (T.pack "IntSub") [iRef1, lit1]
        goISub1 <- emitNode $ NApp goRef1 iSub1
        restRef <- emitNode $ NVar restId
        goISub1Rest <- emitNode $ NApp goISub1 restRef
        xRef <- emitNode $ NVar xId
        consResult <- emitNode $ NCon consId [xRef, goISub1Rest]
        -- Build: case a of { [] -> []; (:) x rest -> (:) x (go (i-1) rest) }
        nilIdx <- emitNode $ NCon nilId []
        aRef <- emitNode $ NVar aId
        let aNilAlt  = FlatAlt (FDataAlt nilId) [] nilIdx
            aConsAlt = FlatAlt (FDataAlt consId) [xId, restId] consResult
        aCaseIdx <- emitNode $ NCase aRef aId [aNilAlt, aConsAlt]
        -- Build: case (IntLe i 0) of { DEFAULT -> <aCaseIdx>; 1# -> [] }
        iRef2 <- emitNode $ NVar iId
        lit0 <- emitNode $ NLit (LEInt 0)
        leResult <- emitNode $ NPrimOp (T.pack "IntLe") [iRef2, lit0]
        let leDefaultAlt = FlatAlt FDefault [] aCaseIdx
            leTrueAlt    = FlatAlt (FLitAlt (LEInt 1)) [] nilIdx
        leCaseIdx <- emitNode $ NCase leResult 0 [leDefaultAlt, leTrueAlt]
        -- Build: \i -> \a -> case (IntLe i 0) of ...
        lamA <- emitNode $ NLam aId leCaseIdx
        lamI <- emitNode $ NLam iId lamA
        -- Build: go n xs
        goRef2 <- emitNode $ NVar goId
        goN <- emitNode $ NApp goRef2 nIdx
        goNXs <- emitNode $ NApp goN xsIdx
        -- Build: letrec go = \i -> \a -> ... in go n xs
        emitNode $ NLetRec [(goId, lamI)] goNXs

    Var v | Just dc <- isDataConWorkId_maybe v
          , length args == valueRepArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId v) childIdxs

    -- GADT constructors have separate wrapper Ids that handle type coercions.
    -- isDataConWorkId_maybe returns Nothing for wrappers, so we check separately.
    -- We emit NCon using the *worker* Id since that's what DataConTable indexes.
    Var v | Just dc <- isDataConWrapId_maybe v
          , length args == valueRepArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId (dataConWorkId dc)) childIdxs

    -- unsafeEqualityProof → unit value (always matches the single UnsafeRefl alt)
    -- GHC uses this for GADT equality evidence in freer-simple's Member constraint.
    -- It only appears as a case scrutinee with one alternative, so the tag is irrelevant.
    Var v | isUnsafeEqualityProofVar v -> do
        recordDC unitDataCon
        emitNode $ NCon (varId (dataConWorkId unitDataCon)) []

    -- tagToEnum# @T arg → case arg of { 0# → C0; 1# → C1; ... }
    -- We desugar here because type information is erased downstream.
    Var v | Just pop <- isPrimOpId_maybe v
          , pop == TagToEnumOp
          , length args == 1 -> do
        let typeArgs = filter (not . isValueArg) allArgs
        case typeArgs of
          [Type ty] | Just (tc, _) <- splitTyConApp_maybe ty -> do
            let dcs = tyConDataCons tc
            argIdx <- translate (head args)
            altData <- forM (zip [0..] dcs) $ \(i :: Int, dc) -> do
              recordDC dc
              conIdx <- emitNode $ NCon (varId (dataConWorkId dc)) []
              return $ FlatAlt (FLitAlt (LEInt (fromIntegral i))) [] conIdx
            -- Use VarId 0 as the case binder (unused in alternatives)
            emitNode $ NCase argIdx 0 altData
          _ -> error $ "tagToEnum# without resolvable type argument"

    Var v | Just pop <- isPrimOpId_maybe v
          , length args == primOpArity pop -> do
        childIdxs <- mapM translate args
        emitNode $ NPrimOp (mapPrimOp pop) childIdxs

    Var v | Just arity <- isJoinId_maybe v
          , length allArgs == arity -> do
        recJoins <- gets tsRecJoinIds
        if Set.member (varId v) recJoins
          then do
            -- Rec join point: translated as LetRec lambda, emit App chain
            hIdx <- emitNode $ NVar (varId v)
            childIdxs <- mapM translate args
            foldM (\fIdx aIdx -> emitNode $ NApp fIdx aIdx) hIdx childIdxs
          else do
            childIdxs <- mapM translate args
            emitNode $ NJump (varId v) childIdxs
    
    _ -> do
      hIdx <- translateHead hd
      foldM (\fIdx arg -> do
        aIdx <- translate arg
        emitNode $ NApp fIdx aIdx) hIdx args

translateHead :: CoreExpr -> TransM Int
translateHead = \case
  Var v
    | isRuntimeErrorVar v -> do
        let kind = if occNameString (nameOccName (idName v)) == "divZeroError" then 0 else 1
        emitNode $ NVar (0x4500000000000000 .|. kind)  -- tag 'E' for error
    | isErrorVar v -> emitNode $ NVar 0x4500000000000002  -- tag 'E', kind 2 (error)
    | isUndefinedVar v -> emitNode $ NVar 0x4500000000000003  -- tag 'E', kind 3 (undefined)
    | isTypeMetadataVar v -> emitNode $ NVar 0x4500000000000004  -- tag 'E', kind 4 (type metadata)
    | otherwise -> do
        emitNode $ NVar (varId v)
  Lit l -> emitNode $ NLit (mapLit l)
  Lam b body
    | isTyVar b -> translate body
    | otherwise -> do
        bodyIdx <- translate body
        emitNode $ NLam (varId b) bodyIdx
  Let (NonRec b rhs) body
    | Just arity <- isJoinId_maybe b -> do
        let (params, joinRhs) = collectValueBinders arity rhs
        joinRhsIdx <- translate joinRhs
        bodyIdx <- translate body
        emitNode $ NJoin (varId b) (map varId params) joinRhsIdx bodyIdx
    | otherwise -> do
        rhsIdx <- translate rhs
        bodyIdx <- translate body
        emitNode $ NLetNonRec (varId b) rhsIdx bodyIdx
  Let (Rec pairs) body -> do
    -- For join point binders in Rec groups (GHC's "joinrec"), strip the
    -- join arity and translate as regular lambdas.  Register them so that
    -- call sites emit NApp chains instead of NJump.
    let recJoins = [varId b | (b, _) <- pairs, isJoinId b]
    modify' $ \s -> s { tsRecJoinIds = tsRecJoinIds s `Set.union` Set.fromList recJoins }
    pairIdxs <- forM pairs $ \(b, rhs) -> do
      rhs' <- case isJoinId_maybe b of
        Just arity -> do
          let (params, joinBody) = collectValueBinders arity rhs
          joinBodyIdx <- translate joinBody
          -- Build nested NLam chain: \p1 -> \p2 -> ... -> joinBody
          foldM (\inner p -> emitNode $ NLam (varId p) inner)
                joinBodyIdx (reverse params)
        Nothing -> translate rhs
      return (varId b, rhs')
    bodyIdx <- translate body
    emitNode $ NLetRec pairIdxs bodyIdx
  -- Desugar multi-return primops: case quotRemInt# a b of (# q, r #) -> body
  -- Split into:
  --   case quotInt# a b of q { DEFAULT ->
  --   case remInt# a b of r { DEFAULT ->
  --   body }}
  -- This ensures both components are forced and a/b are shared.
  Case scrut _caseBinder _ty [Alt (DataAlt _dc) binders body]
    | (Var v, allArgs) <- collectArgs scrut
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name) <- splitMultiReturnPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a, b] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [qBinder, rBinder] <- vBinders -> do
        aIdx <- translate a
        bIdx <- translate b
        qValIdx <- emitNode $ NPrimOp op1Name [aIdx, bIdx]
        rValIdx <- emitNode $ NPrimOp op2Name [aIdx, bIdx]
        -- Bind q and r using Case to force them, then translate body
        bodyIdx <- translate body
        -- case rVal of rBinder { DEFAULT -> body }
        rCaseIdx <- emitNode $ NCase rValIdx (varId rBinder) [FlatAlt FDefault [] bodyIdx]
        -- case qVal of qBinder { DEFAULT -> rCaseIdx }
        emitNode $ NCase qValIdx (varId qBinder) [FlatAlt FDefault [] rCaseIdx]
  Case scrut b _alts_ty alts -> do
    scrutIdx <- translate scrut
    altData <- mapM translateAlt alts
    emitNode $ NCase scrutIdx (varId b) altData
  Cast e _ -> translate e
  Tick _ e -> translate e
  Type _ -> error "Bare Type in expression position"
  Coercion _ -> error "Bare Coercion in expression position"
  App _ _ -> error "App should be handled by translate"
  _ -> error "Unexpected expression form"

translateAlt :: CoreAlt -> TransM FlatAlt
translateAlt (Alt con binders body) = do
  let vBinders = filter (not . isTyVar) binders
  altCon <- mapAltCon con
  bodyIdx <- translate body
  return $ FlatAlt altCon (map varId vBinders) bodyIdx

mapAltCon :: AltCon -> TransM FlatAltCon
mapAltCon = \case
  DataAlt dc -> do
    recordDC dc
    return $ FDataAlt (varId (dataConWorkId dc))
  LitAlt l   -> return $ FLitAlt (mapLit l)
  DEFAULT    -> return FDefault

varId :: Var -> Word64
varId v = fromIntegral (getKey (varUnique v))

collectValueBinders :: Int -> CoreExpr -> ([Var], CoreExpr)
collectValueBinders 0 e = ([], e)
collectValueBinders n (Lam b e)
  | isTyVar b = collectValueBinders (n-1) e  -- type args count toward join arity
  | otherwise = let (bs, body) = collectValueBinders (n-1) e in (b:bs, body)
-- GHC may eta-reduce join point RHSes; return what we found.
collectValueBinders _ e = ([], e)

isValueArg :: CoreExpr -> Bool
isValueArg (Type _) = False
isValueArg (Coercion _) = False
isValueArg _ = True

mapLit :: Literal -> LitEnc
mapLit = \case
  LitNumber nt n  -> case nt of
    LitNumInt    -> LEInt (fromInteger n)
    LitNumInt8   -> LEInt (fromInteger n)
    LitNumInt16  -> LEInt (fromInteger n)
    LitNumInt32  -> LEInt (fromInteger n)
    LitNumInt64  -> LEInt (fromInteger n)
    LitNumWord   -> LEWord (fromInteger n)
    LitNumWord8  -> LEWord (fromInteger n)
    LitNumWord16 -> LEWord (fromInteger n)
    LitNumWord32 -> LEWord (fromInteger n)
    LitNumWord64 -> LEWord (fromInteger n)
    LitNumBigNat -> error "BigNat literal not supported"
  LitChar c              -> LEChar (fromIntegral (ord c))
  LitString bs           -> LEString bs
  LitFloat r             -> LEFloat (fromIntegral (castFloatToWord32 (fromRational r)))
  LitDouble r            -> LEDouble (castDoubleToWord64 (fromRational r))
  other                  -> error $ "Unsupported literal: " ++ showPprUnsafe other

mapPrimOp :: PrimOp -> Text
mapPrimOp = \case
  IntAddOp    -> "IntAdd"
  IntSubOp    -> "IntSub"
  IntMulOp    -> "IntMul"
  IntNegOp    -> "IntNegate"
  IntEqOp     -> "IntEq"
  IntNeOp     -> "IntNe"
  IntLtOp     -> "IntLt"
  IntLeOp     -> "IntLe"
  IntGtOp     -> "IntGt"
  IntGeOp     -> "IntGe"
  WordAddOp   -> "WordAdd"
  WordSubOp   -> "WordSub"
  WordMulOp   -> "WordMul"
  WordEqOp    -> "WordEq"
  WordNeOp    -> "WordNe"
  WordLtOp    -> "WordLt"
  WordLeOp    -> "WordLe"
  WordGtOp    -> "WordGt"
  WordGeOp    -> "WordGe"
  DoubleAddOp -> "DoubleAdd"
  DoubleSubOp -> "DoubleSub"
  DoubleMulOp -> "DoubleMul"
  DoubleDivOp -> "DoubleDiv"
  DoubleEqOp  -> "DoubleEq"
  DoubleNeOp  -> "DoubleNe"
  DoubleLtOp  -> "DoubleLt"
  DoubleLeOp  -> "DoubleLe"
  DoubleGtOp  -> "DoubleGt"
  DoubleGeOp  -> "DoubleGe"
  CharEqOp    -> "CharEq"
  CharNeOp    -> "CharNe"
  CharLtOp    -> "CharLt"
  CharLeOp    -> "CharLe"
  CharGtOp    -> "CharGt"
  CharGeOp    -> "CharGe"
  IndexArrayOp -> "IndexArray"
  TagToEnumOp -> "TagToEnum"
  DataToTagSmallOp -> "DataToTag"
  DataToTagLargeOp -> "DataToTag"
  IntQuotOp -> "IntQuot"
  IntRemOp  -> "IntRem"
  ChrOp     -> "Chr"
  OrdOp     -> "Ord"
  -- Int bitwise
  IntAndOp  -> "IntAnd"
  IntOrOp   -> "IntOr"
  IntXorOp  -> "IntXor"
  IntNotOp  -> "IntNot"
  IntSllOp  -> "IntShl"
  IntSraOp  -> "IntShra"
  IntSrlOp  -> "IntShrl"
  -- Word arithmetic + bitwise
  WordQuotOp -> "WordQuot"
  WordRemOp  -> "WordRem"
  WordAndOp  -> "WordAnd"
  WordOrOp   -> "WordOr"
  WordXorOp  -> "WordXor"
  WordNotOp  -> "WordNot"
  WordSllOp  -> "WordShl"
  WordSrlOp  -> "WordShrl"
  -- Int↔Word conversions
  IntToWordOp -> "Int2Word"
  WordToIntOp -> "Word2Int"
  -- Narrowing
  Narrow8IntOp   -> "Narrow8Int"
  Narrow16IntOp  -> "Narrow16Int"
  Narrow32IntOp  -> "Narrow32Int"
  Narrow8WordOp  -> "Narrow8Word"
  Narrow16WordOp -> "Narrow16Word"
  Narrow32WordOp -> "Narrow32Word"
  -- Float arithmetic + comparison
  FloatAddOp    -> "FloatAdd"
  FloatSubOp    -> "FloatSub"
  FloatMulOp    -> "FloatMul"
  FloatDivOp    -> "FloatDiv"
  FloatNegOp    -> "FloatNegate"
  FloatEqOp     -> "FloatEq"
  FloatNeOp     -> "FloatNe"
  FloatLtOp     -> "FloatLt"
  FloatLeOp     -> "FloatLe"
  FloatGtOp     -> "FloatGt"
  FloatGeOp     -> "FloatGe"
  -- Double extras
  DoubleNegOp   -> "DoubleNegate"
  -- Type conversions
  IntToDoubleOp   -> "Int2Double"
  DoubleToIntOp   -> "Double2Int"
  IntToFloatOp    -> "Int2Float"
  FloatToIntOp    -> "Float2Int"
  DoubleToFloatOp -> "Double2Float"
  FloatToDoubleOp -> "Float2Double"
  -- Addr#
  IndexOffAddrOp_Char -> "IndexCharOffAddr"
  other       -> error $ "Unsupported primop: " ++ showPprUnsafe other

collectDataCons :: [TyCon] -> [(Word64, Text, Int, Int, [Text])]
collectDataCons tycons =
  [ (varId (dataConWorkId dc), T.pack (occNameString (nameOccName (dataConName dc))), dataConTag dc, valueRepArity dc, map mapBang (dataConSrcBangs dc))
  | tc <- tycons
  , isAlgTyCon tc
  , dc <- tyConDataCons tc
  ]

mapBang :: HsSrcBang -> Text
mapBang (HsSrcBang _ (HsBang srcUnpack srcBang)) =
  case (srcUnpack, srcBang) of
    (_, SrcStrict) -> "SrcBang"
    (SrcUnpack, _) -> "SrcUnpack"
    _              -> "NoSrcBang"

-- | Wired-in constructors that GHC always knows about but may not appear in
-- mg_tcs or binder types. We include these unconditionally in metadata so
-- that ToCore impls ((), Bool, Char, Int, Word, Double, Float, tuples,
-- Ordering, lists) always find their constructors in the DataConTable.
wiredInDataCons :: [(Word64, Text, Int, Int, [Text])]
wiredInDataCons = concatMap (\dc ->
    [( varId (dataConWorkId dc)
     , T.pack (occNameString (nameOccName (dataConName dc)))
     , dataConTag dc
     , valueRepArity dc
     , map mapBang (dataConSrcBangs dc)
     )]
  ) wiredInList
  where
    wiredInList =
      [ consDataCon, nilDataCon
      , trueDataCon, falseDataCon
      , charDataCon, unitDataCon
      , intDataCon, wordDataCon, doubleDataCon, floatDataCon
      , tupleDataCon Boxed 2  -- (,)
      , tupleDataCon Boxed 3  -- (,,)
      , ordLTDataCon, ordEQDataCon, ordGTDataCon
      ]

-- | Count value arguments excluding GADT equality evidence.
-- dataConRepArity includes equality evidence args (EqSpec) for GADT constructors,
-- but GHC Core passes these as Coercion arguments, which isValueArg filters out.
-- Subtract the EqSpec count to match what the translator sees as "value arguments".
-- For non-GADT constructors (including typeclass dicts), EqSpec is empty so this
-- equals dataConRepArity.
valueRepArity :: DataCon -> Int
valueRepArity dc =
  let (_, _, eqSpec, _, _, _) = dataConFullSig dc
  in dataConRepArity dc - length eqSpec

-- | Recognize GHC's unpackCString# and unpackCStringUtf8# builtins.
-- These convert Addr# (C string pointers) to [Char]. Since our
-- serializer already has the string bytes as LitString, we erase
-- the conversion and keep just the literal.
-- | Recognize GHC.Internal.Base.++ (list append).
isAppendVar :: Id -> Bool
isAppendVar v = occNameString (nameOccName (idName v)) == "++"

isErrorVar :: Id -> Bool
isErrorVar v = occNameString (nameOccName (idName v)) == "error"

isUndefinedVar :: Id -> Bool
isUndefinedVar v = occNameString (nameOccName (idName v)) == "undefined"

isUnsafeTakeVar :: Id -> Bool
isUnsafeTakeVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "$wunsafeTake" || name == "unsafeTake"

isRuntimeErrorVar :: Id -> Bool
isRuntimeErrorVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "divZeroError" || name == "overflowError"

isUnsafeEqualityProofVar :: Id -> Bool
isUnsafeEqualityProofVar v =
  occNameString (nameOccName (idName v)) == "unsafeEqualityProof"

-- | Recognize GHC type-representation metadata vars ($tc*, $trModule*, krep$*, $krep*).
-- These have no runtime semantics and no unfoldings; emit as error VarId.
isTypeMetadataVar :: Id -> Bool
isTypeMetadataVar v =
  let name = occNameString (nameOccName (idName v))
  in any (`isPrefixOf` name) ["$trModule", "$krep", "$tc", "krep$"]

isUnpackCStringVar :: Id -> Bool
isUnpackCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackCString#" || name == "unpackCStringUtf8#"

-- | Recognize GHC's unpackAppendCString# builtin.
-- unpackAppendCString# :: Addr# -> [Char] -> [Char]
-- Prepends a C string literal to a suffix list.
isUnpackAppendCStringVar :: Id -> Bool
isUnpackAppendCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackAppendCString#"

-- | Recognize primops that return unboxed tuples and can be split into
-- two individual primops. Returns (primop1, primop2) text names.
splitMultiReturnPrimOp :: PrimOp -> Maybe (Text, Text)
splitMultiReturnPrimOp = \case
  IntQuotRemOp  -> Just (T.pack "IntQuot", T.pack "IntRem")
  WordQuotRemOp -> Just (T.pack "WordQuot", T.pack "WordRem")
  _             -> Nothing

-- | Extract Addr# literal bytes from an expression.
-- Handles both direct Lit and Var with an unfolding to Lit
-- (GHC -O2 let-floats Addr# literals into separate bindings).
extractAddrLitBytes :: CoreExpr -> Maybe [Word8]
extractAddrLitBytes (Lit (LitString bs)) = Just (BS.unpack bs)
extractAddrLitBytes (Var v) =
  case maybeUnfoldingTemplate (idUnfolding v) of
    Just (Lit (LitString bs)) -> Just (BS.unpack bs)
    _ -> case maybeUnfoldingTemplate (realIdUnfolding v) of
      Just (Lit (LitString bs)) -> Just (BS.unpack bs)
      _ -> Nothing
extractAddrLitBytes _ = Nothing

primOpArity :: PrimOp -> Int
primOpArity op = let (_, _, _, a, _) = primOpSig op in a

isJoinId_maybe :: Id -> Maybe Int
isJoinId_maybe v = case idJoinPointHood v of
  JoinPoint n -> Just n
  NotJoinPoint -> Nothing
