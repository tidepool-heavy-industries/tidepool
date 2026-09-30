-- Exact original groups can retain package dependencies that the optimized
-- target no longer references. Their executable closure is part of admission.
module Tidepool.OriginalProductRoots
  ( requiredOriginalPackageGlobals, requiredOriginalPackageGlobalsWithExact
  , requiredOriginalPackageGlobalsWithRetained ) where

import Control.Monad (foldM)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , GlobalDecl(..) )
import Tidepool.ModuleCandidates
  ( ModuleCandidate(..), CandidateGroup(..), CandidateGlobal(..) )

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
requiredOriginalPackageGlobalsWithRetained fresh cached exact retained target = do
  indexed <- foldM insert Map.empty groups
  Set.toAscList <$> walk indexed Set.empty Set.empty
    [globalIdentity global | global <- target
      , globalRequiredGeneration global == Nothing
      , globalIdentity global `Set.notMember` retained
      , (symbolUnit (globalIdentity global), symbolModule (globalIdentity global))
          `Set.member` owners]
  where
    groups =
      [ ((unit, name, fromIntegral (projectedOriginalOrdinal group)),
          projectedBinders group,
          [(globalIdentity global, globalRequiredGeneration global == Nothing)
           | global <- projectedGlobals (projectedBody group)])
      | (unit, name, Right originals) <- fresh, group <- originals ]
      ++ [ ((candidateUnit candidate, candidateModule candidate,
             candidateGroupOrdinal group), candidateGroupBinders group,
             [(candidateGlobalIdentity global, candidateGlobalGeneration global == Nothing)
              | global <- candidateGroupGlobals group])
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
