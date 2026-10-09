-- | Exact dependency closure for closed programs. Session generation linking
-- remains a separate owner: this path never loads missing source-home bodies
-- from an interface left by an earlier edit.
module Tidepool.PreparedRecovery
  ( RecoveryFailure(..), RecoveryPublicationFailure(..), requirePreparedRecoveryPublication
  , RecoveredClosure(..), recoverPreparedClosure
  , newPreparedRecovery, newPreparedRecoveryWithPackageRoots, newPreparedRecoveryWithExecutor
  , PreparedRecovery, preparedRecoveryClosure, growPreparedRecovery
  , insertGroup
  ) where

import Control.Exception (Exception, evaluate, throwIO)
import Control.Monad (unless, when)
import Data.IORef (atomicModifyIORef', modifyIORef', newIORef, readIORef)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Maybe (isJust)
import Data.Word (Word64)
import GHC.Types.Unique.Set (elementOfUniqSet, nonDetEltsUniqSet, sizeUniqSet, addListToUniqSet)
import GHC.Types.Unique (getKey)
import System.Environment (lookupEnv)
import GHC.Core (CoreBind, Bind(..))
import GHC.Driver.Env (HscEnv)
import GHC.Types.Id (idType)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.RepType (typePrimRep_maybe)
import GHC.Types.Var (Id, varName, varUnique)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (Module, unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionProjection
  ( PreparedReachability(..), ProjectionContext(..), admitReachFacts
  , combinePreparedTargetReferences, emptyPreparedReachability
  , preparedModuleReachFacts, preparedModuleReferenceFacts, preparedSeedUniques
  , preparedTargetReferences, preparedRootIdentity, topBinders )
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.FatIface
  ( FatIfaceCache, FatIfaceMissing, FatIfaceComponentLookup(..), OwnerInterfaceCache
  , lookupFatIfaceComponents )
import Tidepool.PreparedStg
  ( PreparedBodyCache, PreparedModule, pmModule, pmBindings, RecoveredModuleFailure(..)
  , PreparedComponents, newPreparedComponentUnitsPreparer, runPreparedComponentTask
  , preparedComponentModules, preparedComponentDelta, validatePreparedComponents )
import Tidepool.CompilerExecution (CompilerExecutor, runCompilerWorklist)
import Tidepool.Resolve (ExactBodyLookup(..), recoverExactBody)
import Tidepool.PreparedBuiltins (deferredFunction, wiredInErrorKind)
import Tidepool.Timing (emitDetailPhase, emitCount, readTimingEnabled, timeSection)

data RecoveryFailure
  = MissingImplementation Name FatIfaceMissing
  | InterfaceLoadingFailure Module String
  | IncompatibleImplementation Name String
  | UnsupportedExternalCapability Name
  | MissingHomeImplementation Name
  | DefiningPreparationFailure RecoveredModuleFailure
  deriving (Eq)

instance Show RecoveryFailure where
  show failure = case failure of
    MissingImplementation name reason ->
      "missing implementation " ++ renderName name ++ ": " ++ show reason
    InterfaceLoadingFailure owner reason ->
      "interface loading failure " ++ renderModule owner ++ ": " ++ reason
    IncompatibleImplementation name reason ->
      "incompatible implementation " ++ renderName name ++ ": " ++ reason
    UnsupportedExternalCapability name ->
      "unsupported external capability " ++ renderName name
    MissingHomeImplementation name ->
      "missing home implementation " ++ renderName name
    DefiningPreparationFailure reason ->
      "defining preparation failure: " ++ show reason
    where
      renderModule owner = unitString (moduleUnit owner) ++ ":"
        ++ moduleNameString (moduleName owner)
      renderName name = case nameModule_maybe name of
        Just owner -> renderModule owner ++ ":" ++ occNameString (nameOccName name)
        Nothing -> showSDocUnsafe (ppr name)

-- | Residuals are evidence, not an empty-map fallback. Projection retains
-- their global declarations, allowing corpus admission to report what remains.
data RecoveredClosure = RecoveredClosure
  { closureModules :: [PreparedModule]
  , closureHomeModules :: [PreparedModule]
  , closureComponentSelections :: [PreparedComponents]
  , closureFailures :: [RecoveryFailure]
  -- | Per-segment evidence of reuse from the request's immutable body-set
  -- memo. Reachability and accounting are never shared between targets.
  , closureFactCacheHits :: Int
  -- | Final target-local closure over the modules in 'closureModules'.
  , closureReachability :: PreparedReachability
  }

data RecoveryPublicationFailure = RecoveryPublicationFailure String [RecoveryFailure]
  deriving (Show)

instance Exception RecoveryPublicationFailure

-- Corpus recovery retains every residual. Executable publication cannot
-- proceed after a required interface or defining preparation failed.
requirePreparedRecoveryPublication :: String -> RecoveredClosure -> IO ()
requirePreparedRecoveryPublication target closure =
  case filter hardFailure (closureFailures closure) of
    [] -> pure ()
    failures -> throwIO (RecoveryPublicationFailure target failures)
  where
    hardFailure failure = case failure of
      InterfaceLoadingFailure{} -> True
      DefiningPreparationFailure{} -> True
      MissingImplementation{} -> False
      IncompatibleImplementation{} -> False
      UnsupportedExternalCapability{} -> False
      MissingHomeImplementation{} -> False

-- | One target's closed recovery state. The only continuation admits more
-- package roots under the same home graph, exact interfaces and authority.
-- State is captured immutably, so another target cannot inherit its attempts,
-- failures, prepared body sets or reachability.
data PreparedRecovery = PreparedRecovery
  { preparedRecoveryClosure :: RecoveredClosure
  , growPreparedRecovery :: [Id] -> IO PreparedRecovery
  }

-- | Coordinator elapsed work is summed over demand updates. Preparation
-- service sums independent task elapsed times and may exceed request elapsed
-- time under overlap; it is never an additive request-phase breakdown.
data Spent = Spent
  { spentFacts :: !Integer
  , spentReach :: !Integer
  , spentRefs :: !Integer
  , spentLookup :: !Integer
  , spentPrepare :: !Integer
  , spentRounds :: !Integer
  , spentPreparations :: !Integer
  }

-- | Reprepare only defining modules whose exact body set grows. Attempted
-- names include typed failures, so unavailable bodies terminate the worklist
-- without a retry limit. New CorePrep references re-enter this same loop.
--
-- 'cache' (fat-interface Core, keyed by defining module), 'ownerCache'
-- (an owner's already-read-and-typechecked defining interface) and
-- 'bodyCache' (the prepared bodies themselves; see
-- 'Tidepool.PreparedStg.prepareRecoveredBodies') are all caller-owned so a
-- resident daemon can hoist them to daemon lifetime across requests, with
-- eviction at the request boundary for a request's own target module and any
-- @Tidepool.Session.*@ module (the scoped eviction callback in app/Main.hs); a one-shot invocation instead passes a
-- fresh cache created just for this call.
recoverPreparedClosure :: HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext -> [PreparedModule]
  -> IO RecoveredClosure
recoverPreparedClosure env cache ownerCache bodyCache context home = do
  recover <- newPreparedRecovery env cache ownerCache bodyCache context home
  recover (projectionEntry context)

-- | Each entry starts an isolated target closure; only immutable facts are
-- shared between targets. Use the package-root factory when continuing one.
newPreparedRecovery :: HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext -> [PreparedModule]
  -> IO (SymbolIdentity -> IO RecoveredClosure)
newPreparedRecovery env cache ownerCache bodyCache baseContext home = do
  recover <- newPreparedRecoveryWithPackageRoots env cache ownerCache bodyCache
    baseContext home []
  pure (fmap preparedRecoveryClosure . recover)

-- | Share immutable facts between targets; continue only within one target.
-- Roots affect seeds and selection, not module facts. Authority remains fixed;
-- a replaced exact body set still invalidates facts through 'preparedBodyKey'.
-- Certified homes come only from admitted original products; package roots
-- come from the emitted-global demand of those exact original groups.
newPreparedRecoveryWithPackageRoots :: HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext
  -> [PreparedModule] -> [Id] -> IO (SymbolIdentity -> IO PreparedRecovery)
newPreparedRecoveryWithPackageRoots = newPreparedRecoveryUsing Nothing

-- | All recovery jobs use the request's existing allowance. Module contexts
-- are acquired by its coordinator before lowering tasks enter the executor.
newPreparedRecoveryWithExecutor :: CompilerExecutor -> HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext
  -> [PreparedModule] -> [Id] -> IO (SymbolIdentity -> IO PreparedRecovery)
newPreparedRecoveryWithExecutor executor = newPreparedRecoveryUsing (Just executor)

newPreparedRecoveryUsing :: Maybe CompilerExecutor -> HscEnv -> FatIfaceCache -> OwnerInterfaceCache
  -> PreparedBodyCache -> ProjectionContext
  -> [PreparedModule] -> [Id] -> IO (SymbolIdentity -> IO PreparedRecovery)
newPreparedRecoveryUsing executor env cache ownerCache bodyCache baseContext home initialRoots = do
  timing <- readTimingEnabled
  checking <- isJust <$> lookupEnv "TIDEPOOL_RECOVERY_CHECK"
  let factsOf prepared =
        (preparedModuleReferenceFacts baseContext prepared, preparedModuleReachFacts baseContext prepared)
      homeFacts = [(prepared, factsOf prepared) | prepared <- home]
      runJobs :: (input -> IO output) -> (input -> output -> IO [input]) -> [input] -> IO [output]
      runJobs action completed inputs = case executor of
        Just shared -> runCompilerWorklist shared action completed inputs
        Nothing -> let loop [] outputs = pure (reverse outputs)
                       loop (input:pending) outputs = do
                         output <- action input
                         additions <- completed input output
                         loop (pending ++ additions) (output:outputs)
                    in loop inputs []
  factsMemo <- newIORef Map.empty
  acquireBodies <- newPreparedComponentUnitsPreparer env ownerCache bodyCache
  pure $ \entry -> do
    let homeOwners = Set.fromList (map pmModule home)
        run roots carriedAttempts carriedGroups carriedModules carriedFailures carriedOwners carriedReach = do
          let context = baseContext
                { projectionEntry = entry
                , projectionAuxiliaryRoots = projectionAuxiliaryRoots baseContext
                    ++ map preparedRootIdentity roots
                }
              seedList = nonDetEltsUniqSet (addListToUniqSet (preparedSeedUniques context home)
                (map varUnique roots))
          factHits <- newIORef (0 :: Int)
          spent <- newIORef (Spent 0 0 0 0 0 0 0)
          state <- newIORef RecoveryState
            { recoveryAttempted = carriedAttempts
            , recoveryGroups = carriedGroups
            , recoveryPrepared = carriedModules
            , recoveryFailures = carriedFailures
            , recoveryAdmitted = carriedOwners
            , recoveryReach = carriedReach
            , recoveryDirty = Set.empty
            , recoveryVersions = Map.map (const 0) carriedGroups
            , recoveryRunning = Set.empty
            , recoveryNewUnits = home
            , recoveryRebuild = False
            }
          let factsFor prepared = do
                memo <- readIORef factsMemo
                case Map.lookup (preparedBodyKey prepared) memo of
                  Just hit -> pure (hit, True)
                  Nothing -> do
                    let fresh = factsOf prepared
                    atomicModifyIORef' factsMemo (\known ->
                      (Map.insert (preparedBodyKey prepared) fresh known, ()))
                    pure (fresh, False)
              includeRoots reach references = Map.elems (Map.fromList
                [(varName binder, binder)
                | binder <- references ++ roots
                , not (elementOfUniqSet (varUnique binder) (admittedTops reach))])
              roundReferences reach entries =
                let reached = reachedUniques reach
                    kept binding =
                      any ((`elementOfUniqSet` reached) . varUnique) (topBinders binding)
                in combinePreparedTargetReferences context (admittedTops reach) kept
                     [(prepared, references) | (prepared, (references, _)) <- entries]
              charge update = modifyIORef' spent update
              recordLookup found = modifyIORef' state $ \current -> case found of
                ExactBody owner bodies ->
                  let previous = Map.findWithDefault [] owner (recoveryGroups current)
                      next = foldl (flip insertGroup) previous bodies
                      names = Set.fromList . concatMap (map varName . binders)
                  in if names next == names previous then current else current
                    { recoveryGroups = Map.insert owner next (recoveryGroups current)
                    , recoveryDirty = Set.insert owner (recoveryDirty current)
                    , recoveryVersions = Map.insertWith (+) owner 1 (recoveryVersions current)
                    }
                MissingExactBody name reason -> addFailure current (MissingImplementation name reason)
                BodyInterfaceFailure owner reason -> addFailure current (InterfaceLoadingFailure owner reason)
                BodyTypeMismatch _ name requested candidate detail -> addFailure current
                  (IncompatibleImplementation name ("requested type " ++ requested
                    ++ "; candidate type " ++ candidate ++ "; " ++ detail))
                UnsupportedBodyCapability name -> addFailure current (UnsupportedExternalCapability name)
                where addFailure current failure = current
                        { recoveryFailures = recoveryFailures current ++ [failure] }
              lookupNeeded binder
                | Just _ <- wiredInErrorKind binder = pure Nothing
                | Just _ <- deferredFunction binder = pure Nothing
                | Map.member (varName binder) (projectionCurrentOriginals context) = pure Nothing
                | maybe False (`Set.member` homeOwners) (nameModule_maybe (varName binder)) = do
                    modifyIORef' state (\current -> current
                      { recoveryFailures = recoveryFailures current ++ [MissingHomeImplementation (varName binder)] })
                    pure Nothing
                | otherwise = Just <$> recoverExactBody env cache binder
              acquireOwner current owner = do
                let version = Map.findWithDefault 0 owner (recoveryVersions current)
                selected <- lookupFatIfaceComponents env cache owner
                  (concatMap (map varName . binders) (Map.findWithDefault [] owner (recoveryGroups current)))
                task <- case selected of
                  FatIfaceComponents components -> acquireBodies (Map.lookup owner (recoveryPrepared current)) components
                  FatIfaceComponentsMissing reason -> pure (Left
                    (RecoveredModulePreparationFailure owner
                      ("original body set disappeared: " ++ show reason)))
                  FatIfaceComponentsLoadFailure _ reason -> pure (Left
                    (RecoveredModuleInterfaceFailure owner reason))
                pure (owner, version, task)
              -- Recompute target demand immediately on module completion. Other
              -- queued/running jobs remain on the same allowance; recursive
              -- demand never creates a second pool or a visited-owner shortcut.
              expand = do
                current <- readIORef state
                (recovered, factsMs) <- timeSection $ do
                  entries <- mapM (\prepared -> do
                    (facts, hit) <- factsFor prepared
                    when hit (modifyIORef' factHits (+ 1))
                    pure (prepared, facts)) (concatMap preparedComponentModules (Map.elems (recoveryPrepared current)))
                  mapM_ (\(_, (references, reach)) -> do
                    _ <- evaluate (sum (map length (Map.elems references)))
                    evaluate (sum (map (length . snd) reach))) entries
                  pure entries
                let entries = homeFacts ++ recovered
                    modules = home ++ concatMap preparedComponentModules (Map.elems (recoveryPrepared current))
                (reach, reachMs) <- timeSection $ do
                  let newlyAdmitted = Set.fromList (map preparedBodyKey (recoveryNewUnits current))
                      admittedFacts =
                        [ facts | (prepared, (_, facts)) <- entries
                        , recoveryRebuild current || preparedBodyKey prepared `Set.member` newlyAdmitted ]
                      prior = if recoveryRebuild current then emptyPreparedReachability else recoveryReach current
                      extended = admitReachFacts seedList admittedFacts prior
                  _ <- evaluate (sizeUniqSet (reachedUniques extended))
                  _ <- evaluate (sizeUniqSet (admittedTops extended))
                  pure extended
                (references, refsMs) <- timeSection $ do
                  refs <- evaluate (includeRoots reach (roundReferences reach entries))
                  _ <- evaluate (length refs)
                  when checking $ do
                    let expected = includeRoots reach (preparedTargetReferences context modules)
                    unless (Set.fromList (map (getKey . varUnique) refs) == Set.fromList (map (getKey . varUnique) expected)) $
                      throwIO (userError ("recovery reachability diverged from identity selection: "
                        ++ show (length refs) ++ " vs " ++ show (length expected) ++ " references"))
                  pure refs
                let pending = filter (\binder -> varName binder `Set.notMember` recoveryAttempted current
                        && typePrimRep_maybe (idType binder) /= Just []) references
                modifyIORef' state (\latest -> latest
                  { recoveryReach = reach
                  , recoveryAdmitted = Set.empty
                  , recoveryNewUnits = []
                  , recoveryRebuild = False
                  , recoveryAttempted = Set.union (recoveryAttempted latest) (Set.fromList (map varName pending))
                  })
                charge (\cost -> cost
                  { spentFacts = spentFacts cost + factsMs
                  , spentReach = spentReach cost + reachMs
                  , spentRefs = spentRefs cost + refsMs
                  , spentRounds = spentRounds cost + 1 })
                -- Interface/finder hydration is a compiler-context operation,
                -- not an independently acquired lowering input. Keep it on the
                -- coordinator while other immutable STG tasks may continue.
                (_, lookupMs) <- timeSection $ mapM (\binder -> do
                  found <- lookupNeeded binder
                  maybe (pure ()) recordLookup found) pending
                charge (\cost -> cost { spentLookup = spentLookup cost + lookupMs })
                selected <- readIORef state
                let dirty = Set.toAscList (recoveryDirty selected `Set.difference` recoveryRunning selected)
                jobs <- mapM (acquireOwner selected) dirty
                modifyIORef' state (\latest -> latest
                  { recoveryDirty = recoveryDirty latest `Set.difference` Set.fromList dirty
                  , recoveryRunning = recoveryRunning latest `Set.union` Set.fromList dirty })
                emitCount timing "prepared_recover_ready_width" (fromIntegral (length jobs))
                charge (\cost -> cost { spentPreparations = spentPreparations cost + fromIntegral (length jobs) })
                pure jobs
              complete (owner, version, _) (outcome, prepareMs) = do
                charge (\cost -> cost { spentPrepare = spentPrepare cost + prepareMs })
                latest <- readIORef state
                -- One owner has one active selection. Pure results remain valid
                -- when demand grows while it runs; another selection follows.
                modifyIORef' state $ \settled -> case outcome of
                  Right prepared ->
                    let (replaced,delta) = preparedComponentDelta (Map.lookup owner (recoveryPrepared settled)) prepared
                    in settled
                      { recoveryPrepared = Map.insert owner prepared (recoveryPrepared settled)
                      , recoveryFailures = filter (not . preparationFailureFor owner) (recoveryFailures settled)
                      , recoveryAdmitted = Set.insert owner (recoveryAdmitted settled)
                      , recoveryNewUnits = recoveryNewUnits settled ++ delta
                      , recoveryRebuild = recoveryRebuild settled || replaced
                      , recoveryRunning = Set.delete owner (recoveryRunning settled)
                      , recoveryDirty = if Map.lookup owner (recoveryVersions latest) /= Just version
                          then Set.insert owner (recoveryDirty settled) else recoveryDirty settled
                      }
                  Left failure -> settled
                    { recoveryFailures = recoveryFailures settled ++ [DefiningPreparationFailure failure]
                    , recoveryRunning = Set.delete owner (recoveryRunning settled) }
                expand
          initialJobs <- expand
          _ <- runJobs
            (\(_,_,task) -> timeSection (either (pure . Left) runPreparedComponentTask task))
            complete initialJobs
          settled <- readIORef state
          Spent factsTotal reachTotal refsTotal lookupTotal prepareTotal rounds preparations <- readIORef spent
          emitDetailPhase timing "prepared_recover" "prepared_recover_facts" factsTotal
          emitDetailPhase timing "prepared_recover" "prepared_recover_reach" reachTotal
          emitDetailPhase timing "prepared_recover" "prepared_recover_refs" refsTotal
          emitDetailPhase timing "prepared_recover" "prepared_recover_lookup" lookupTotal
          emitDetailPhase timing "prepared_recover" "prepared_recover_prepare_service" prepareTotal
          emitCount timing "prepared_recover_rounds" rounds
          emitCount timing "prepared_recover_module_preparations" preparations
          mapM_ validatePreparedComponents (Map.elems (recoveryPrepared settled))
          hits <- readIORef factHits
          pure (PreparedRecovery
            (RecoveredClosure (home ++ concatMap preparedComponentModules (Map.elems (recoveryPrepared settled)))
              home (Map.elems (recoveryPrepared settled))
              (recoveryFailures settled) hits (recoveryReach settled))
            (\extra -> run (Map.elems (Map.fromList
                [(preparedRootIdentity root, root) | root <- roots ++ extra]))
              (recoveryAttempted settled) (recoveryGroups settled)
              (recoveryPrepared settled) (recoveryFailures settled) Set.empty (recoveryReach settled)))
    run initialRoots Set.empty Map.empty Map.empty [] homeOwners emptyPreparedReachability

-- Only the coordinator mutates demand, reachability and incorporation. Exact
-- versions fence late completions when another task grows an owner's body set.
data RecoveryState = RecoveryState
  { recoveryAttempted :: Set.Set Name
  , recoveryGroups :: Map.Map Module [CoreBind]
  , recoveryPrepared :: Map.Map Module PreparedComponents
  , recoveryFailures :: [RecoveryFailure]
  , recoveryAdmitted :: Set.Set Module
  , recoveryReach :: PreparedReachability
  , recoveryDirty :: Set.Set Module
  , recoveryVersions :: Map.Map Module Int
  , recoveryRunning :: Set.Set Module
  , recoveryNewUnits :: [PreparedModule]
  , recoveryRebuild :: Bool
  }

-- | The recovered-body cache's exact group identity.  It is deliberately more
-- specific than the owner: recovery can prepare the same owner repeatedly as
-- its exact body set grows, and facts from one such set cannot describe the
-- next one.
preparedBodyKey :: PreparedModule -> (Module, [[Word64]])
preparedBodyKey prepared =
  (pmModule prepared,
    [ map (getKey . varUnique) (topBinders binding)
    | (binding, _) <- pmBindings prepared
    ])

preparationFailureFor :: Module -> RecoveryFailure -> Bool
preparationFailureFor owner (DefiningPreparationFailure failure) =
  recoveredFailureOwner failure == owner
preparationFailureFor _ _ = False

recoveredFailureOwner :: RecoveredModuleFailure -> Module
recoveredFailureOwner failure = case failure of
  RecoveredModuleFinderFailure owner _ -> owner
  RecoveredModuleInterfaceFailure owner _ -> owner
  RecoveredModulePreparationFailure owner _ -> owner

-- Full recursive groups supersede overlapping singleton groups. When a
-- newly discovered group bridges two previously disjoint groups, retain every
-- member from all of them; dropping the non-overlapping siblings would leave
-- references in the merged group unbound.
insertGroup :: CoreBind -> [CoreBind] -> [CoreBind]
insertGroup incoming previous =
  let names = Set.fromList (map varName (binders incoming))
      overlaps group = any ((`Set.member` names) . varName) (binders group)
      existing = filter overlaps previous
      existingNames group = Set.fromList (map varName (binders group))
      merged = Rec (mergePairs (concatMap pairsOf existing) (pairsOf incoming))
  in case existing of
       [] -> previous ++ [incoming]
       -- A full fat-interface Rec group is authoritative for the complete
       -- sibling set. Rediscovering a subset must not discard those siblings.
       -- Only a partial
       -- overlap needs a merge; in that case incoming pairs deterministically
       -- replace duplicate binders while retaining every sibling.
       [group] | names `Set.isSubsetOf` existingNames group -> previous
       _ -> filter (not . overlaps) previous ++ [merged]
  where
    pairsOf (NonRec binder body) = [(binder, body)]
    pairsOf (Rec pairs) = pairs

    -- When a partial overlap bridges groups, prefer the incoming body for a
    -- duplicate binder while retaining all siblings from every group.
    mergePairs existingPairs incomingPairs =
      foldl replace existingPairs incomingPairs

    replace pairs incomingPair@(binder, _) =
      filter ((/= varName binder) . varName . fst) pairs ++ [incomingPair]

binders :: CoreBind -> [Id]
binders (NonRec binder _) = [binder]
binders (Rec pairs) = map fst pairs
