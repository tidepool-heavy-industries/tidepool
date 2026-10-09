-- | Per-key loading shared by a defining interface's immutable outcomes.
-- This module owns completion only; it cannot issue executable Core authority.
module Tidepool.FatIface.Internal
  ( LoadCache, newLoadCache, copyLoadCache, mergeLoadCaches, selectLoadCaches
  , evictLoadCache, lookupLoadCache, lookupCompletedLoadCache
  , privateComponents
  , OwnerInterfaceContext(..), issueOwnerInterfaceContext
  ) where

import Data.Unique (Unique, newUnique)
import GHC.Core.TyCon (TyCon)
import GHC.Types.Name (Name)
import GHC.Types.Var (Id)
import GHC.Unit.Types (Module)
import GHC.Unit.Module.Location (ModLocation)
import GHC.Utils.Fingerprint (Fingerprint)

import Control.Concurrent.MVar
  (MVar, modifyMVar, modifyMVar_, newEmptyMVar, newMVar, putMVar, readMVar, withMVar)
import Control.Exception (SomeException, mask, throwIO, try, uninterruptibleMask_)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set

-- The issuer token follows this immutable context through cache copies and
-- selections. Its interface identity binds the declaring entries independently
-- of the optional executable Core version.
data OwnerInterfaceContext = OwnerInterfaceContext
  { ownerInterfaceIdentity :: !Unique
  , ownerInterfaceVersion :: !(Module, Fingerprint)
  , ownerInterfaceLocation :: ModLocation
  , ownerInterfaceTyCons :: [TyCon]
  , ownerInterfaceEntries :: Map.Map Name Id
  }

issueOwnerInterfaceContext :: Module -> Fingerprint -> ModLocation -> [TyCon]
  -> Map.Map Name Id -> IO OwnerInterfaceContext
issueOwnerInterfaceContext owner fingerprint location tycons entries = do
  identity <- newUnique
  pure (OwnerInterfaceContext identity (owner,fingerprint) location tycons entries)

data Entry value = Cached value | Loading (MVar (Maybe value))
data Selection value = UseCached value | AwaitLoad (MVar (Maybe value)) | StartLoad (MVar (Maybe value))
newtype LoadCache key value = LoadCache (MVar (Map.Map key (Entry value)))

-- | A read-only completed lookup. In-flight entries are deliberately absent;
-- this never waits for or assumes ownership of another loader's completion.
lookupCompletedLoadCache :: Ord key => LoadCache key value -> key -> IO (Maybe value)
lookupCompletedLoadCache (LoadCache ref) key = withMVar ref $ \entries ->
  pure $ case Map.lookup key entries of
    Just (Cached value) -> Just value
    _ -> Nothing

-- | Canonical weakly connected components for a directed adjacency map.
-- Vertices are the map keys; references outside that census are ignored here
-- and must be rejected by the caller that owns the source graph.
privateComponents :: Map.Map Int (Set.Set Int) -> [Set.Set Int]
privateComponents adjacency = collect (Map.keysSet adjacency) []
  where
    undirected = Map.foldlWithKey' addOutgoing
      (Map.map (const Set.empty) adjacency) adjacency
    addOutgoing graph source targets = Set.foldl' (addEdge source) graph targets
    addEdge source graph target
      | Map.member target adjacency = Map.adjust (Set.insert target) source
          (Map.adjust (Set.insert source) target graph)
      | otherwise = graph
    collect remaining components = case Set.minView remaining of
      Nothing -> reverse components
      Just (seed, _) ->
        let component = flood Set.empty [seed]
        in collect (remaining `Set.difference` component) (component : components)
    flood seen [] = seen
    flood seen (vertex : pending)
      | vertex `Set.member` seen = flood seen pending
      | otherwise = flood (Set.insert vertex seen)
          (Set.toAscList (Map.findWithDefault Set.empty vertex undirected) ++ pending)

newLoadCache :: IO (LoadCache key value)
newLoadCache = LoadCache <$> newMVar Map.empty

-- | A failed attempt cannot transfer unfinished additions into a completed
-- context. Copies share immutable completed values but own their map.
copyLoadCache :: LoadCache key value -> IO (LoadCache key value)
copyLoadCache (LoadCache ref) = do
  entries <- readMVar ref
  LoadCache <$> newMVar (completedEntries (const True) entries)

-- | Select completed entries from each source before combining them. Earlier
-- sources win duplicate keys; in-flight ownership is never transferred.
mergeLoadCaches :: Ord key => [(LoadCache key value, key -> Bool)] -> IO (LoadCache key value)
mergeLoadCaches sources = do
  selected <- mapM (\(LoadCache ref, keep) -> completedEntries keep <$> readMVar ref) sources
  LoadCache <$> newMVar (Map.unions selected)

-- | Resolve only the requested keys. Historical maps may contain many other
-- owners; selection neither visits their entries nor transfers loading cells.
selectLoadCaches :: Ord key => [(LoadCache key value, Set.Set key)] -> IO (LoadCache key value)
selectLoadCaches sources = do
  selected <- mapM (\(LoadCache ref, keys) -> do
    entries <- readMVar ref
    pure (Map.fromAscList
      [(key, Cached value) | key <- Set.toAscList keys
        , Just (Cached value) <- [Map.lookup key entries]])) sources
  LoadCache <$> newMVar (Map.unions selected)

completedEntries :: (key -> Bool) -> Map.Map key (Entry value) -> Map.Map key (Entry value)
completedEntries keep = Map.mapMaybeWithKey completed
  where
    completed key (Cached value) | keep key = Just (Cached value)
    completed _ _ = Nothing

evictLoadCache :: LoadCache key value -> (key -> Bool) -> IO ()
evictLoadCache (LoadCache ref) stale =
  modifyMVar_ ref (pure . Map.filterWithKey (\key _ -> not (stale key)))

-- | The map lock is held only for selection and publication. Losing the
-- winning loader removes its own generation and wakes every waiter to retry;
-- an evicted generation still settles its existing callers, but cannot replace
-- a newer generation. Cacheable failure values remain distinct from cancellation.
lookupLoadCache :: Ord key => LoadCache key value -> key -> IO value -> IO value
lookupLoadCache cache@(LoadCache ref) key load = mask $ \restore -> do
  selected <- modifyMVar ref $ \entries -> case Map.lookup key entries of
    Just (Cached value) -> pure (entries, UseCached value)
    Just (Loading completion) -> pure (entries, AwaitLoad completion)
    Nothing -> do
      completion <- newEmptyMVar
      pure (Map.insert key (Loading completion) entries, StartLoad completion)
  case selected of
    UseCached value -> pure value
    AwaitLoad completion -> do
      settled <- restore (readMVar completion)
      maybe (restore (lookupLoadCache cache key load)) pure settled
    StartLoad completion -> do
      loaded <- try (restore load)
      case loaded of
        Left exception -> do
          uninterruptibleMask_ $ do
            modifyMVar_ ref $ \entries -> pure $ case Map.lookup key entries of
              Just (Loading current) | current == completion -> Map.delete key entries
              _ -> entries
            putMVar completion Nothing
          throwIO (exception :: SomeException)
        Right value -> do
          uninterruptibleMask_ $ do
            modifyMVar_ ref $ \entries -> pure $ case Map.lookup key entries of
              Just (Loading current) | current == completion -> Map.insert key (Cached value) entries
              _ -> entries
            putMVar completion (Just value)
          pure value
