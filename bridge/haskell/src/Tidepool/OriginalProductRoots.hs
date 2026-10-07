-- Exact original groups can retain package dependencies that the optimized
-- target no longer references. Their executable closure is part of admission.
module Tidepool.OriginalProductRoots
  ( requiredOriginalPackageGlobals, requiredOriginalPackageGlobalsWithExact
  , requiredOriginalPackageGlobalsWithRetained
  , ReconciledOriginalProducts, reconcileOriginalProducts, unrecoveredExactProducts
  , projectedOriginalGlobalDemand, candidateOriginalGlobalDemand ) where

import Control.Monad (foldM, forM, forM_, unless)
import qualified Data.Map.Strict as Map
import Data.Maybe (isNothing)
import qualified Data.Set as Set
import qualified Data.Text as T
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , GlobalDecl(..) )
import Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateGroup(..), CandidateGlobal(..) )
import Tidepool.ExactScope
  ( ExactScope(..), ExactProduct(..), ExactOriginalGroup(..)
  , CanonicalInterfaceProof, scopeModuleInterfaceProofs, canonicalProofMatchesOwner
  , scopeAvailableOriginalProducts )

-- Package demand and certification share one validated original index. Native
-- availability uses the old complete census without selecting more roots.
-- A separately prepared canonical original must still match admitted evidence.
newtype ReconciledOriginalProducts = ReconciledOriginalProducts [ExactProduct]

unrecoveredExactProducts :: ReconciledOriginalProducts -> [ExactProduct]
unrecoveredExactProducts (ReconciledOriginalProducts products) = products

reconcileOriginalProducts
  :: Maybe ExactScope -> Map.Map (String, String) CanonicalInterfaceProof
  -> [(String, String, [ExactOriginalGroup])] -> Either String ReconciledOriginalProducts
reconcileOriginalProducts Nothing _ _ = Right (ReconciledOriginalProducts [])
reconcileOriginalProducts (Just scope) proofs recovered = do
  let owners = Map.fromList [((unit, name), groups) | (unit, name, groups) <- recovered]
  unless (Map.size owners == length recovered)
    (Left "duplicate recovered original owner in package root inventory")
  let available = Map.fromList [((originalUnit original,originalModule original),original)
        | original <- scopeAvailableOriginalProducts scope]
  selected <- forM (scopeProducts scope) $ \original -> case Map.lookup (originalUnit original, originalModule original) owners of
    Nothing -> Right (Map.lookup (originalUnit original,originalModule original) available)
    Just groups -> do
      let owner = (originalUnit original, originalModule original)
          indexed = Map.fromList [(originalOrdinal group, group) | group <- groups]
      unless (Map.size indexed == length groups)
        (Left "duplicate recovered original ordinal in package root inventory")
      case (Map.lookup owner proofs, Map.lookup owner (scopeModuleInterfaceProofs scope)) of
        (Just proof, Just admitted)
          | canonicalProofMatchesOwner (scopeProducerSha256 scope) owner proof && proof == admitted -> Right ()
        _ -> Left "recovered original group lacks its exact canonical owner"
      forM_ (originalGroups original) $ \group -> case Map.lookup (originalOrdinal group) indexed of
        Nothing -> Left "recovered original lacks an admitted native group ordinal"
        Just current -> do
          unless (originalBinders current == originalBinders group
              && Set.fromList (originalGlobals current) == Set.fromList (originalGlobals group))
            (Left "recovered original group differs from its admitted native witness")
          Right ()
      Right Nothing
  Right (ReconciledOriginalProducts [original | Just original <- selected])

-- The outline bit records whether recovery must supply a definition. GHC's
-- evaluatedness flag is independent: an unevaluated dictionary still needs
-- its complete executable dependency closure.
projectedOriginalGlobalDemand :: GlobalDecl -> (SymbolIdentity, Bool)
projectedOriginalGlobalDemand global =
  (globalIdentity global, isNothing (globalRequiredGeneration global))

candidateOriginalGlobalDemand :: CandidateGlobal -> (SymbolIdentity, Bool)
candidateOriginalGlobalDemand global =
  (candidateGlobalIdentity global, isNothing (candidateGlobalGeneration global))

requiredOriginalPackageGlobals
  :: [(String, String, Either String [ProjectedGroup])] -> [ModuleCandidate] -> [GlobalDecl]
  -> Either String [SymbolIdentity]
requiredOriginalPackageGlobals fresh cached =
  requiredOriginalPackageGlobalsWithExact fresh cached []

-- Exact context outlines come from the same protected original products,
-- independently of source lookup and the selected virtual lexical graph.
requiredOriginalPackageGlobalsWithExact
  :: [(String, String, Either String [ProjectedGroup])] -> [ModuleCandidate]
  -> [(String, String, [(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])])]
  -> [GlobalDecl] -> Either String [SymbolIdentity]
requiredOriginalPackageGlobalsWithExact fresh cached exact =
  requiredOriginalPackageGlobalsWithRetained fresh cached exact Set.empty

-- Retained generations already provide executable imports. Their exact
-- identity is a closure boundary, including while walking original groups:
-- the projected module deliberately omits the imported implementation.
requiredOriginalPackageGlobalsWithRetained
  :: [(String, String, Either String [ProjectedGroup])] -> [ModuleCandidate]
  -> [(String, String, [(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])])]
  -> Set.Set SymbolIdentity -> [GlobalDecl] -> Either String [SymbolIdentity]
requiredOriginalPackageGlobalsWithRetained fresh cached exact retained =
  -- The original inventory is immutable across target and recovery passes.
  -- Partially applying this function retains its validated binder index;
  -- each target still starts a fresh traversal of that inventory.
  case foldM insert Map.empty groups of
    Left reason -> const (Left reason)
    Right indexed -> \target -> Set.toAscList <$> walk indexed Set.empty Set.empty
      [globalIdentity global | global <- target
        , globalRequiredGeneration global == Nothing
        , globalIdentity global `Set.notMember` retained
        , (symbolUnit (globalIdentity global), symbolModule (globalIdentity global))
            `Set.member` owners]
  where
    groups =
      [ ((unit, name, fromIntegral (projectedOriginalOrdinal group)),
          projectedBinders group,
          map projectedOriginalGlobalDemand (projectedGlobals (projectedBody group)))
      | (unit, name, Right originals) <- fresh, group <- originals ]
      ++ [ ((candidateUnit candidate, candidateModule candidate,
             candidateGroupOrdinal group), candidateGroupBinders group,
             map candidateOriginalGlobalDemand (candidateGroupGlobals group))
         | candidate <- cached, group <- candidateGroups candidate ]
      ++ [((unit, name, ordinal), binders, references)
         | (unit, name, originals) <- exact, (ordinal, binders, references) <- originals]
    owners = Set.fromList
      ([(T.pack unit, T.pack name) | (unit, name, _) <- fresh]
       ++ [(T.pack (candidateUnit candidate), T.pack (candidateModule candidate))
          | candidate <- cached]
       ++ [(T.pack unit, T.pack name) | (unit, name, _) <- exact])
    failures = Map.fromList
      [((T.pack unit, T.pack name), reason) | (unit, name, Left reason) <- fresh]
    insert indexed (key, binders, globals) = foldM
      (\current binder -> if Map.member binder current
        then Left "duplicate original binder in package root inventory"
        else Right (Map.insert binder (key, globals) current)) indexed binders
    walk _ _ packages [] = Right packages
    walk indexed seen packages (symbol : pending)
      | symbol `Set.member` retained = walk indexed seen packages pending
      | otherwise = case Map.lookup symbol indexed of
          Just (key, globals)
            | key `Set.member` seen -> walk indexed seen packages pending
            | otherwise -> walk indexed (Set.insert key seen) packages
                ([identity | (identity, required) <- globals, required] ++ pending)
          Nothing
            | (symbolUnit symbol, symbolModule symbol) `Set.member` owners ->
                let owner = (symbolUnit symbol, symbolModule symbol)
                    reason = Map.findWithDefault "no original group" owner failures
                in Left ("required source global has no original group in package root inventory: "
                  ++ show symbol ++ ": " ++ reason)
            | otherwise -> walk indexed seen (Set.insert symbol packages) pending
