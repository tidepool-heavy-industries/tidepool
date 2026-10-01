{-# LANGUAGE QuasiQuotes #-}
let refusedBranch = withContext (selected id) (withLifetime ActorOwned $ coding @Text projectHead (assignment [label|refused-worker|] ("No allocation" :: Text)))
let refusedChild name = workChild name refusedBranch
beforeRefusal <- length . snapshotActors <$> snapshot
emptyRefusal <- unfoldWork (batch "typed-batch" "empty") [] (keepWork :: WorkSink Text)
blankRefusal <- unfoldWork (batch "typed-batch" "blank") [refusedChild "  "] keepWork
duplicateRefusal <- unfoldWork (batch "typed-batch" "duplicate") [refusedChild "same", refusedChild "same"] keepWork
contextRefusal <- unfoldWork (batch "typed-batch" "uncaptured") [workChild "uncaptured" (withContext inherited refusedBranch)] keepWork
afterRefusal <- length . snapshotActors <$> snapshot
(case emptyRefusal of { Left EmptyWorkBatch -> True; _ -> False }) && (case blankRefusal of { Left EmptyWorkName -> True; _ -> False }) && (case duplicateRefusal of { Left (DuplicateWorkName "same") -> True; _ -> False }) && (case contextRefusal of { Left (WorkAdmissionRefused (UnfoldUncapturedContext _)) -> True; _ -> False }) && beforeRefusal == afterRefusal
