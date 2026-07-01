module Tidepool.Translate
  ( translateBinds
  , translateModule
  , translateModuleClosed
  , collectDataCons
  , collectUsedDataCons
  , collectTransitiveDCons
  , wiredInDataCons
  , mergeMetaPreserving
  , dcToMeta
  , valueRepArity
  , mapBang
  , targetBindingHasIO
  , FlatNode(..)
  , FlatAlt(..)
  , FlatAltCon(..)
  , LitEnc(..)
  , UnresolvedVar(..)
  , varId
  , stableVarId
  , fieldParentDisamb
  ) where

import GHC
import GHC.Core
import GHC.Types.Id
import GHC.Types.Var (isTyVar, isCoVar, varUnique, varName, setVarUnique)
import GHC.Types.Unique (getKey)
import GHC.Types.Unique.Supply (UniqSupply, mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Core.DataCon (DataCon, dataConRepArity, dataConRepArgTys, dataConFullSig, dataConTag, dataConWorkId, dataConName, dataConSrcBangs, dataConOrigArgTys, isUnboxedTupleDataCon, HsSrcBang(..), HsBang(..), SrcUnpackedness(..), SrcStrictness(..))
import Language.Haskell.Syntax.Basic (Boxity(..))
import GHC.Builtin.Types (consDataCon, nilDataCon, trueDataCon, falseDataCon, charDataCon, unitDataCon, tupleDataCon, ordLTDataCon, ordEQDataCon, ordGTDataCon, intDataCon, wordDataCon, doubleDataCon, floatDataCon)
import GHC.Builtin.Names (ioTyConKey)
import GHC.Builtin.PrimOps
import GHC.Types.Literal
import GHC.Types.Name (nameOccName, isExternalName, isSystemName, nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString, fieldOcc_maybe)
import GHC.Data.FastString (unpackFS)
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Unit.Types (moduleUnitId, unitIdString)
import GHC.Utils.Fingerprint (fingerprintString, Fingerprint(..))
import GHC.Core.TyCon
import GHC.Core.Type (splitTyConApp_maybe, splitFunTy_maybe, isCoercionTy)
import GHC.Builtin.Types.Prim (statePrimTyCon)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Types.Var.Set (VarSet, emptyVarSet, extendVarSet, elemVarSet)
import GHC.Types.Unique.Set as USet (nonDetEltsUniqSet)
import GHC.Types.Unique.Set (UniqSet, emptyUniqSet, addOneToUniqSet, elementOfUniqSet, nonDetEltsUniqSet)
import GHC.Types.Basic (JoinPointHood(..))
import GHC.Utils.Outputable (showPprUnsafe)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import Data.Char (ord)
import Data.List (isPrefixOf, isInfixOf)
import Data.Bits ((.&.), (.|.), shiftL, shiftR)
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
import System.IO (hPutStrLn, stderr)

import GHC.Driver.Env (HscEnv)
import Tidepool.Resolve (resolveExternals, UnresolvedVar(..))
import qualified System.Environment
import qualified Data.List
import qualified Data.Maybe
import qualified Numeric
import qualified Tidepool.GhcPipeline
import qualified Debug.Trace
import System.IO.Unsafe (unsafePerformIO)

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
  | LEByteArray !ByteString  -- raw ByteArray# contents (e.g. BigNat# payload)
  | LEFloat !Word64    -- IEEE 754 bits
  | LEDouble !Word64   -- IEEE 754 bits
  deriving (Show)

data TransState = TransState
  { tsNodes :: !(Seq FlatNode)
  -- Keyed by (stableVarId, module-qualified name), NOT varId alone. Two
  -- DISTINCT constructors whose 56-bit varIds collide would otherwise coalesce
  -- here — silently, within a single scan, before the (varId,qname)-keyed
  -- merge and the Rust insert_checked guard could ever see the clash. Keying on
  -- the qualified name too keeps both colliding entries distinct so the
  -- collision reaches the loud guard. In the no-collision case each varId maps
  -- to exactly one qname, so the recorded constructor set is unchanged.
  , tsUsedDCs :: !(Map.Map (Word64, Text) DataCon)
  , tsRecJoinIds :: !(Set.Set Word64)  -- join IDs from Rec groups (translated as LetRec lambdas)
  , tsSynthCounter :: !Word64          -- counter for synthetic VarIds (tag 'T')
  , tsUnresolvedIds :: !(Set.Set Word64) -- IDs that should be translated as error nodes
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
  s { tsUsedDCs = Map.insert (varId (dataConWorkId dc), qualifiedName (dataConName dc)) dc (tsUsedDCs s) }

-- | Emit a runtime unpackCString# loop for a non-static Addr# value.
-- Produces: letrec go = \a -> case indexCharOffAddr# a 0# of
--             { '\0'# -> []; c -> C# c : go (plusAddr# a 1#) }
--           in go addrIdx
emitRuntimeUnpackCString :: Int -> TransM Int
emitRuntimeUnpackCString addrIdx = do
    goId <- freshSynthVarId
    aId <- freshSynthVarId
    let consId = varId (dataConWorkId consDataCon)
        nilId  = varId (dataConWorkId nilDataCon)
        charId = varId (dataConWorkId charDataCon)
    recordDC consDataCon
    recordDC nilDataCon
    recordDC charDataCon
    -- Body: indexCharOffAddr# a 0#
    aRef <- emitNode $ NVar aId
    lit0 <- emitNode $ NLit (LEInt 0)
    charAt <- emitNode $ NPrimOp (T.pack "IndexCharOffAddr") [aRef, lit0]
    -- [] (nil result)
    nilIdx <- emitNode $ NCon nilId []
    -- C# charAt : go (plusAddr# a 1#)
    charBox <- emitNode $ NCon charId [charAt]
    lit1 <- emitNode $ NLit (LEInt 1)
    aRef2 <- emitNode $ NVar aId
    nextAddr <- emitNode $ NPrimOp (T.pack "PlusAddr") [aRef2, lit1]
    goRef <- emitNode $ NVar goId
    goNext <- emitNode $ NApp goRef nextAddr
    consResult <- emitNode $ NCon consId [charBox, goNext]
    -- case charAt of { '\0'# -> []; DEFAULT -> consResult }
    let nullAlt = FlatAlt (FLitAlt (LEChar 0)) [] nilIdx
        defaultAlt = FlatAlt FDefault [] consResult
    caseIdx <- emitNode $ NCase charAt 0 [nullAlt, defaultAlt]
    -- \a -> case ...
    lamA <- emitNode $ NLam aId caseIdx
    -- go addrIdx
    goRef2 <- emitNode $ NVar goId
    appIdx <- emitNode $ NApp goRef2 addrIdx
    -- letrec go = \a -> ... in go addrIdx
    emitNode $ NLetRec [(goId, lamA)] appIdx

-- | Emit a runtime unpackAppendCString# loop for a non-static Addr# value.
-- Like emitRuntimeUnpackCString but appends suffix instead of []:
-- letrec go = \a -> case indexCharOffAddr# a 0# of
--           { '\0'# -> suffix; c -> C# c : go (plusAddr# a 1#) }
--         in go addrIdx
emitRuntimeUnpackAppendCString :: Int -> Int -> TransM Int
emitRuntimeUnpackAppendCString addrIdx suffixIdx = do
    goId <- freshSynthVarId
    aId <- freshSynthVarId
    let consId = varId (dataConWorkId consDataCon)
        charId = varId (dataConWorkId charDataCon)
    recordDC consDataCon
    recordDC charDataCon
    -- Body: indexCharOffAddr# a 0#
    aRef <- emitNode $ NVar aId
    lit0 <- emitNode $ NLit (LEInt 0)
    charAt <- emitNode $ NPrimOp (T.pack "IndexCharOffAddr") [aRef, lit0]
    -- C# charAt : go (plusAddr# a 1#)
    charBox <- emitNode $ NCon charId [charAt]
    lit1 <- emitNode $ NLit (LEInt 1)
    aRef2 <- emitNode $ NVar aId
    nextAddr <- emitNode $ NPrimOp (T.pack "PlusAddr") [aRef2, lit1]
    goRef <- emitNode $ NVar goId
    goNext <- emitNode $ NApp goRef nextAddr
    consResult <- emitNode $ NCon consId [charBox, goNext]
    -- case charAt of { '\0'# -> suffix; DEFAULT -> consResult }
    let nullAlt = FlatAlt (FLitAlt (LEChar 0)) [] suffixIdx
        defaultAlt = FlatAlt FDefault [] consResult
    caseIdx <- emitNode $ NCase charAt 0 [nullAlt, defaultAlt]
    -- \a -> case ...
    lamA <- emitNode $ NLam aId caseIdx
    -- go addrIdx
    goRef2 <- emitNode $ NVar goId
    appIdx <- emitNode $ NApp goRef2 addrIdx
    -- letrec go = \a -> ... in go addrIdx
    emitNode $ NLetRec [(goId, lamA)] appIdx

-- | Emit a safe replacement body for $fShowDouble_$sshowSignedFloat.
-- The original body pulls in floatToDigits/Integer arithmetic which the JIT
-- can't handle. We replace it with:
--   \fmt -> \minExpt -> \d -> \rest -> unpackAppendCString# (ShowDoubleAddr d) rest
-- This preserves the ShowS continuation (rest) for correct behavior in
-- composed show expressions like derived Show instances.
emitShowDoubleSpecBody :: Id -> TransM Int
emitShowDoubleSpecBody binder = do
    fmtId      <- freshSynthVarId
    minExptId  <- freshSynthVarId
    dId        <- freshSynthVarId
    restId     <- freshSynthVarId
    dRef       <- emitNode $ NVar dId
    addrIdx    <- emitNode $ NPrimOp (T.pack "ShowDoubleAddr") [dRef]
    restRef    <- emitNode $ NVar restId
    resultIdx  <- emitRuntimeUnpackAppendCString addrIdx restRef
    lam4       <- emitNode $ NLam restId resultIdx
    lam3       <- emitNode $ NLam dId lam4
    lam2       <- emitNode $ NLam minExptId lam3
    emitNode $ NLam fmtId lam2

translateBinds :: [CoreBind] -> [(String, Seq FlatNode)]
translateBinds binds = concatMap translateBind binds
  where
    translateBind (NonRec b rhs) =
      let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
          finalNodes = tsNodes s
          rootIdx = Seq.length finalNodes - 1
      in if idx == rootIdx
         then [(occNameString (nameOccName (idName b)), finalNodes)]
         else error "Root index mismatch in NonRec"
    translateBind (Rec pairs) =
      map (\(b, rhs) ->
        let (idx, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
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
--
-- Returns the emitted nodes, the DataCons used during translation, and the
-- reachable binds ('neededBinds') that were actually compiled. The reachable
-- subset is exactly what the emitted program references; callers feed it to
-- the DataConTable meta walks so those harvest only constructors the program
-- can run, never the full closed graph (quoter-internal / TH machinery binds
-- that merely sit on the include path).
translateModule :: [CoreBind] -> String -> Set.Set Word64 -> (Seq FlatNode, Map.Map (Word64, Text) DataCon, [CoreBind])
translateModule allBinds targetName unresolvedIds =
  let targetId = findTargetId targetName allBinds
      neededBinds = reachableBinds allBinds targetId
      (_, finalState) = runState (wrapAllBinds neededBinds targetId) (TransState Seq.empty Map.empty Set.empty 0 unresolvedIds)
  in (tsNodes finalState, tsUsedDCs finalState, neededBinds)
  where
    findTargetId name binds =
      case filter isTarget (concatMap bindersOf binds) of
        (b:_) -> b
        -- Fall back to name-only match if no External binding found
        -- (GHC may mark user bindings as Internal after optimization)
        []    -> case filter isNameMatch (concatMap bindersOf binds) of
                   (b:_) -> b
                   []    -> error $ "translateModule: exported top-level binding '" ++ name ++ "' not found"
      where
        isTarget b =
          occNameString (nameOccName (idName b)) == name
          && isExportedId b
          && isExternalName (idName b)
          && not (isSystemName (idName b))
        isNameMatch b =
          occNameString (nameOccName (idName b)) == name
          && not (isSystemName (idName b))

    bindersOf (NonRec b _) = [b]
    bindersOf (Rec pairs)  = map fst pairs

    -- | Filter bindings to only those transitively reachable from the target.
    -- Flattens Rec groups into individual (binder, rhs) pairs for fine-grained
    -- reachability analysis, then reconstructs reachable pairs into a single Rec.
    -- This prevents a single large Rec from pulling in all bindings when only
    -- a few are actually needed.
    reachableBinds :: [CoreBind] -> Id -> [CoreBind]
    reachableBinds binds target =
      let -- Flatten all binding groups into individual (binder, rhs) pairs
          allPairs :: [(Id, CoreExpr)]
          allPairs = concatMap (\bind -> case bind of
            NonRec b rhs -> [(b, rhs)]
            Rec ps       -> ps) binds

          -- Index each pair individually
          pairInfo :: [((Id, CoreExpr), Word64, Set.Set Word64)]
          pairInfo = map (\p@(b, rhs) ->
            (p, varId b, exprFreeVarKeys rhs)) allPairs

          -- Map from binder key -> index into pairInfo
          keyToIdx :: Map.Map Word64 Int
          keyToIdx = Map.fromList
            [(k, i) | (i, (_, k, _)) <- zip [0..] pairInfo]

          pairInfoLen = length pairInfo
          pairInfoAt idx = case drop idx pairInfo of
            (x:_) -> x
            []    -> error $ "reachableBinds: index " ++ show idx ++ " out of bounds (length " ++ show pairInfoLen ++ ")"

          -- DFS collecting reachable pair indices
          go :: Set.Set Int -> [Word64] -> Set.Set Int
          go visited [] = visited
          go visited (v:vs) = case Map.lookup v keyToIdx of
            Just idx | not (Set.member idx visited) ->
              let (_, _, fvs) = pairInfoAt idx
              in go (Set.insert idx visited) (Set.toList fvs ++ vs)
            _ -> go visited vs

          targetKey = varId target
          reachable = case Map.lookup targetKey keyToIdx of
            Just idx ->
              let (_, _, fvs) = pairInfoAt idx
              in go (Set.singleton idx) (Set.toList fvs)
            Nothing -> Set.empty

          reachablePairs = [(b, rhs) | (i, ((b, rhs), _, _)) <- zip [0..] pairInfo, Set.member i reachable]
      in if null reachablePairs then [] else [Rec reachablePairs]

    -- | Free variable keys (as 'varId' Word64s) of a Core expression, computed
    -- SYNTACTICALLY over the same tree 'translate' emits NVars from, with
    -- scoping keyed by varId (the id space of the serialized program and the
    -- JIT emit env). GHC's own FV machinery ('exprSomeFreeVars') is wrong for
    -- reachability on two axes: (1) it also walks let-binders' IdInfo
    -- (unfolding templates, RULES), which the translator never emits — and
    -- 'externalizeInternalTops' renames occurrences only in expression bodies,
    -- so IdInfo still holds the PRE-RENAME vars; (2) its result set dedups by
    -- unique, so such a stale IdInfo var (same unique, different name ⇒
    -- different varId) can EVICT the renamed tree var from the free-var set.
    -- The reachability walk then misses the binding while the emitted program
    -- still references it: a dangling NVar that traps at runtime only when
    -- forced (the @$sunion@ class — a module-local SPEC binding referenced by
    -- a 'Map.fromListWith' combine that only fires on key collision).
    exprFreeVarKeys :: CoreExpr -> Set.Set Word64
    exprFreeVarKeys = go Set.empty
      where
        bindV b bound | isTyVar b || isCoVar b = bound
                      | otherwise = Set.insert (varId b) bound
        go bound expr = case expr of
          Var v | isTyVar v || isCoVar v -> Set.empty
                | varId v `Set.member` bound -> Set.empty
                | otherwise -> Set.singleton (varId v)
          Lit{} -> Set.empty
          App f a -> go bound f `Set.union` go bound a
          Lam b e -> go (bindV b bound) e
          Let (NonRec b rhs) e -> go bound rhs `Set.union` go (bindV b bound) e
          Let (Rec ps) e ->
            let bound' = foldr (bindV . fst) bound ps
            in foldr (Set.union . go bound' . snd) (go bound' e) ps
          Case s b _ alts ->
            let boundB = bindV b bound
            in go bound s `Set.union`
               foldr (\(Alt _ bs rhs) acc ->
                        go (foldr bindV boundB bs) rhs `Set.union` acc)
                     Set.empty alts
          Cast e _ -> go bound e
          Tick _ e -> go bound e
          Type{} -> Set.empty
          Coercion{} -> Set.empty

    wrapAllBinds :: [CoreBind] -> Id -> TransM Int
    wrapAllBinds [] target = emitNode (NVar (varId target))
    wrapAllBinds (NonRec b rhs : rest) target
      | isTyVar b = wrapAllBinds rest target  -- skip type bindings
      | isShowDoubleSpecVar b = do
          -- Replace body with safe lambda wrapper instead of compiling the
          -- original body which pulls in floatToDigits/Integer arithmetic.
          -- \fmt -> \minExpt -> \d -> \rest -> unpackAppendCString (ShowDoubleAddr d) rest
          rhsIdx <- emitShowDoubleSpecBody b
          bodyIdx <- wrapAllBinds rest target
          emitNode (NLetNonRec (varId b) rhsIdx bodyIdx)
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
            rhs' <- if isShowDoubleSpecVar b
              then emitShowDoubleSpecBody b
              else case isJoinId_maybe b of
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
-- translated tree, used DataCons, any variables that could not be
-- resolved (no unfolding available), and the REACHABLE binds that were
-- actually compiled (the target's transitive closure, NOT the full closed
-- graph) — the meta walks consume this so the DataConTable only ever sees
-- constructors the emitted program references.
translateModuleClosed :: HscEnv -> [CoreBind] -> String -> IO (Seq FlatNode, Map.Map (Word64, Text) DataCon, [UnresolvedVar], [CoreBind])
translateModuleClosed hscEnv allBinds targetName = do
  (closedBinds0, unresolved) <- resolveExternals varId hscEnv allBinds
  closedBinds <- uniquifyDuplicateBinders closedBinds0
  -- TIDEPOOL_DUMP_CLOSED=<needle>: dump resolved bindings whose binder
  -- name contains the needle (post-resolveExternals Core — what the JIT
  -- actually compiles; can differ from --dump-core's module view).
  dumpNeedle <- System.Environment.lookupEnv "TIDEPOOL_DUMP_CLOSED"
  case dumpNeedle of
    Just needle ->
      -- Match individual (binder, rhs) pairs (the closed graph is one giant
      -- Rec post-resolveExternals); emit on stderr (stdout is swallowed by
      -- the Rust runtime on success).
      let pairs = concatMap (\cb -> case cb of
            NonRec b rhs -> [(b, rhs)]
            Rec ps       -> ps) closedBinds
          matches = [ p | p@(b, _) <- pairs
                    , needle `Data.List.isInfixOf` occNameString (nameOccName (idName b)) ]
      in mapM_ (\(b, rhs) -> hPutStrLn stderr
           ("=== CLOSED BIND " ++ occNameString (nameOccName (idName b)) ++ "\n"
            ++ Tidepool.GhcPipeline.dumpCore [NonRec b rhs])) matches
    Nothing -> pure ()
  -- TIDEPOOL_VARID_AUDIT=1: report VarId collisions — distinct binding
  -- sites whose varId hashes coincide. The JIT's emit env is a flat map
  -- keyed by VarId; a collision aliases two closures (#313 t11 class).
  auditEnv <- System.Environment.lookupEnv "TIDEPOOL_VARID_AUDIT"
  case auditEnv of
    Just _ -> do
      let sites = filter (not . isTyVar . fst) (concatMap bindingSites closedBinds)
          grouped = Map.fromListWith (++)
            [ (varId b, [(b, top)]) | (b, top) <- sites ]
          collisions = Map.filter (\xs -> length xs > 1) grouped
          describe (b, mtop) =
            (case mtop of
               Nothing -> "TOP "
               Just t  -> "in " ++ occNameString (nameOccName (varName t))
                          ++ "_" ++ showPprUnsafe (varUnique t) ++ ": ")
            ++ occNameString (nameOccName (varName b))
            ++ "_" ++ showPprUnsafe (varUnique b)
            ++ (case nameModule_maybe (varName b) of
                  Just m  -> " [" ++ moduleNameString (moduleName m) ++ "]"
                  Nothing -> "")
      mapM_ (\(vid, xs) -> hPutStrLn stderr
               ("[VARID COLLISION] 0x" ++ Numeric.showHex vid ""
                ++ " sites=" ++ show (length xs) ++ ": "
                ++ Data.List.intercalate " | " (map describe xs)))
            (Map.toList collisions)
      hPutStrLn stderr ("[VARID AUDIT] " ++ show (length sites)
        ++ " binding sites, " ++ show (Map.size collisions) ++ " collisions")
      -- TIDEPOOL_VARID_AUDIT=<hex>,<hex>,...: additionally resolve specific
      -- VarIds (e.g. lam_binder values from TIDEPOOL_TRACE=calls) to names.
      case auditEnv of
        Just spec | spec /= "1" -> do
          let parseHex h = case Numeric.readHex (dropWhile (== 'x') (dropWhile (== '0') h)) of
                [(n, "")] -> Just (n :: Word64)
                _         -> Nothing
              wanted = Data.Maybe.mapMaybe parseHex (splitOnComma spec)
              splitOnComma s = case break (== ',') s of
                (a, ',':rest) -> a : splitOnComma rest
                (a, _)        -> [a]
          mapM_ (\vid -> hPutStrLn stderr
                   ("[VARID NAME] 0x" ++ Numeric.showHex vid "" ++ " = "
                    ++ maybe "<not a binding site>"
                             (Data.List.intercalate " | " . map describe)
                             (Map.lookup vid grouped)))
                wanted
        _ -> pure ()
    Nothing -> pure ()
  let unresolvedIds = Set.fromList (map uvKey unresolved)
      (nodes, usedDCs, reachBinds) = translateModule closedBinds targetName unresolvedIds
  let referencedIds = foldl' (\acc n -> case n of { NVar v -> Set.insert v acc; _ -> acc }) Set.empty nodes
      trulyUnresolved = filter (\uv -> uvKey uv `Set.member` referencedIds) unresolved
      -- Debug: find dangling NVar references (referenced but not bound by any Let/Lam/Case)
      boundIds = foldl' collectBound Set.empty nodes
      danglingIds = Set.filter (\v -> not (Set.member v boundIds) && (v `shiftR` 56) /= 0x45) referencedIds
  -- Dangling NVar check — ids the emitted program references but nothing
  -- binds (and no 0x45 poison covers). These surface at runtime as an
  -- unresolved_var_trap ONLY when forced, so the class hides behind
  -- rarely-taken branches (a fromListWith combine that only fires on key
  -- collision). Fail LOUDLY at extract time, naming the symbols.
  --
  -- The one LEGIT dangling class: tidepool-repl session values
  -- (Tidepool.Session.Val.*). Their values live in the resident JIT machine's
  -- heap, bound at codegen via the ExternalEnv override keyed on stableVarId
  -- (see Resolve.isSessionValVar) — subtract them before judging.
  --
  -- TIDEPOOL_DANGLING_DEBUG=1 additionally prints EVERY dangling id
  -- (session-val ones included) for forensics.
  let refVars = Map.fromListWith (++)
        [ (varId v, [v]) | cb <- closedBinds, v <- deepVarRefsOfCB cb ]
      describeRef v = occNameString (nameOccName (varName v))
        ++ (case nameModule_maybe (varName v) of
              Just m  -> " [" ++ moduleNameString (moduleName m) ++ "]"
              Nothing -> "")
      isSessionValRef v = case nameModule_maybe (varName v) of
        Just m  -> "Tidepool.Session.Val." `isPrefixOf` moduleNameString (moduleName m)
        Nothing -> False
      nameDangling vid = case Map.findWithDefault [] vid refVars of
        [] -> "<no reference site in closed graph>"
        vs -> Data.List.intercalate " | " (Data.List.nub (map describeRef vs))
      hardDangling =
        [ vid | vid <- Set.toList danglingIds
              , not (any isSessionValRef (Map.findWithDefault [] vid refVars)) ]
  danglingEnv <- System.Environment.lookupEnv "TIDEPOOL_DANGLING_DEBUG"
  case danglingEnv of
    Just _ ->
      mapM_ (\vid -> hPutStrLn stderr
               ("[DANGLING NVAR] 0x" ++ Numeric.showHex vid "" ++ " = "
                ++ nameDangling vid))
            (Set.toList danglingIds)
    Nothing -> pure ()
  case hardDangling of
    [] -> pure ()
    vids -> error $
      "Dangling NVar reference(s) — the emitted program references these but "
      ++ "nothing binds them; forcing one at runtime would trap as an "
      ++ "unresolved variable:\n"
      ++ unlines [ "  0x" ++ Numeric.showHex vid "" ++ " = " ++ nameDangling vid
                 | vid <- vids ]
      ++ "This is an extract-pipeline bug (a binding was renamed, culled, or "
      ++ "missed by reachability) — not a user error."
  -- Return the REACHABLE binds (what 'translateModule' actually compiled), not
  -- the full closed graph. The meta walks (collectUsedDataCons /
  -- collectTransitiveDCons) run over this, so they harvest only constructors
  -- the emitted program references — quoter-internal Tidepool.QQ.* AST cons and
  -- other compile-time-only binds on the include path are no longer collected.
  -- (The TIDEPOOL_DUMP_CLOSED / TIDEPOOL_VARID_AUDIT diagnostics above keep
  -- scanning the FULL closedBinds: they are forensics over the whole compiled
  -- graph, independent of what reaches the table.)
  return (nodes, usedDCs, trulyUnresolved, reachBinds)
  where
    collectBound :: Set.Set Word64 -> FlatNode -> Set.Set Word64
    collectBound acc (NLam b _) = Set.insert b acc
    collectBound acc (NLetNonRec b _ _) = Set.insert b acc
    collectBound acc (NLetRec pairs _) = foldl' (\a (b,_) -> Set.insert b a) acc pairs
    collectBound acc (NCase _ b alts) =
      let withBinder = Set.insert b acc
      in foldl' (\a (FlatAlt _ bs _) -> foldl' (\a' b' -> Set.insert b' a') a bs) withBinder alts
    collectBound acc (NJoin b params _ _) = foldl' (\a p -> Set.insert p a) (Set.insert b acc) params
    collectBound acc _ = acc
    bindersOfCB (NonRec b _) = [b]
    bindersOfCB (Rec pairs)  = map fst pairs
    -- Walk into all expressions to find ALL variable references (for debug naming)
    deepVarRefsOfCB :: CoreBind -> [Id]
    deepVarRefsOfCB (NonRec _ rhs) = deepVarRefsOfExpr rhs
    deepVarRefsOfCB (Rec pairs) = concatMap (deepVarRefsOfExpr . snd) pairs
    deepVarRefsOfExpr :: CoreExpr -> [Id]
    deepVarRefsOfExpr (Var v) = [v]
    deepVarRefsOfExpr (Lit _) = []
    deepVarRefsOfExpr (App f a) = deepVarRefsOfExpr f ++ deepVarRefsOfExpr a
    deepVarRefsOfExpr (Lam _ e) = deepVarRefsOfExpr e
    deepVarRefsOfExpr (Let bind e) = deepVarRefsOfCB bind ++ deepVarRefsOfExpr e
    deepVarRefsOfExpr (Case scrut _ _ alts) =
      deepVarRefsOfExpr scrut ++ concatMap (\(Alt _ _ rhs) -> deepVarRefsOfExpr rhs) alts
    deepVarRefsOfExpr (Cast e _) = deepVarRefsOfExpr e
    deepVarRefsOfExpr (Tick _ e) = deepVarRefsOfExpr e
    deepVarRefsOfExpr (Type _) = []
    deepVarRefsOfExpr (Coercion _) = []

-- | #313 t11 fix: globally freshen duplicate binder uniques.
--
-- GHC's rapier-style simplifier clones a binder only when it clashes with
-- the enclosing in-scope set, so identical uniques legitimately recur in
-- DISJOINT sibling scopes (an unfolding template inlined at N sites keeps
-- its binder uniques in every copy — observed: one @$j2@ bound at 20
-- sites, 1491 duplicated binders in a single closed graph). Lexically
-- harmless — but the serialized program keys everything by
-- @VarId = hash(occName, unique)@: the JIT's flat emit env, the global
-- rec-join registry ('tsRecJoinIds'), closure capture resolution. Two
-- binding sites sharing a VarId alias each other's closures at runtime
-- (#313 t11: the second T.breakOn's SpecConstr'd join @$j3@ aliased the
-- first's — same code pointer, different captures → CASE TRAP).
--
-- Walk the whole program threading a global set of seen unique keys; a
-- repeat binder gets a fresh unique, substituted through its scope via a
-- lexical 'VarEnv'. Lexical scoping makes the local substitution
-- complete. TyVars/CoVars are skipped (erased at translation). Top-level
-- binders are never renamed (external names post-'externalizeInternalTops',
-- referenced across bindings); their keys seed the seen set.
uniquifyDuplicateBinders :: [CoreBind] -> IO [CoreBind]
uniquifyDuplicateBinders binds = do
  us0 <- mkSplitUniqSupply 'k'
  let topKeys = Set.fromList
        [ getKey (varUnique b) | bind <- binds, b <- topBindersOf bind ]
  return (evalState (mapM goTop binds) (us0, topKeys))
  where
    topBindersOf (NonRec b _) = [b]
    topBindersOf (Rec ps)     = map fst ps

    goTop :: CoreBind -> State (UniqSupply, Set.Set Word64) CoreBind
    goTop (NonRec b rhs) = NonRec b <$> goE emptyVarEnv rhs
    goTop (Rec ps) = Rec <$> mapM (\(b, rhs) -> (b,) <$> goE emptyVarEnv rhs) ps

    -- Visit a binder: rename iff its unique key was already seen.
    goB :: VarEnv Var -> Var -> State (UniqSupply, Set.Set Word64) (VarEnv Var, Var)
    goB env b
      | isTyVar b || isCoVar b = return (env, b)
      | otherwise = do
          (us, seen) <- get
          let k = getKey (varUnique b)
          if k `Set.member` seen
            then do
              let fresh s = let (u, s') = takeUniqFromSupply s
                            in if getKey u `Set.member` seen then fresh s' else (u, s')
                  (u', us') = fresh us
                  b' = setVarUnique b u'
              put (us', Set.insert (getKey u') seen)
              return (extendVarEnv env b b', b')
            else do
              put (us, Set.insert k seen)
              return (env, b)

    goBs :: VarEnv Var -> [Var] -> State (UniqSupply, Set.Set Word64) (VarEnv Var, [Var])
    goBs env [] = return (env, [])
    goBs env (b:bs) = do
      (env', b') <- goB env b
      (env'', bs') <- goBs env' bs
      return (env'', b' : bs')

    goE :: VarEnv Var -> CoreExpr -> State (UniqSupply, Set.Set Word64) CoreExpr
    goE env expr = case expr of
      Var v -> return (Var (maybe v id (lookupVarEnv env v)))
      Lit{} -> return expr
      App f a -> App <$> goE env f <*> goE env a
      Lam b body -> do
        (env', b') <- goB env b
        Lam b' <$> goE env' body
      Let (NonRec b rhs) body -> do
        rhs' <- goE env rhs
        (env', b') <- goB env b
        Let (NonRec b' rhs') <$> goE env' body
      Let (Rec ps) body -> do
        (env', bs') <- goBs env (map fst ps)
        rhss' <- mapM (goE env' . snd) ps
        Let (Rec (zip bs' rhss')) <$> goE env' body
      Case s b ty alts -> do
        s' <- goE env s
        (env', b') <- goB env b
        alts' <- mapM (goAlt env') alts
        return (Case s' b' ty alts')
      Cast e co -> (`Cast` co) <$> goE env e
      Tick t e -> Tick t <$> goE env e
      Type{} -> return expr
      Coercion{} -> return expr
      where
        goAlt env' (Alt c bs rhs) = do
          (env'', bs') <- goBs env' bs
          Alt c bs' <$> goE env'' rhs

-- | All binding sites (binder, enclosing top-level binder) in a CoreBind,
-- including nested Lam/Let/Case binders (Nothing = the site IS top-level).
-- Used by the TIDEPOOL_VARID_AUDIT collision check / name resolver.
bindingSites :: CoreBind -> [(Var, Maybe Var)]
bindingSites (NonRec b rhs) = (b, Nothing) : map (\v -> (v, Just b)) (nestedBinders rhs)
bindingSites (Rec ps) =
  concatMap (\(b, rhs) -> (b, Nothing) : map (\v -> (v, Just b)) (nestedBinders rhs)) ps

nestedBinders :: CoreExpr -> [Var]
nestedBinders = go
  where
    go (Lam b e)                 = b : go e
    go (Let (NonRec b r) e)      = b : go r ++ go e
    go (Let (Rec ps) e)          = map fst ps ++ concatMap (go . snd) ps ++ go e
    go (Case s b _ alts)         = b : go s ++ concatMap goAlt alts
    go (App f a)                 = go f ++ go a
    go (Cast e _)                = go e
    go (Tick _ e)                = go e
    go _                         = []
    goAlt (Alt _ bs e)           = bs ++ go e

-- | True when a unit id names the GHC compiler library itself — id of the form
-- @ghc-<version>@ (e.g. @ghc-9.12.2-fb67@). @ghc-prim@, @ghc-bignum@,
-- @ghc-internal@, @ghc-boot@ all have a word (not a digit) after @ghc-@, so
-- they are NOT matched — those carry executable constructors (@I#@, @D#@,
-- @Integer@, …) the JIT really runs.
isGhcCompilerUnitId :: String -> Bool
isGhcCompilerUnitId uid = case Data.List.stripPrefix "ghc-" uid of
  Just (c : _) -> c >= '0' && c <= '9'
  _            -> False

-- | True when a Name is defined in the GHC compiler library.
--
-- Such entities (HsExpr, DynFlags, RdrName, …) are reachable only through
-- compile-time-only TH machinery: the vendored @Tidepool.QQ.HsMeta.*@ hole
-- parser imports the GHC API, and those binders enter the translated set even
-- though the JIT never runs them. Their constructors must be kept OUT of the
-- DataConTable: the whole @DynFlags@ type closure is thousands of cons, and at
-- that volume a 64-bit varId collision evicts a real constructor — notably
-- freer-simple's @Union@, which fails effect-machine setup for every QQ eval.
isGhcCompilerName :: Name -> Bool
isGhcCompilerName n = case nameModule_maybe n of
  Just m  -> isGhcCompilerUnitId (unitIdString (moduleUnitId m))
  Nothing -> False

isGhcCompilerDC :: DataCon -> Bool
isGhcCompilerDC = isGhcCompilerName . dataConName

isGhcCompilerTyCon :: TyCon -> Bool
isGhcCompilerTyCon = isGhcCompilerName . tyConName

-- | Collect all DataCons encountered during translation of Core bindings.
-- This includes constructors from imported packages (e.g. freer-simple's
-- Val, E, Leaf, Node, Union) that aren't in the module's mg_tcs.
-- GHC compiler-library constructors are excluded (see 'isGhcCompilerDC').
collectUsedDataCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text], Text)]
collectUsedDataCons binds =
  let allDCs = foldMap collectFromBind binds
  in map dcToMeta (filter (not . isGhcCompilerDC) (Map.elems allDCs))
  where
    collectFromBind (NonRec _ rhs) =
      let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
      in tsUsedDCs s
    collectFromBind (Rec pairs) =
      foldMap (\(_, rhs) ->
        let (_, s) = runState (translate rhs) (TransState Seq.empty Map.empty Set.empty 0 Set.empty)
        in tsUsedDCs s
      ) pairs

dcToMeta :: DataCon -> (Word64, Text, Int, Int, [Text], Text)
dcToMeta dc =
  ( varId (dataConWorkId dc)
  , T.pack (occNameString (nameOccName (dataConName dc)))
  , dataConTag dc
  , valueRepArity dc
  , map mapBang (dataConSrcBangs dc)
  , qualifiedName (dataConName dc)
  )

-- | Combine the metadata sources (HIGHEST priority FIRST, e.g.
-- @[wiredIn, tycon, used, scan, transitive]@) into the final entry list.
--
-- Entries are coalesced by @(varId, module-qualified name)@: the same
-- constructor seen across several sources collapses to its highest-priority
-- copy. Two DISTINCT constructors that hash to the SAME varId (a collision)
-- have different qualified names, so they key DIFFERENTLY here and are BOTH
-- preserved — the loader then rejects the duplicate id loudly
-- (@DataConTable::insert_checked@), naming both. This replaces the old
-- @Map.fromList@/@Map.union@ merge that was keyed on the varId alone and
-- silently dropped one of a colliding pair (the freer-simple @Union@
-- eviction). In the no-collision case the output is identical — every varId
-- still appears once, in ascending varId order — so meta.cbor is unchanged.
mergeMetaPreserving :: [[(Word64, Text, Int, Int, [Text], Text)]]
                    -> [(Word64, Text, Int, Int, [Text], Text)]
mergeMetaPreserving sources =
  -- Map.fromList keeps the LAST value per key, so feed the flattened sources
  -- reversed: the highest-priority copy (earliest in the input) is seen last
  -- and wins. Map.elems then yields ascending (varId, qname) order.
  Map.elems $ Map.fromList
    [ ((dcid, qname), e)
    | e@(dcid, _, _, _, _, qname) <- reverse (concat sources) ]

-- | Compute transitive closure of TyCons reachable from all binder types,
-- expanding through newtypes, then return metadata for all their DataCons.
collectTransitiveDCons :: [CoreBind] -> [(Word64, Text, Int, Int, [Text], Text)]
collectTransitiveDCons binds =
  let binderTypes = [ idType b | b <- concatMap bindersOfBind binds ]
      seedTyCons  = filter (not . isGhcCompilerTyCon)
                      (foldMap (nonDetEltsUniqSet . tyConsOfType) binderTypes)
      allTyCons   = closeTyCons emptyUniqSet seedTyCons
  in  concatMap tyConToDCMeta (nonDetEltsUniqSet allTyCons)
  where
    bindersOfBind (NonRec b _) = [b]
    bindersOfBind (Rec pairs)  = map fst pairs

closeTyCons :: UniqSet TyCon -> [TyCon] -> UniqSet TyCon
closeTyCons visited []     = visited
closeTyCons visited (tc:rest)
  | tc `elementOfUniqSet` visited = closeTyCons visited rest
  -- Never enter the GHC compiler library's type closure (e.g. DynFlags): it is
  -- enormous and only reachable from compile-time-only TH binders. See
  -- 'isGhcCompilerName'.
  | isGhcCompilerTyCon tc         = closeTyCons visited rest
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

tyConToDCMeta :: TyCon -> [(Word64, Text, Int, Int, [Text], Text)]
tyConToDCMeta tc = case tyConDataCons_maybe tc of
  Just dcs -> map (\dc ->
    ( varId (dataConWorkId dc)
    , T.pack (occNameString (nameOccName (dataConName dc)))
    , dataConTag dc
    , valueRepArity dc
    , map mapBang (dataConSrcBangs dc)
    , qualifiedName (dataConName dc)
    )) dcs
  Nothing  -> []

translate :: CoreExpr -> TransM Int
translate expr =
  let (hd, allArgs) = collectArgs expr
      args = filter isValueArg allArgs
  in case hd of
    -- Intercept showDouble: emit ShowDoubleAddr primop + unpackCString loop
    Var v | isShowDoubleVar v
          , [arg] <- args -> do
        argIdx <- translate arg
        addrIdx <- emitNode $ NPrimOp (T.pack "ShowDoubleAddr") [argIdx]
        emitRuntimeUnpackCString addrIdx

    -- Eta-expanded case: showDouble' = $fShowDouble_$cshow (bare Var, no args)
    -- Emit \d -> unpackCString# (ShowDoubleAddr d) so the binding has a valid body.
    Var v | isShowDoubleVar v
          , null args -> do
        let paramVarId = varId v .|. 0x01  -- unique param id
        paramRef <- emitNode $ NVar paramVarId
        addrIdx <- emitNode $ NPrimOp (T.pack "ShowDoubleAddr") [paramRef]
        resultIdx <- emitRuntimeUnpackCString addrIdx
        emitNode $ NLam paramVarId resultIdx

    -- Intercept $fShowDouble_$sshowSignedFloat (GHC's specialized show for Double).
    -- Takes 4 args: (fmt, minExpt, d :: Double, rest :: String).
    -- We use ShowDoubleAddr for the Double, and append rest via unpackAppendCString.
    Var v | isShowDoubleSpecVar v -> do
        case drop 2 args of  -- skip fmt, minExpt → [d, rest, ...]
          (dArg : restArg : _) -> do
            argIdx <- translate dArg
            addrIdx <- emitNode $ NPrimOp (T.pack "ShowDoubleAddr") [argIdx]
            restIdx <- translate restArg
            emitRuntimeUnpackAppendCString addrIdx restIdx
          [dArg] -> do
            -- 3 args applied (fmt, minExpt, d); returns ShowS = String -> String
            argIdx <- translate dArg
            addrIdx <- emitNode $ NPrimOp (T.pack "ShowDoubleAddr") [argIdx]
            restParamId <- freshSynthVarId
            restRef <- emitNode $ NVar restParamId
            resultIdx <- emitRuntimeUnpackAppendCString addrIdx restRef
            emitNode $ NLam restParamId resultIdx
          [] -> do
            -- Partial application / eta-reduced: emit full lambda wrapper
            emitShowDoubleSpecBody v

    -- Intercept Data.Text.empty
    -- We construct the Text constructor directly: Text ByteArray# 0 0.
    -- We use a LitString for the ByteArray# field; tidepool-bridge supports this
    -- fallback in its FromCore implementation.
    Var v | isDataTextEmptyVar v -> do
        case splitTyConApp_maybe (idType v) of
          Just (tc, _) -> case tyConDataCons tc of
            (dc:_) -> do
              recordDC dc
              let textId = varId (dataConWorkId dc)
              baLit <- emitNode $ NLit (LEString BS.empty)
              int0  <- emitNode $ NLit (LEInt 0)
              emitNode $ NCon textId [baLit, int0, int0]
            [] -> error "Data.Text.empty has TyCon with no DataCons"
          Nothing -> error "Data.Text.empty type is not a TyConApp"

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

    -- Fallback: unpackCString# with non-static Addr# (e.g., computed via plusAddr#).
    -- Desugar to runtime iteration using IndexCharOffAddr/PlusAddr primops.
    Var v | isUnpackCStringVar v
          , [arg] <- args -> do
        return () -- unpackCString# non-literal fallback
        argIdx <- translate arg
        emitRuntimeUnpackCString argIdx

    -- Fallback: unpackAppendCString# with non-static addr (e.g., show generates these)
    -- Desugar to runtime iteration: go addr suffix where
    --   go a s = case indexCharOffAddr# a 0# of { '\0'# -> s; c -> C# c : go (plusAddr# a 1#) s }
    Var v | isUnpackAppendCStringVar v
          , [litArg, suffixArg] <- args
          , Nothing <- extractAddrLitBytes litArg -> do
        return () -- unpackAppendCString# non-literal fallback
        litIdx <- translate litArg
        suffixIdx <- translate suffixArg
        emitRuntimeUnpackAppendCString litIdx suffixIdx

    -- Partial application of unpackAppendCString# (1 arg only — produces a lambda)
    -- unpackAppendCString# addr → \suffix -> go addr suffix
    Var v | isUnpackAppendCStringVar v
          , [litArg] <- args -> do
        return () -- unpackAppendCString# partial apply
        case extractAddrLitBytes litArg of
          Just bytes -> do
            -- Static: build \suffix -> "prefix" ++ suffix (cons chain ending with suffix)
            sufId <- freshSynthVarId
            sufRef <- emitNode $ NVar sufId
            let consId = varId (dataConWorkId consDataCon)
                charId = varId (dataConWorkId charDataCon)
            recordDC consDataCon
            recordDC charDataCon
            bodyIdx <- foldM (\acc byte -> do
                unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
                charIdx <- emitNode $ NCon charId [unboxedCharIdx]
                emitNode $ NCon consId [charIdx, acc]
              ) sufRef (reverse bytes)
            emitNode $ NLam sufId bodyIdx
          Nothing -> do
            -- Dynamic: build \suffix -> runtime unpackAppend
            litIdx <- translate litArg
            sufId <- freshSynthVarId
            sufRef <- emitNode $ NVar sufId
            bodyIdx <- emitRuntimeUnpackAppendCString litIdx sufRef
            emitNode $ NLam sufId bodyIdx

    -- Zero-arg unpackAppendCString# (eta-reduced): emit as \addr -> \suffix -> go addr suffix
    Var v | isUnpackAppendCStringVar v
          , null args -> do
        return () -- unpackAppendCString# zero-arg (eta-reduced)
        adrId <- freshSynthVarId
        sufId <- freshSynthVarId
        adrRef <- emitNode $ NVar adrId
        sufRef <- emitNode $ NVar sufId
        bodyIdx <- emitRuntimeUnpackAppendCString adrRef sufRef
        lamSuf <- emitNode $ NLam sufId bodyIdx
        emitNode $ NLam adrId lamSuf

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

    -- Intercept error calls to preserve message string
    Var v | isErrorVar v -> do
      hIdx <- emitNode $ NVar 0x4500000000000002
      let findMsg [] = Nothing
          findMsg (a:as) = case extractErrorMessage a of
                             Just bs -> Just bs
                             Nothing -> findMsg as
      case findMsg (reverse args) of
        Just bytes -> do
          msgIdx <- emitNode $ NLit (LEString (BS.pack bytes))
          emitNode $ NApp hIdx msgIdx
        Nothing ->
          foldM (\fIdx aArg -> do
            aIdx <- translate aArg
            emitNode $ NApp fIdx aIdx) hIdx args

    -- Desugar unpackFoldrCString# "lit"# f z → f (C# c1) (f (C# c2) (... (f (C# cn) z)))
    -- GHC's build/foldr fusion rewrites foldr/build pairs into unpackFoldrCString#,
    -- whose unfolding uses plusAddr#/indexCharOffAddr# (Addr# pointer arithmetic).
    -- We intercept and expand statically to avoid needing Addr# primops.
    Var v | isUnpackFoldrCStringVar v
          , [litArg, fArg, zArg] <- args
          , Just bytes <- extractAddrLitBytes litArg -> do
        zIdx <- translate zArg
        fIdx <- translate fArg
        let charId = varId (dataConWorkId charDataCon)
        recordDC charDataCon
        foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            fCharIdx <- emitNode $ NApp fIdx charIdx
            emitNode $ NApp fCharIdx acc
          ) zIdx (reverse bytes)

    -- Partial application of unpackFoldrCString# (2 args: lit + f).
    -- GHC.CString's rules produce these under build/augment
    -- (unpackCString# a = build (unpackFoldrCString# a), matchers can leave
    -- the f-applied form). Eta-expand the missing z and expand statically.
    -- Without this the head falls through to a bare NVar that nothing can
    -- ever bind (Resolve skips magic unpack vars) — a dangling reference
    -- that traps at runtime only when the (usually dead, e.g. error-message)
    -- branch is forced.
    Var v | isUnpackFoldrCStringVar v
          , [litArg, fArg] <- args
          , Just bytes <- extractAddrLitBytes litArg -> do
        fIdx <- translate fArg
        zId <- freshSynthVarId
        zRef <- emitNode $ NVar zId
        let charId = varId (dataConWorkId charDataCon)
        recordDC charDataCon
        bodyIdx <- foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            fCharIdx <- emitNode $ NApp fIdx charIdx
            emitNode $ NApp fCharIdx acc
          ) zRef (reverse bytes)
        emitNode $ NLam zId bodyIdx

    -- Partial application of unpackFoldrCString# (1 arg: lit only, the
    -- build/augment argument shape). Eta-expand f and z.
    Var v | isUnpackFoldrCStringVar v
          , [litArg] <- args
          , Just bytes <- extractAddrLitBytes litArg -> do
        fId <- freshSynthVarId
        zId <- freshSynthVarId
        fRef <- emitNode $ NVar fId
        zRef <- emitNode $ NVar zId
        let charId = varId (dataConWorkId charDataCon)
        recordDC charDataCon
        bodyIdx <- foldM (\acc byte -> do
            unboxedCharIdx <- emitNode $ NLit (LEChar (fromIntegral byte))
            charIdx <- emitNode $ NCon charId [unboxedCharIdx]
            fCharIdx <- emitNode $ NApp fRef charIdx
            emitNode $ NApp fCharIdx acc
          ) zRef (reverse bytes)
        lamZ <- emitNode $ NLam zId bodyIdx
        emitNode $ NLam fId lamZ

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
        leResult <- emitOp (T.pack "IntLe") [iRef2, lit0]
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

    -- Unboxed 1-tuple (# x #) has no runtime representation: it IS its single
    -- field. GHC introduces these (MkSolo#) to wrap a representation-polymorphic
    -- value — e.g. the ReadP CPS function of type `forall b. (a -> P b) -> P b`.
    -- Erase the Con to its field, symmetric with the case side, where
    -- `case scrut of (# x #) -> body` binds x = scrut (identity — see the
    -- single value-binder isUnboxedTupleDataCon branch below). Boxing it here
    -- would leave a constructor in function position when the field is later
    -- applied → BadFunPtrTag (JIT) / NotAFunction (eval). Nullary (# #) and
    -- multi-element (# a, b #) builds keep the NCon path (state token /
    -- heap-boxed, matching their case branches).
    Var v | Just dc <- isDataConWorkId_maybe v
          , isUnboxedTupleDataCon dc
          , length args == valueRepArity dc
          , [singleArg] <- args ->
        translate singleArg
    Var v | Just dc <- isDataConWorkId_maybe v
          , length args == valueRepArity dc -> do
        recordDC dc
        childIdxs <- mapM translate args
        emitNode $ NCon (varId v) childIdxs

    -- DataCon wrapper Ids: the wrapper takes *boxed* args (e.g., ByteArray, Int)
    -- but the worker representation stores *unboxed* fields.
    -- We keep the boxing in place (translate args normally, no stripBoxCon) so that
    -- Case expressions matching on these fields (e.g. matching I# in Text offset)
    -- see proper Con values. The codegen's recursive unbox_* helpers handle both
    -- boxed and unboxed values when primops need the raw Int#.
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

    -- runRW# :: (State# RealWorld -> o) -> o
    -- Underlying primop for unsafePerformIO / unsafeDupablePerformIO.
    -- Pure library code (Data.Text, etc.) uses unsafePerformIO internally for
    -- buffer allocation, and --all-closed inlining exposes the runRW# call.
    -- Desugar: runRW# f  →  f ()   (state token is erased at runtime)
    Var v | isRunRWVar v
          , [f] <- args -> do
      recordDC unitDataCon
      fIdx <- translate f
      tokIdx <- emitNode $ NCon (varId (dataConWorkId unitDataCon)) []
      emitNode $ NApp fIdx tokIdx

    -- runRW# applied to zero args (rare, but handle gracefully as a lambda)
    Var v | isRunRWVar v
          , null args -> do
      recordDC unitDataCon
      argId <- freshSynthVarId
      tokIdx <- emitNode $ NCon (varId (dataConWorkId unitDataCon)) []
      argRef <- emitNode $ NVar argId
      body <- emitNode $ NApp argRef tokIdx
      emitNode $ NLam argId body

    -- nospec :: a -> a  (GHC.Magic) — the identity, inserted by the specializer
    -- (once Opt_Specialise is on) to block over-specialization of dictionary /
    -- class-method code. It has no unfolding, so the JIT can't link it; desugar
    -- `nospec @t f x...` → `f x...` (drop the wrapper, apply its first value arg
    -- to the rest). See plans/send-print-unresolved-bug.md.
    Var v | isNospecVar v
          , (f:rest) <- args -> do
      fIdx <- translate f
      restIdxs <- mapM translate rest
      foldM (\acc aIdx -> emitNode $ NApp acc aIdx) fIdx restIdxs

    -- tagToEnum# @T arg → case arg of { 0# → C0; 1# → C1; ... }
    -- We desugar here because type information is erased downstream.
    Var v | Just pop <- isPrimOpId_maybe v
          , pop == TagToEnumOp
          , [arg] <- args -> do
        let typeArgs = filter (not . isValueArg) allArgs
        case typeArgs of
          [Type ty] | Just (tc, _) <- splitTyConApp_maybe ty -> do
            let dcs = tyConDataCons tc
            argIdx <- translate arg
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
        emitPrimOpDispatch pop childIdxs

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
    
    -- Foreign calls: map known FFI functions to our primops; unsupported ones
    -- (often over-collected into a closure, in a dead branch) become poisons.
    Var v | isFCallId v -> do
        let pprName = showPprUnsafe v
        childIdxs <- mapM translate args
        case mapFfiCall pprName of
          Just name -> emitOp name childIdxs
          Nothing   -> emitFfiPoison

    _ -> do
      hIdx <- translateHead hd
      foldM (\fIdx arg -> do
        aIdx <- translate arg
        emitNode $ NApp fIdx aIdx) hIdx args

emitOp :: Text -> [Int] -> TransM Int
emitOp name args = emitNode $ NPrimOp name args

translateHead :: CoreExpr -> TransM Int
translateHead = \case
  Var v
    | isRuntimeErrorVar v -> do
        let kind = if occNameString (nameOccName (idName v)) == "divZeroError" then 0 else 1
        emitNode $ NVar (0x4500000000000000 .|. kind)  -- tag 'E' for error
    | isErrorVar v -> emitNode $ NVar 0x4500000000000002  -- tag 'E', kind 2 (error)
    | isUndefinedVar v -> emitNode $ NVar 0x4500000000000003  -- tag 'E', kind 3 (undefined)
    | isRealWorldVar v ->
        emitNode $ NLit (LEInt 0)  -- realWorld# state token → dummy literal
    | isTypeMetadataVar v ->
        emitNode $ NVar 0x4500000000000004  -- tag 'E', kind 4 (type metadata)
    | isNospecVar v -> do
        -- GHC.Magic.nospec is the identity; bare / zero-value-arg occurrence
        -- (the applied form is desugared in the App handler). Emit `\x -> x`.
        argId <- freshSynthVarId
        argRef <- emitNode $ NVar argId
        emitNode $ NLam argId argRef
    | otherwise -> do
        unresolved <- gets (Set.member (varId v) . tsUnresolvedIds)
        if unresolved
          then emitNode $ NVar 0x4500000000000004
          else emitNode $ NVar (varId v)
  Lit l -> emitNode $ NLit (mapLit l)
  Lam b body
    | isTyVar b -> translate body
    | otherwise -> do
        bodyIdx <- translate body
        emitNode $ NLam (varId b) bodyIdx
  Let (NonRec b rhs) body
    | Just arity <- isJoinId_maybe b
    , jumpCrossesLam (varId b) body -> do
        -- Join point is used inside a lambda in the body — can't compile as
        -- a Cranelift block (lambdas are separate functions). Convert to a
        -- regular LetNonRec with a lambda wrapper, same as Rec join handling.
        let (params, joinBody) = collectValueBinders arity rhs
        joinBodyIdx <- translate joinBody
        rhsIdx <- foldM (\inner p -> emitNode $ NLam (varId p) inner)
                        joinBodyIdx (reverse params)
        modify' $ \s -> s { tsRecJoinIds = Set.insert (varId b) (tsRecJoinIds s) }
        bodyIdx <- translate body
        emitNode $ NLetNonRec (varId b) rhsIdx bodyIdx
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
          when joinrecDebugEnabled $
            Debug.Trace.traceM ("[313-joinrec] " ++ occNameString (nameOccName (idName b))
              ++ " varId=" ++ showHex' (varId b)
              ++ " params=" ++ show (map (showHex' . varId) params))
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
  Case scrut _caseBinder _ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc
    , (Var v, allArgs) <- collectArgs (stripTicksAndCasts scrut)
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name) <- splitMultiReturnPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a, b] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [qBinder, rBinder] <- vBinders -> do
        aIdx <- translate a
        bIdx <- translate b
        qValIdx <- emitOp op1Name [aIdx, bIdx]
        rValIdx <- emitOp op2Name [aIdx, bIdx]
        -- Bind q and r using Case to force them, then translate body
        bodyIdx <- translate body
        -- case rVal of rBinder { DEFAULT -> body }
        rCaseIdx <- emitNode $ NCase rValIdx (varId rBinder) [FlatAlt FDefault [] bodyIdx]
        -- case qVal of qBinder { DEFAULT -> rCaseIdx }
        emitNode $ NCase qValIdx (varId qBinder) [FlatAlt FDefault [] rCaseIdx]
  -- 3-input / 2-output: case quotRemWord2# hi lo d of (# q, r #) -> body
  Case scrut _caseBinder _ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc
    , (Var v, allArgs) <- collectArgs (stripTicksAndCasts scrut)
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name) <- splitWord2DivPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a, b, c] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [qBinder, rBinder] <- vBinders -> do
        aIdx <- translate a
        bIdx <- translate b
        cIdx <- translate c
        qValIdx <- emitOp op1Name [aIdx, bIdx, cIdx]
        rValIdx <- emitOp op2Name [aIdx, bIdx, cIdx]
        bodyIdx <- translate body
        rCaseIdx <- emitNode $ NCase rValIdx (varId rBinder) [FlatAlt FDefault [] bodyIdx]
        emitNode $ NCase qValIdx (varId qBinder) [FlatAlt FDefault [] rCaseIdx]
  -- Desugar unary multi-return primops: case decodeDouble_Int64# x of (# m, e #) -> body
  Case scrut _caseBinder _ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc
    , (Var v, allArgs) <- collectArgs (stripTicksAndCasts scrut)
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name) <- splitUnaryMultiReturnPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [r1Binder, r2Binder] <- vBinders -> do
        aIdx <- translate a
        v1Idx <- emitOp op1Name [aIdx]
        v2Idx <- emitOp op2Name [aIdx]
        bodyIdx <- translate body
        c1 <- emitNode $ NCase v2Idx (varId r2Binder) [FlatAlt FDefault [] bodyIdx]
        emitNode $ NCase v1Idx (varId r1Binder) [FlatAlt FDefault [] c1]
  -- Desugar triple-return primops: case timesInt2# a b of (# hi, lo, ovf #) -> body
  Case scrut _caseBinder _ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc
    , (Var v, allArgs) <- collectArgs (stripTicksAndCasts scrut)
    , Just pop <- isPrimOpId_maybe v
    , Just (op1Name, op2Name, op3Name) <- splitTripleReturnPrimOp pop
    , let valArgs = filter isValueArg allArgs
    , [a, b] <- valArgs
    , vBinders <- filter (not . isTyVar) binders
    , [b1, b2, b3] <- vBinders -> do
        aIdx <- translate a
        bIdx <- translate b
        v1Idx <- emitOp op1Name [aIdx, bIdx]
        v2Idx <- emitOp op2Name [aIdx, bIdx]
        v3Idx <- emitOp op3Name [aIdx, bIdx]
        bodyIdx <- translate body
        c3 <- emitNode $ NCase v3Idx (varId b3) [FlatAlt FDefault [] bodyIdx]
        c2 <- emitNode $ NCase v2Idx (varId b2) [FlatAlt FDefault [] c3]
        emitNode $ NCase v1Idx (varId b1) [FlatAlt FDefault [] c2]
  -- Desugar stateful primop/FFI calls returning unboxed tuples with a state token.
  -- Pattern: case op args... s of (# s', results... #) -> body
  -- Where op is a primop or FFI call and the case unpacks an unboxed tuple.
  -- The state token (s and s') is erased, so we:
  --   1. Drop the state token arg from the primop call
  --   2. For 1 result binder: case op args of result { DEFAULT -> body }
  --   3. For 0 result binders (void ops like write): run op, then body
  Case scrut _caseBinder _ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc
    , (Var v, allArgs) <- collectArgs (stripTicksAndCasts scrut)
    , isPrimOpId_maybe v /= Nothing || isFCallId v
    , let valArgs = filter isValueArg allArgs
    -- Only drop the last value arg if the first result binder has State# type
    -- (stateful primops like readSmallArray#). For pure primops returning unboxed
    -- tuples (like indexSmallArray# :: SmallArray# a -> Int# -> (# a #)), keep all args.
    , vBinders <- filter (not . isTyVar) binders
    , let hasStateBinder = case vBinders of
            (b:_) -> case splitTyConApp_maybe (idType b) of
                       Just (tc, _) -> tc == statePrimTyCon
                       Nothing      -> False
            _ -> False
    , let nonStateArgs = if hasStateBinder
                         then case valArgs of { [] -> []; _ -> init valArgs }
                         else valArgs
    -> do
        childIdxs <- mapM translate nonStateArgs
        -- Emit the primop or FFI call (unsupported FFI -> lazy poison).
        primIdx <- case isPrimOpId_maybe v of
                    Just pop -> emitPrimOpDispatch pop childIdxs
                    Nothing  -> case mapFfiCall (showPprUnsafe v) of
                                  Just name -> emitOp name childIdxs
                                  Nothing   -> emitFfiPoison
        if hasStateBinder then do
          -- Stateful primop: bind s' (state token) to dummy, bind results to primop
          dummyState <- emitNode $ NLit (LEInt 0)
          case vBinders of
            [s']           -> do
              -- Void op (e.g. writeWord8Array#): force primop for side effects, bind s'
              bodyIdx <- translate body
              inner <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              emitNode $ NCase primIdx (varId s') [FlatAlt FDefault [] inner]
            [s', result]   -> do
              bodyIdx <- translate body
              inner <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              emitNode $ NCase primIdx (varId result) [FlatAlt FDefault [] inner]
            [s', r1, r2]   -> do
              bodyIdx <- translate body
              c2 <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              c1 <- emitNode $ NCase primIdx (varId r2) [FlatAlt FDefault [] c2]
              emitNode $ NCase primIdx (varId r1) [FlatAlt FDefault [] c1]
            [s', r1, r2, r3] -> do
              bodyIdx <- translate body
              c3 <- emitNode $ NCase dummyState (varId s') [FlatAlt FDefault [] bodyIdx]
              c2 <- emitNode $ NCase primIdx (varId r3) [FlatAlt FDefault [] c3]
              c1 <- emitNode $ NCase primIdx (varId r2) [FlatAlt FDefault [] c2]
              emitNode $ NCase primIdx (varId r1) [FlatAlt FDefault [] c1]
            _ -> error $ "Unsupported stateful unboxed tuple arity: " ++ show (length vBinders) ++ " binders"
        else do
          -- Pure primop returning unboxed tuple (e.g. indexSmallArray# -> (# a #))
          -- No state token: bind results directly to primop output
          case vBinders of
            [result] -> do
              bodyIdx <- translate body
              emitNode $ NCase primIdx (varId result) [FlatAlt FDefault [] bodyIdx]
            [r1, r2] -> do
              -- Pure 2-result primop (e.g. decodeDouble_Int64#, casSmallArray#)
              bodyIdx <- translate body
              c1 <- emitNode $ NCase primIdx (varId r2) [FlatAlt FDefault [] bodyIdx]
              emitNode $ NCase primIdx (varId r1) [FlatAlt FDefault [] c1]
            _ -> error $ "Unsupported pure unboxed tuple arity: " ++ show (length vBinders) ++ " binders"
  Case scrut b _alts_ty [Alt (DataAlt dc) binders body]
    | isUnboxedTupleDataCon dc -> do
        scrutIdx <- translate scrut
        let vBinders = filter (not . isTyVar) binders
        bodyIdx <- translate body
        case vBinders of
          [valBinder] -> do
            -- Single-element unboxed tuple: use FDefault to handle both Lit and Con.
            -- This happens when a primop returns a raw literal that GHC wraps in (# #).
            emitNode $ NCase scrutIdx (varId valBinder) [FlatAlt FDefault [] bodyIdx]
          [] -> do
            -- Zero-element unboxed tuple: use FDefault, bind to dummy.
            emitNode $ NCase scrutIdx (varId b) [FlatAlt FDefault [] bodyIdx]
          _ -> do
            -- Multi-element: must be a heap box, use FDataAlt to bind fields.
            recordDC dc
            emitNode $ NCase scrutIdx (varId b) [FlatAlt (FDataAlt (varId (dataConWorkId dc))) (map varId vBinders) bodyIdx]
  -- unsafeEqualityProof: elide the case entirely.
  -- GHC emits `case unsafeEqualityProof of UnsafeRefl -> body` for GADT evidence
  -- (e.g. freer-simple's Member constraint). After cross-module inlining via
  -- resolveExternals, these cases survive because GHC's optimizer ran before
  -- the bindings were merged. The UnsafeRefl constructor always matches, so we
  -- emit the body directly. Without this, the translator emits Con_unit for
  -- unsafeEqualityProof but the case alt expects Con_UnsafeRefl, causing a
  -- tag mismatch (CASE TRAP) at runtime.
  Case scrut _b _alts_ty [Alt (DataAlt _dc) _binders body]
    | isUnsafeEqualityCase scrut ->
        translate body
  Case scrut b _alts_ty alts -> do
    scrutIdx <- translate scrut
    altData <- mapM translateAlt alts
    emitNode $ NCase scrutIdx (varId b) altData
  Cast e _ -> translate e
  Tick _ e -> translate e
  Type _ -> error "Bare Type in expression position"
  -- Coercions are zero-cost type evidence (newtype proofs). They appear in
  -- expression position when GHC inlines through vendored code compiled from
  -- source (e.g., newtype Key = Key Text). Emit unit literal as a placeholder.
  Coercion _ -> emitNode $ NLit (LEInt 0)
  App _ _ -> error "App should be handled by translate"
  _ -> error "Unexpected expression form"

translateAlt :: CoreAlt -> TransM FlatAlt
translateAlt (Alt con binders body) = do
  -- Keep only VALUE binders. A GADT pattern's Core binders include the
  -- equality-evidence coercion var (e.g. `AddE co a b` for
  -- `AddE :: Expr Int -> Expr Int -> Expr Int`), which is a CoVar — NOT a
  -- TyVar — so filtering `isTyVar` alone leaves it in, binding one too many
  -- fields. The Con BUILD drops it (via `isValueArg`, which excludes both type
  -- AND coercion args / `valueRepArity = dataConRepArity - |eqSpec|`), so an
  -- unfiltered alt reads past the stored fields: eval ArityMismatch, JIT SIGSEGV.
  -- Exclude coercion binders too, matching the build's value-field count.
  let vBinders = filter (\b -> not (isTyVar b) && not (isCoVar b)) binders
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
varId v = case isDataConId_maybe v of
  Just dc -> stableVarId (varName (dataConWorkId dc))
  Nothing
    | isExternalName (varName v) -> stableVarId (varName v)
    | otherwise                  -> localVarId v

-- | For local (non-external) variables, hash the OccName together with the
-- GHC unique to produce a disambiguated ID. Raw GHC uniques collide across
-- modules after cross-module inlining: e.g., unique (X, 12) may appear in
-- 63 different inlined bindings with names like exit_Xc, ww_Xc, ds_Xc.
-- Including the OccName in the hash disambiguates them.
localVarId :: Var -> Word64
localVarId v =
  let k = getKey (varUnique v)
      occ = occNameString (nameOccName (varName v))
      combined = occ ++ "#" ++ show k
      Fingerprint h1 _ = fingerprintString combined
  in h1 .&. 0x00FFFFFFFFFFFFFF

-- | Normalize a module name by stripping ".Internal" / "Internal." segments.
normalizeMod :: String -> String
normalizeMod s =
  let t = T.pack s
      t1 = T.replace ".Internal" "" t
      t2 = T.replace "Internal." "" t1
  in T.unpack t2

-- | Module-qualified name for a DataCon (e.g. "Data.Map.Bin").
-- Falls back to just the OccName for wired-in names without a module.
qualifiedName :: Name -> Text
qualifiedName name = case nameModule_maybe name of
  Just m  -> T.pack (normalizeMod (moduleNameString (moduleName m)) ++ "." ++ occNameString (nameOccName name))
  Nothing -> T.pack (occNameString (nameOccName name))

stableVarId :: Name -> Word64
stableVarId name = stableVarIdWith (fieldParentDisamb name) name

-- | Record fields live in their own 'NameSpace' ('FldName', GHC ≥9.6) carrying
-- the parent type/constructor; fold it into the fingerprint so two records
-- sharing a field label ('DuplicateRecordFields') get DISTINCT ids.
--
-- Supersedes b4e0f8c's @recSelParentKey@, which read 'idDetails' to find the
-- 'RecSelId' parent. Deriving the disambiguator from the 'Name' makes it a pure
-- function of the reference, independent of how the 'Id' was reconstructed: it
-- cannot silently degrade to 'Nothing' on a binder that has lost its 'RecSelId'
-- details (anything rebuilt from fat-interface Core via 'tcTopIfaceBindings'
-- gets vanilla 'idDetails'). This is a simplification-plus-hardening, NOT a fix
-- for a collision reproduced on current source: every dup-field configuration
-- constructible in-tree (home source, and records as a compiled-package
-- dependency, with or without exposed unfoldings) already carried 'RecSelId', so
-- b4e0f8c and this mechanism agree on all of them. The historically-observed
-- collision (0xfea90eccc07baa0f, two TOP @path@ sites in Tidepool.Records) came
-- from a STALE deployed extract binary predating b4e0f8c, not from current
-- source. Contract pinned by @test-varid/VarIdMechanismTest.hs@. Non-field names
-- get @""@ (byte-identical to the original 'stableVarId'), so the DataConTable \/
-- fixture meta is unperturbed.
fieldParentDisamb :: Name -> String
fieldParentDisamb n = case fieldOcc_maybe (nameOccName n) of
  Just parent -> '@' : unpackFS parent
  Nothing     -> ""

-- | 'stableVarId' with an explicit disambiguator folded into the fingerprinted
-- string.  @'stableVarIdWith' "" name@ is byte-identical to the original scheme.
-- Callers outside 'varId' (e.g. 'sessionBinderName') are in the 'VarName'
-- namespace so 'fieldParentDisamb' returns @""@ for them — no change.
stableVarIdWith :: String -> Name -> Word64
stableVarIdWith disamb name =
  let modStr = case nameModule_maybe name of
        Just m  -> normalizeMod (moduleNameString (moduleName m))
        Nothing -> "WiredIn"
      occStr = occNameString (nameOccName name)
      fullStr = modStr ++ ":" ++ occStr ++ disamb
      Fingerprint h1 _ = fingerprintString fullStr
  in (0xFE `shiftL` 56) .|. (h1 .&. 0x00FFFFFFFFFFFFFF)

stripTicksAndCasts :: CoreExpr -> CoreExpr
stripTicksAndCasts (Tick _ e) = stripTicksAndCasts e
stripTicksAndCasts (Cast e _) = stripTicksAndCasts e
stripTicksAndCasts e          = e

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

-- | Strip a single-field box constructor from a wrapper DataCon arg.
-- When a DataCon wrapper is applied, its args are boxed:
--   Text (ByteArray ba#) (I# off#) (I# len#)
-- We need to strip the boxing to get the worker args:
--   Text ba# off# len#
-- This handles I#, W#, ByteArray, and any other single-field product constructor.
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
    -- BigNat# literal (the payload of a big IP/IN Integer literal): materialize
    -- as a little-endian 64-bit-limb ByteArray#. ghc-bignum reads the limb count
    -- from sizeofByteArray#, so the byte length must be exactly the significant
    -- limbs (no extra zero limb). See bigNatLitBytes.
    LitNumBigNat -> LEByteArray (BS.pack (bigNatLitBytes n))
  LitChar c              -> LEChar (fromIntegral (ord c))
  LitString bs           -> LEString bs
  LitFloat r             -> LEFloat (fromIntegral (castFloatToWord32 (fromRational r)))
  LitDouble r            -> LEDouble (castDoubleToWord64 (fromRational r))
  LitNullAddr            -> LEInt 0  -- Addr# null → dummy value (dead code path)
  LitLabel{}             -> LEInt 0  -- Function label → dummy value (dead code path)
  LitRubbish{}           -> LEInt 0  -- Rubbish literal → dummy value
  other                  -> error $ "Unsupported literal: " ++ showPprUnsafe other

-- | Little-endian 64-bit-limb bytes for a BigNat# literal payload (ByteArray#).
-- @n@ is the non-negative magnitude (sign lives in the IP/IN constructor).
-- Bytes are padded up to a whole limb; the top limb stays non-zero (normalized),
-- so sizeofByteArray# yields the correct GMP limb count.
bigNatLitBytes :: Integer -> [Word8]
bigNatLitBytes n =
  let go 0 = []
      go k = fromIntegral (k .&. 0xff) : go (k `shiftR` 8)
      raw = go n
      pad = (8 - length raw `mod` 8) `mod` 8
  in if null raw then replicate 8 0 else raw ++ replicate pad 0

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
  -- Float unary math with native hardware opcodes (sqrt/fabs). The Float
  -- transcendentals (expFloat#, sinFloat#, …) have no hardware opcode and are
  -- desugared to the Double libm path in `desugarFloatMath` before reaching here.
  FloatSqrtOp   -> "FloatSqrt"
  FloatFabsOp   -> "FloatFabs"
  -- Double extras
  DoubleNegOp   -> "DoubleNegate"
  DoubleFabsOp  -> "DoubleFabs"
  -- Double math (Floating class)
  DoubleSqrtOp  -> "DoubleSqrt"
  DoubleExpOp   -> "DoubleExp"
  DoubleExpM1Op -> "DoubleExpM1"
  DoubleLogOp   -> "DoubleLog"
  DoubleLog1POp -> "DoubleLog1P"
  DoubleSinOp   -> "DoubleSin"
  DoubleCosOp   -> "DoubleCos"
  DoubleTanOp   -> "DoubleTan"
  DoubleAsinOp  -> "DoubleAsin"
  DoubleAcosOp  -> "DoubleAcos"
  DoubleAtanOp  -> "DoubleAtan"
  DoubleSinhOp  -> "DoubleSinh"
  DoubleCoshOp  -> "DoubleCosh"
  DoubleTanhOp  -> "DoubleTanh"
  DoubleAsinhOp -> "DoubleAsinh"
  DoubleAcoshOp -> "DoubleAcosh"
  DoubleAtanhOp -> "DoubleAtanh"
  DoublePowerOp -> "DoublePower"
  -- Type conversions
  IntToDoubleOp   -> "Int2Double"
  WordToDoubleOp  -> "Word2Double"
  DoubleToIntOp   -> "Double2Int"
  IntToFloatOp    -> "Int2Float"
  FloatToIntOp    -> "Float2Int"
  DoubleToFloatOp -> "Double2Float"
  FloatToDoubleOp -> "Float2Double"
  -- Pointer equality (polyfill: always 0# = not equal)
  ReallyUnsafePtrEqualityOp -> "ReallyUnsafePtrEquality"
  -- Addr#
  IndexOffAddrOp_Char -> "IndexCharOffAddr"
  AddrAddOp           -> "PlusAddr"
  AddrEqOp            -> "EqAddr"
  AddrSubOp           -> "MinusAddr"
  IndexByteArrayOp_Addr -> "IndexAddrArray"
  -- Off-addr reads (the stateful `read*OffAddr#` variants share the serial name
  -- with their pure `index*OffAddr#` siblings — the trailing State# arg is erased
  -- and the (# State#, result #) tuple collapses to just the result at emit).
  IndexOffAddrOp_Addr     -> "IndexAddrOffAddr"
  ReadOffAddrOp_Addr      -> "IndexAddrOffAddr"
  IndexOffAddrOp_Int8     -> "IndexInt8OffAddr"
  ReadOffAddrOp_Int8      -> "IndexInt8OffAddr"
  IndexOffAddrOp_Word32   -> "IndexWord32OffAddr"
  ReadOffAddrOp_Word32    -> "IndexWord32OffAddr"
  ReadOffAddrOp_Word8     -> "IndexWord8OffAddr"
  IndexOffAddrOp_WideChar -> "IndexWideCharOffAddr"
  ReadOffAddrOp_WideChar  -> "IndexWideCharOffAddr"
  WriteOffAddrOp_WideChar -> "WriteWideCharOffAddr"
  -- ByteArray#
  NewByteArrayOp_Char         -> "NewByteArray"
  -- Pinned alloc is identical to a normal ByteArray# for us; contents# yields the
  -- payload Addr#. These ride the dead Integer->Addr# serialization closure.
  NewPinnedByteArrayOp_Char        -> "NewByteArray"
  ByteArrayContents_Char           -> "ByteArrayContents"
  MutableByteArrayContents_Char    -> "ByteArrayContents"
  SizeofByteArrayOp           -> "SizeofByteArray"
  SizeofMutableByteArrayOp    -> "SizeofByteArray"
  UnsafeFreezeByteArrayOp     -> "UnsafeFreezeByteArray"
  CopyAddrToByteArrayOp       -> "CopyAddrToByteArray"
  ReadByteArrayOp_Word8       -> "ReadWord8Array"
  WriteByteArrayOp_Word8      -> "WriteWord8Array"
  IndexByteArrayOp_Word       -> "IndexWordArray"
  WriteByteArrayOp_Word       -> "WriteWordArray"
  ReadByteArrayOp_Word        -> "ReadWordArray"
  SetByteArrayOp              -> "SetByteArray"
  ShrinkMutableByteArrayOp_Char -> "ShrinkMutableByteArray"
  IndexByteArrayOp_Word8      -> "IndexWord8Array"
  IndexOffAddrOp_Word8        -> "IndexWord8OffAddr"
  WriteOffAddrOp_Word8        -> "WriteWord8OffAddr"
  CopyByteArrayOp             -> "CopyByteArray"
  CopyMutableByteArrayOp      -> "CopyMutableByteArray"
  CompareByteArraysOp         -> "CompareByteArrays"
  GetSizeofMutableByteArrayOp -> "SizeofByteArray"
  ResizeMutableByteArrayOp_Char -> "ResizeMutableByteArray"
  -- Word8
  Word8ToWordOp               -> "Word8ToWord"
  WordToWord8Op               -> "WordToWord8"
  Word8AddOp                  -> "Word8Add"
  Word8SubOp                  -> "Word8Sub"
  Word8LtOp                   -> "Word8Lt"
  Word8LeOp                   -> "Word8Le"
  Word8GeOp                   -> "Word8Ge"
  Word8GtOp                   -> "Word8Gt"
  Word8QuotOp                 -> "Word8Quot"
  Word8RemOp                  -> "Word8Rem"
  Word8MulOp                  -> "Word8Mul"
  -- Int8
  Int8ToIntOp                 -> "Int8ToInt"
  Int8ToWord8Op               -> "Int8ToWord8"
  Word8ToInt8Op               -> "Word8ToInt8"
  Int8NegOp                   -> "Int8Negate"
  -- Word32 / Int32
  Int32ToIntOp                -> "Int32ToInt"
  Word32ToWordOp              -> "Word32ToWord"
  WordToWord32Op              -> "WordToWord32"
  Word32GtOp                  -> "Word32Gt"
  Word32LeOp                  -> "Word32Le"
  Word32LtOp                  -> "Word32Lt"
  Word32AddOp                 -> "Word32Add"
  Word32SubOp                 -> "Word32Sub"
  -- Int64
  Int64AddOp                  -> "Int64Add"
  Int64SubOp                  -> "Int64Sub"
  Int64MulOp                  -> "Int64Mul"
  Int64NegOp                  -> "Int64Negate"
  Int64SllOp                  -> "Int64Shl"
  Int64SraOp                  -> "Int64Shra"
  Int64LtOp                   -> "Int64Lt"
  Int64LeOp                   -> "Int64Le"
  Int64GtOp                   -> "Int64Gt"
  Int64GeOp                   -> "Int64Ge"
  Int64ToIntOp                -> "Int64ToInt"
  IntToInt64Op                -> "IntToInt64"
  Int64ToWord64Op             -> "Int64ToWord64"
  -- Word64
  Word64ToInt64Op             -> "Word64ToInt64"
  Word64ToWordOp              -> "Word64ToWord"
  WordToWord64Op              -> "WordToWord64"
  Word64SllOp                 -> "Word64Shl"
  Word64SrlOp                 -> "Word64Shrl"
  Word64OrOp                  -> "Word64Or"
  Word64AndOp                 -> "Word64And"
  Word64EqOp                  -> "Word64Eq"
  Word64NeOp                  -> "Word64Ne"
  Word64LtOp                  -> "Word64Lt"
  Word64LeOp                  -> "Word64Le"
  Word64GtOp                  -> "Word64Gt"
  Word64GeOp                  -> "Word64Ge"
  -- Carry arithmetic and wide multiply handled by splitMultiReturnPrimOp / splitTripleReturnPrimOp
  -- CLZ
  Clz8Op                      -> "Clz8"
  ClzOp                       -> "Clz"
  -- SmallArray#
  NewSmallArrayOp             -> "NewSmallArray"
  ReadSmallArrayOp            -> "ReadSmallArray"
  WriteSmallArrayOp           -> "WriteSmallArray"
  IndexSmallArrayOp           -> "IndexSmallArray"
  SizeofSmallArrayOp          -> "SizeofSmallArray"
  SizeofSmallMutableArrayOp   -> "SizeofSmallMutableArray"
  GetSizeofSmallMutableArrayOp -> "SizeofSmallMutableArray"
  UnsafeFreezeSmallArrayOp    -> "UnsafeFreezeSmallArray"
  UnsafeThawSmallArrayOp      -> "UnsafeThawSmallArray"
  CopySmallArrayOp            -> "CopySmallArray"
  CopySmallMutableArrayOp     -> "CopySmallMutableArray"
  CloneSmallArrayOp           -> "CloneSmallArray"
  CloneSmallMutableArrayOp    -> "CloneSmallMutableArray"
  ShrinkSmallMutableArrayOp_Char -> "ShrinkSmallMutableArray"
  CasSmallArrayOp             -> "CasSmallArray"
  -- Array#
  NewArrayOp                  -> "NewArray"
  ReadArrayOp                 -> "ReadArray"
  WriteArrayOp                -> "WriteArray"
  IndexArrayOp                -> "IndexArray"
  SizeofArrayOp               -> "SizeofArray"
  SizeofMutableArrayOp        -> "SizeofMutableArray"
  UnsafeFreezeArrayOp         -> "UnsafeFreezeArray"
  UnsafeThawArrayOp           -> "UnsafeThawArray"
  CopyArrayOp                 -> "CopyArray"
  CopyMutableArrayOp          -> "CopyMutableArray"
  CloneArrayOp                -> "CloneArray"
  CloneMutableArrayOp         -> "CloneMutableArray"
  -- Bit operations
  PopCntOp                    -> "PopCnt"
  PopCnt8Op                   -> "PopCnt8"
  PopCnt16Op                  -> "PopCnt16"
  PopCnt32Op                  -> "PopCnt32"
  PopCnt64Op                  -> "PopCnt64"
  CtzOp                       -> "Ctz"
  Ctz8Op                      -> "Ctz8"
  Ctz16Op                     -> "Ctz16"
  Ctz32Op                     -> "Ctz32"
  Ctz64Op                     -> "Ctz64"
  -- Exception
  RaiseOp     -> "Raise"
  -- Arithmetic exceptions raised in ghc-bignum's check branches.
  RaiseUnderflowOp -> "RaiseUnderflow"
  RaiseOverflowOp  -> "RaiseOverflow"
  RaiseDivZeroOp   -> "RaiseDivZero"
  other       -> error $ "Unsupported primop: " ++ showPprUnsafe other

-- | Float transcendental primops (expFloat#, sinFloat#, …) have no hardware
-- opcode. Map each to its Double-precision sibling's serial name so it can be
-- desugared to the Double libm path (the JIT/eval implement only Double libm).
-- Hardware-opcode Float ops (sqrtFloat#/fabsFloat#) are NOT here — they go
-- through mapPrimOp natively. Returns Nothing for any non-transcendental primop.
floatMathToDouble :: PrimOp -> Maybe Text
floatMathToDouble = \case
  FloatExpOp   -> Just "DoubleExp"
  FloatExpM1Op -> Just "DoubleExpM1"
  FloatLogOp   -> Just "DoubleLog"
  FloatLog1POp -> Just "DoubleLog1P"
  FloatSinOp   -> Just "DoubleSin"
  FloatCosOp   -> Just "DoubleCos"
  FloatTanOp   -> Just "DoubleTan"
  FloatAsinOp  -> Just "DoubleAsin"
  FloatAcosOp  -> Just "DoubleAcos"
  FloatAtanOp  -> Just "DoubleAtan"
  FloatSinhOp  -> Just "DoubleSinh"
  FloatCoshOp  -> Just "DoubleCosh"
  FloatTanhOp  -> Just "DoubleTanh"
  FloatAsinhOp -> Just "DoubleAsinh"
  FloatAcoshOp -> Just "DoubleAcosh"
  FloatAtanhOp -> Just "DoubleAtanh"
  FloatPowerOp -> Just "DoublePower"
  _            -> Nothing

-- | Emit a saturated primop. A Float transcendental is desugared to the Double
-- libm path: each Float arg is promoted (`float2Double#`), the Double op runs,
-- and the result is demoted (`double2Float#`). Everything else (including the
-- native sqrtFloat#/fabsFloat#) goes straight through mapPrimOp.
emitPrimOpDispatch :: PrimOp -> [Int] -> TransM Int
emitPrimOpDispatch pop childIdxs =
  case floatMathToDouble pop of
    Just dop -> do
      promoted <- mapM (\c -> emitOp "Float2Double" [c]) childIdxs
      dres <- emitOp dop promoted
      emitOp "Double2Float" [dres]
    Nothing -> emitOp (mapPrimOp pop) childIdxs

-- | Check whether a named top-level binding has IO in its result type.
targetBindingHasIO :: [CoreBind] -> String -> Bool
targetBindingHasIO binds name =
  case filter isTarget (concatMap bOf binds) of
    (b:_) -> hasIOType (idType b)
    []    -> False
  where
    bOf (NonRec b _) = [b]
    bOf (Rec pairs)  = map fst pairs
    isTarget b = occNameString (nameOccName (idName b)) == name

hasIOType :: Type -> Bool
hasIOType ty = case splitTyConApp_maybe ty of
  Just (tc, _) | getKey (tyConUnique tc) == getKey ioTyConKey -> True
  _ -> case splitFunTy_maybe ty of
    Just (_, _, _, ret) -> hasIOType ret
    Nothing -> False

collectDataCons :: [TyCon] -> [(Word64, Text, Int, Int, [Text], Text)]
collectDataCons tycons =
  [ (varId (dataConWorkId dc), T.pack (occNameString (nameOccName (dataConName dc))), dataConTag dc, valueRepArity dc, map mapBang (dataConSrcBangs dc), qualifiedName (dataConName dc))
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
wiredInDataCons :: [(Word64, Text, Int, Int, [Text], Text)]
wiredInDataCons = concatMap (\dc ->
    [( varId (dataConWorkId dc)
     , T.pack (occNameString (nameOccName (dataConName dc)))
     , dataConTag dc
     , valueRepArity dc
     , map mapBang (dataConSrcBangs dc)
     , qualifiedName (dataConName dc)
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
isErrorVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "error" || name == "errorWithoutStackTrace"
     || name == "patError" || name == "noMethodBindingError"
     || name == "recSelError" || name == "recConError"
     -- Base-library error workers (2026-06-11). These reach us through .hi
     -- unfoldings as floated bindings like `maximum14 = errorEmptyList
     -- "maximum"`; without the sentinel tag the eager Let spine evaluates the
     -- error RHS at SETUP, so e.g. `maximum (enumFromTo 1 10)` died with
     -- "empty list" before its case ever ran (literal lists worked only
     -- because GHC constant-folds them away). errorEmptyList covers the whole
     -- GHC.List family: maximum/minimum/foldr1/foldl1/last/init/cycle.
     || name == "errorEmptyList"
     -- `lastError`/`initError` are GHC.List's bottoming workers for `last []`
     -- and `init []`. With -O2 + cross-module specialization, an `INLINE _Snoc`
     -- lens (`xs ^? _last`) compiles to a specialized worker that passes
     -- `lastError "last"` into a demand-analysis-DEAD fallback arg slot. Without
     -- the sentinel tag the Var is untagged, so the eager App-argument
     -- evaluation forces the bottoming thunk and raises spuriously. Tagging it
     -- lets the codegen route it through a lazy poison (see EmitFrame::RaiseLazy).
     || name == "lastError" || name == "initError"
     || name == "irrefutPatError" || name == "nonExhaustiveGuardsError"
     || name == "assertError" || name == "absentError"
     || name == "divZeroError" || name == "overflowError"
     || name == "underflowError" || name == "ratioZeroDenominatorError"

isUndefinedVar :: Id -> Bool
isUndefinedVar v = occNameString (nameOccName (idName v)) == "undefined"

isUnsafeTakeVar :: Id -> Bool
isUnsafeTakeVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "$wunsafeTake" || name == "unsafeTake"

isRealWorldVar :: Id -> Bool
isRealWorldVar v = occNameString (nameOccName (idName v)) == "realWorld#"

-- | Map a foreign-call's pretty-printed name to a supported primop name, or
-- Nothing if unsupported. Unsupported FFI calls are emitted as LAZY POISONS by
-- the caller (`emitFfiPoison`), not hard errors: GHC over-collects unrelated FFI
-- into a binding's closure (e.g. __hsbase_MD5Init via GHC.Fingerprint reaches
-- rationalToDouble's closure, in a branch never taken for a Double literal). A
-- poison lets such a binding compile and only raises if the FFI is actually
-- forced at runtime — same discipline as the `error` sentinel / unresolved-var
-- poisons. (Integer/Natural now use the native ghc-bignum backend — pure Core,
-- no __gmpn_*/integer_gmp_* FFI — so those arms are gone.)
mapFfiCall :: String -> Maybe Text
mapFfiCall pprName
  | "strlen" `isInfixOf` pprName                = Just (T.pack "FfiStrlen")
  | "rintDouble" `isInfixOf` pprName            = Just (T.pack "FfiRintDouble")
  | "_hs_text_measure_off" `isInfixOf` pprName  = Just (T.pack "FfiTextMeasureOff")
  | "_hs_text_memchr" `isInfixOf` pprName       = Just (T.pack "FfiTextMemchr")
  | "_hs_text_reverse" `isInfixOf` pprName      = Just (T.pack "FfiTextReverse")
  -- Integer/Natural -> Double encoders (RTS primitives, used by both bignum backends).
  | "__int_encodeDouble" `isInfixOf` pprName    = Just (T.pack "FfiIntEncodeDouble")
  | "__word_encodeDouble" `isInfixOf` pprName   = Just (T.pack "FfiWordEncodeDouble")
  | otherwise                                   = Nothing

-- | Emit a lazy poison node for an unsupported (or dead-branch) construct: a
-- tag-'E' UserError Var. The JIT lowers it to a poison closure that only raises
-- when forced/applied, so it is harmless in dead branches.
emitFfiPoison :: TransM Int
emitFfiPoison = emitNode $ NVar 0x4500000000000002

isRuntimeErrorVar :: Id -> Bool
isRuntimeErrorVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "divZeroError" || name == "overflowError"

isUnsafeEqualityProofVar :: Id -> Bool
isUnsafeEqualityProofVar v =
  occNameString (nameOccName (idName v)) == "unsafeEqualityProof"

-- | Check if a scrutinee expression is unsafeEqualityProof (possibly applied
-- to type arguments and wrapped in ticks/casts). Used to elide
-- case-on-UnsafeRefl at the Case level.
isUnsafeEqualityCase :: CoreExpr -> Bool
isUnsafeEqualityCase expr =
  case fst (collectArgs (stripTicksAndCasts expr)) of
    Var v -> isUnsafeEqualityProofVar v
    _     -> False

isRunRWVar :: Id -> Bool
isRunRWVar v = occNameString (nameOccName (idName v)) == "runRW#"

-- | GHC.Magic.nospec :: a -> a — the specializer's identity wrapper (emitted
-- once Opt_Specialise is on). No unfolding, so it can't be resolved as an
-- external; we desugar it to the identity (see the App + translateHead cases).
isNospecVar :: Id -> Bool
isNospecVar v =
     occNameString (nameOccName (idName v)) == "nospec"
  && maybe False ((== "GHC.Magic") . normalizeMod . moduleNameString . moduleName)
           (nameModule_maybe (idName v))

-- | Recognize GHC type-representation metadata vars ($tc*, $trModule*, krep$*, $krep*).
-- These have no runtime semantics and no unfoldings; emit as error VarId.
-- These vars can appear deep inside resolved unfoldings (e.g. Typeable infrastructure)
-- and are not reported by resolveExternals as unresolved.
isTypeMetadataVar :: Id -> Bool
isTypeMetadataVar v =
  let name = occNameString (nameOccName (idName v))
  in any (`isPrefixOf` name) ["$trModule", "$krep", "$tc", "krep$", "tr$Module"]

isDataTextEmptyVar :: Id -> Bool
isDataTextEmptyVar v =
  let occ = occNameString (nameOccName (idName v))
      modStr = case nameModule_maybe (idName v) of
                 Just m  -> moduleNameString (moduleName m)
                 Nothing -> ""
  in occ == "empty" && (modStr == "Data.Text" || modStr == "Data.Text.Internal")

isUnpackCStringVar :: Id -> Bool
isUnpackCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackCString#" || name == "unpackCStringUtf8#"

isShowDoubleVar :: Id -> Bool
isShowDoubleVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "showDouble" || name == "showDouble'"
     || name == "$fShowDouble_$cshow"

-- | Recognize GHC's specialized showSignedFloat for Double.
-- GHC -O2 specializes show @Double into $fShowDouble_$sshowSignedFloat
-- which takes 4 args: (fmt, minExpt, d :: Double, rest :: String).
-- We intercept this to avoid pulling in the floatToDigits/Integer pipeline.
isShowDoubleSpecVar :: Id -> Bool
isShowDoubleSpecVar v =
  let name = occNameString (nameOccName (idName v))
  in "$fShowDouble_$sshowSignedFloat" `isPrefixOf` name

-- | Recognize GHC's unpackAppendCString# builtin.
-- unpackAppendCString# :: Addr# -> [Char] -> [Char]
-- Prepends a C string literal to a suffix list.
isUnpackAppendCStringVar :: Id -> Bool
isUnpackAppendCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackAppendCString#"

-- | Recognize GHC's unpackFoldrCString# builtin.
-- unpackFoldrCString# :: Addr# -> (Char -> a -> a) -> a -> a
-- GHC's build/foldr fusion rewrites to this; its unfolding uses plusAddr#.
isUnpackFoldrCStringVar :: Id -> Bool
isUnpackFoldrCStringVar v =
  let name = occNameString (nameOccName (idName v))
  in name == "unpackFoldrCString#" || name == "unpackFoldrCStringUtf8#"

-- | Recognize primops that return unboxed tuples and can be split into
-- two individual primops. Returns (primop1, primop2) text names.
splitMultiReturnPrimOp :: PrimOp -> Maybe (Text, Text)
splitMultiReturnPrimOp = \case
  IntQuotRemOp  -> Just (T.pack "IntQuot", T.pack "IntRem")
  WordQuotRemOp -> Just (T.pack "WordQuot", T.pack "WordRem")
  Word8QuotRemOp -> Just (T.pack "Word8Quot", T.pack "Word8Rem")
  IntAddCOp     -> Just (T.pack "AddIntCVal", T.pack "AddIntCCarry")
  IntSubCOp     -> Just (T.pack "SubIntCVal", T.pack "SubIntCCarry")
  WordAddCOp    -> Just (T.pack "AddWordCVal", T.pack "AddWordCCarry")
  WordSubCOp    -> Just (T.pack "SubWordCVal", T.pack "SubWordCCarry")
  WordMul2Op    -> Just (T.pack "TimesWord2Hi", T.pack "TimesWord2Lo")
  WordAdd2Op    -> Just (T.pack "WordAdd2Hi", T.pack "WordAdd2Lo")
  _             -> Nothing

-- | 3-input / 2-output primops: @quotRemWord2# high low divisor -> (# q, r #)@.
-- Like 'splitMultiReturnPrimOp' but the scrutinee op takes THREE value args.
splitWord2DivPrimOp :: PrimOp -> Maybe (Text, Text)
splitWord2DivPrimOp = \case
  WordQuotRem2Op -> Just (T.pack "WordQuotRem2Quot", T.pack "WordQuotRem2Rem")
  _              -> Nothing

-- | Like splitMultiReturnPrimOp but for primops returning 3-element unboxed tuples.
splitTripleReturnPrimOp :: PrimOp -> Maybe (Text, Text, Text)
splitTripleReturnPrimOp = \case
  -- timesInt2# returns (# isHighNeeded#, high#, low# #) — the FIRST component is
  -- the overflow flag, not the high word. The native ghc-bignum backend's
  -- integerMul small path relies on this exact order (it was dormant under the
  -- gmp backend, which multiplied via FFI).
  IntMul2Op -> Just (T.pack "TimesInt2Overflow", T.pack "TimesInt2Hi", T.pack "TimesInt2Lo")
  _         -> Nothing

-- | Like splitMultiReturnPrimOp but for unary primops (single argument)
-- returning unboxed tuples.
splitUnaryMultiReturnPrimOp :: PrimOp -> Maybe (Text, Text)
splitUnaryMultiReturnPrimOp = \case
  DoubleDecode_Int64Op -> Just (T.pack "DecodeDoubleMantissa", T.pack "DecodeDoubleExponent")
  _                    -> Nothing

-- | Extract error message from an expression.
-- Handles both direct LitString and unpackCString# applications,
-- and recursively peels off PushCallStack wrappers.
extractErrorMessage :: CoreExpr -> Maybe [Word8]
extractErrorMessage expr =
  case collectArgs (stripTicksAndCasts expr) of
    (Var v, [arg]) | isUnpackCStringVar v -> extractAddrLitBytes arg
    (Var v, args) | occNameString (nameOccName (idName v)) == "PushCallStack"
                  , (msg:_) <- filter isValueArg args -> extractErrorMessage msg
    _ -> case stripTicksAndCasts expr of
           Lit (LitString bs) -> Just (BS.unpack bs)
           _ -> Nothing

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

-- | True when TIDEPOOL_JOINREC_DEBUG is set (to any value, matching the
-- Rust knobs' is_ok() semantics). Checked once at startup via unsafePerformIO.
{-# NOINLINE joinrecDebugEnabled #-}
joinrecDebugEnabled :: Bool
joinrecDebugEnabled = Data.Maybe.isJust $ unsafePerformIO $ System.Environment.lookupEnv "TIDEPOOL_JOINREC_DEBUG"

-- | Hex-formatting helper for debug output (used by TIDEPOOL_JOINREC_DEBUG).
showHex' :: Word64 -> String
showHex' w = "0x" ++ Numeric.showHex w ""

-- | Check if a jump to a given VarId occurs under a Lam in the expression.
-- When this is true, compiling the join point as a Cranelift block won't work
-- because the lambda gets compiled as a separate function with its own context.
--
-- CRUCIALLY (#313), "lambda" must include CONVERSION-INDUCED lambdas, not
-- just source-level ones: Rec joinrecs are ALWAYS translated as LetRec
-- lambdas (separate Cranelift functions), and a NonRec join that itself
-- converts becomes a lambda too. A jump to an outer join from inside any
-- such body crosses a function boundary that did not exist in the source
-- Core. Without this closure, the outer join compiles as a block in one
-- function while its jump sites live in another — the observed result was
-- a converted-join closure occupying an Eff continuation slot (case trap:
-- expected I#, got Text). Conversion is always SAFE (a lambda+NApp is
-- semantically a superset of a block+jump), so the predicate may be
-- conservative.
-- #313: "lambda" must include CONVERSION-INDUCED lambdas, not just
-- source-level ones — Rec joinrecs always become LetRec lambdas (separate
-- Cranelift functions), and a NonRec join that itself converts becomes a
-- lambda too. A jump to an outer join from inside any such body crosses a
-- function boundary that did not exist in source Core.
jumpCrossesLam :: Word64 -> CoreExpr -> Bool
jumpCrossesLam vid = go False
  where
    go underLam (Var v)   = underLam && varId v == vid
    go underLam (App f a) = go underLam f || go underLam a
    go _        (Lam b e)
      | isTyVar b         = go False e  -- type lambdas don't create new functions
      | otherwise          = go True e
    go underLam (Let (NonRec b rhs) e)
      | isJoinId b =
          -- An inner join that itself converts (same strengthened check
          -- against ITS body; nesting is a tree, so this terminates)
          -- becomes a lambda: jumps to OUR vid inside its RHS cross.
          let rhsUnderLam = underLam || jumpCrossesLam (varId b) e
          in go rhsUnderLam rhs || go underLam e
      | otherwise = go underLam rhs || go underLam e
    go underLam (Let (Rec pairs) e)
      | any (isJoinId . fst) pairs = any (go True . snd) pairs || go underLam e
      | otherwise = any (go underLam . snd) pairs || go underLam e
    go underLam (Case e _ _ alts) = go underLam e || any (goAlt underLam) alts
    go underLam (Cast e _) = go underLam e
    go underLam (Tick _ e) = go underLam e
    go _ (Lit _)          = False
    go _ (Type _)         = False
    go _ (Coercion _)     = False
    goAlt underLam (Alt _ _ e)      = go underLam e