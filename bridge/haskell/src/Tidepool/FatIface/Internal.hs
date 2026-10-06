-- | Per-key loading shared by a defining interface's immutable outcomes.
-- This module owns completion only; it cannot issue executable Core authority.
module Tidepool.FatIface.Internal
  ( LoadCache, newLoadCache, copyLoadCache, mergeLoadCaches, selectLoadCaches, evictLoadCache, lookupLoadCache ) where

import Control.Concurrent.MVar
  (MVar, modifyMVar, modifyMVar_, newEmptyMVar, newMVar, putMVar, readMVar)
import Control.Exception (SomeException, mask, throwIO, try, uninterruptibleMask_)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set

data Entry value = Cached value | Loading (MVar (Maybe value))
data Selection value = UseCached value | AwaitLoad (MVar (Maybe value)) | StartLoad (MVar (Maybe value))
newtype LoadCache key value = LoadCache (MVar (Map.Map key (Entry value)))

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
