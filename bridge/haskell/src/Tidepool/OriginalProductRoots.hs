-- Exact original groups can retain package dependencies that the optimized
-- target no longer references. Their executable closure is part of admission.
module Tidepool.OriginalProductRoots
  ( requiredOriginalPackageGlobals, requiredOriginalPackageGlobalsWithExact
  , requiredOriginalPackageGlobalsWithRetained, requiredOriginalExternalDemandWithPrior
  , requiredOriginalExternalDemandForSeeds
  , requiredOriginalExternalGlobalsWithPrior, OriginalExternalDemand
  , originalExternalHomeGroups, originalExternalPackageGlobals, originalExternalPriorHomeGroups
  , emptyOriginalExternalDemand
  , PriorHomeGroup, PriorOriginalExternalDemand(..)
  , ReconciledOriginalProducts, reconcileOriginalProducts, unrecoveredExactProducts
  , projectedOriginalGlobalDemand, candidateOriginalGlobalDemand ) where

import Control.Monad (foldM, forM_, unless)
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
  ( ExactScope, ExactProduct(..), ExactOriginalGroup(..)
  , scopeAvailableOriginalProducts )

-- Package demand and certification share one validated original index. Native
-- availability uses the old complete census without selecting more roots.
-- A new production never replaces an existing native carrier, including when
-- both lowerings came from the same canonical Core and interface.
newtype ReconciledOriginalProducts = ReconciledOriginalProducts [ExactProduct]

unrecoveredExactProducts :: ReconciledOriginalProducts -> [ExactProduct]
unrecoveredExactProducts (ReconciledOriginalProducts products) = products

reconcileOriginalProducts
  :: Maybe ExactScope -> [(String, String, [ExactOriginalGroup])]
  -> Either String ReconciledOriginalProducts
reconcileOriginalProducts scope recovered = do
  let owners = Map.fromList [((unit, name), groups) | (unit, name, groups) <- recovered]
  unless (Map.size owners == length recovered)
    (Left "duplicate recovered original owner in package root inventory")
  forM_ recovered $ \(_,_,groups) -> unless
    (Set.size (Set.fromList (map originalOrdinal groups)) == length groups)
    (Left "duplicate recovered original ordinal in package root inventory")
  let available = maybe [] scopeAvailableOriginalProducts scope
  forM_ available $ \original ->
    let owner = (originalUnit original,originalModule original)
    in forM_ (Map.lookup owner owners) $ \groups ->
      Left ("new original production replaces an admitted native carrier: " ++ show owner
        ++ "; native version=" ++ originalVersion original
        ++ "; admitted ordinals=" ++ show (map originalOrdinal (originalGroups original))
        ++ "; offered ordinals=" ++ show (map originalOrdinal groups))
  Right (ReconciledOriginalProducts available)

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
  fmap originalExternalPackageGlobals
    . requiredOriginalExternalDemandWithPrior fresh cached exact retained
      (const (Right Nothing))

requiredOriginalExternalGlobalsWithPrior
  :: [(String, String, Either String [ProjectedGroup])] -> [ModuleCandidate]
  -> [(String, String, [(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])])]
  -> Set.Set SymbolIdentity
  -> (SymbolIdentity -> Either String (Maybe PriorOriginalExternalDemand))
  -> [GlobalDecl] -> Either String [SymbolIdentity]
requiredOriginalExternalGlobalsWithPrior fresh cached exact retained prior =
  fmap originalExternalPackageGlobals
    . requiredOriginalExternalDemandWithPrior fresh cached exact retained prior

-- The demand owner reports both package leaves and the HOME groups reached to
-- obtain them. Consumers project only these groups; direct target membership
-- is not a complete HOME closure.
type PriorHomeGroup = (String, String, Maybe String, Word)

-- These coordinates and ordered direct package imports come only from the
-- reached groups in the prior certification, retaining its exact version.
data PriorOriginalExternalDemand = PriorOriginalExternalDemand
  { priorOriginalPackageGlobals :: Set.Set SymbolIdentity
  , priorOriginalHomeGroups :: Map.Map PriorHomeGroup [SymbolIdentity]
  } deriving (Eq, Show)

mergePriorHomeGroups :: Map.Map PriorHomeGroup [SymbolIdentity]
  -> Map.Map PriorHomeGroup [SymbolIdentity]
  -> Either String (Map.Map PriorHomeGroup [SymbolIdentity])
mergePriorHomeGroups = Map.foldlWithKey' add . Right
  where
    add result coordinate identities = do
      known <- result
      case Map.lookup coordinate known of
        Just previous | previous /= identities -> Left "prior HOME package imports changed"
        _ -> Right (Map.insert coordinate identities known)

data OriginalExternalDemand = OriginalExternalDemand
  (Set.Set (String, String, Word)) [SymbolIdentity] (Map.Map PriorHomeGroup [SymbolIdentity])
  deriving (Eq, Show)

originalExternalHomeGroups :: OriginalExternalDemand -> Set.Set (String,String,Word)
originalExternalHomeGroups (OriginalExternalDemand groups _ _) = groups

originalExternalPackageGlobals :: OriginalExternalDemand -> [SymbolIdentity]
originalExternalPackageGlobals (OriginalExternalDemand _ packages _) = packages

originalExternalPriorHomeGroups :: OriginalExternalDemand -> Map.Map PriorHomeGroup [SymbolIdentity]
originalExternalPriorHomeGroups (OriginalExternalDemand _ _ groups) = groups

emptyOriginalExternalDemand :: OriginalExternalDemand
emptyOriginalExternalDemand = OriginalExternalDemand Set.empty [] Map.empty

-- The read-only prior query stays with its certified owner. It is evaluated
-- only for reached symbols, never flattened into this current-group index.
requiredOriginalExternalDemandWithPrior
  :: [(String, String, Either String [ProjectedGroup])] -> [ModuleCandidate]
  -> [(String, String, [(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])])]
  -> Set.Set SymbolIdentity
  -> (SymbolIdentity -> Either String (Maybe PriorOriginalExternalDemand))
  -> [GlobalDecl] -> Either String OriginalExternalDemand
requiredOriginalExternalDemandWithPrior fresh cached exact retained prior =
  requiredOriginalExternalDemandForSeeds fresh cached exact retained prior
    . map globalIdentity
    . filter ((== Nothing) . globalRequiredGeneration)

requiredOriginalExternalDemandForSeeds
  :: [(String, String, Either String [ProjectedGroup])] -> [ModuleCandidate]
  -> [(String, String, [(Word, [SymbolIdentity], [(SymbolIdentity, Bool)])])]
  -> Set.Set SymbolIdentity
  -> (SymbolIdentity -> Either String (Maybe PriorOriginalExternalDemand))
  -> [SymbolIdentity] -> Either String OriginalExternalDemand
requiredOriginalExternalDemandForSeeds fresh cached exact retained prior =
  -- The original inventory is immutable across target and recovery passes.
  -- Partially applying this function retains its validated binder index;
  -- each target still starts a fresh traversal of that inventory.
  case foldM insert Map.empty groups of
    Left reason -> const (Left reason)
    Right indexed -> \seeds -> do
      (roots,packages,priorGroups) <- foldM seed ([],Set.empty,Map.empty)
        [symbol | symbol <- seeds, symbol `Set.notMember` retained]
      (reached, globals, reachedPrior) <- walk indexed Set.empty packages priorGroups roots
      Right (OriginalExternalDemand reached (Set.toAscList globals) reachedPrior)
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
    combine packages priorGroups selected = case selected of
      Nothing -> Right (packages,priorGroups)
      Just demand -> do
        groups' <- mergePriorHomeGroups priorGroups (priorOriginalHomeGroups demand)
        Right (Set.union packages (priorOriginalPackageGlobals demand),groups')
    seed (roots,packages,priorGroups) symbol
      | (symbolUnit symbol,symbolModule symbol) `Set.member` owners = Right (symbol:roots,packages,priorGroups)
      | otherwise = do
          selected <- prior symbol
          (packages',priorGroups') <- combine packages priorGroups selected
          Right (roots,packages',priorGroups')
    walk _ reached packages priorGroups [] = Right (reached, packages, priorGroups)
    walk indexed seen packages priorGroups (symbol : pending)
      | symbol `Set.member` retained = walk indexed seen packages priorGroups pending
      | otherwise = case Map.lookup symbol indexed of
          Just (key, globals)
            | key `Set.member` seen -> walk indexed seen packages priorGroups pending
            | otherwise -> walk indexed (Set.insert key seen) packages priorGroups
                ([identity | (identity, required) <- globals, required] ++ pending)
          Nothing -> do
            selected <- prior symbol
            case selected of
              Just _ -> do
                (packages',priorGroups') <- combine packages priorGroups selected
                walk indexed seen packages' priorGroups' pending
              Nothing
                | (symbolUnit symbol, symbolModule symbol) `Set.member` owners ->
                    let owner = (symbolUnit symbol, symbolModule symbol)
                        reason = Map.findWithDefault "no original group" owner failures
                    in Left ("required source global has no original group in package root inventory: "
                      ++ show symbol ++ ": " ++ reason)
                | otherwise -> walk indexed seen (Set.insert symbol packages) priorGroups pending
