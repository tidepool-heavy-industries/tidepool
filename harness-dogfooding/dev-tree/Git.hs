{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | The typed git seam for the dev-tree harness: every git observation or
-- action the orchestrator performs in a worktree goes through here, as argv
-- (@git -C dir …@ — no shell, no quoting, no metachar expansion), with every
-- exit code inspected and every answer an honest sum.
--
-- Three rules, each bought by a live bug:
--
-- * __No exit code is ever ignored.__  A @git status@ that exits 128 with
--   empty stdout is a broken observation, not a clean tree; reporting it
--   clean once reaped a worker's entire uncommitted output.
-- * __Paths cross as machine output.__  @-z@ plus @core.quotePath=false@,
--   split on NUL — a path with a space or an accent never comes back
--   shell-quoted into a different path.  Diffs run @--no-renames@ so a file
--   MOVED out of a boundary shows up as the deletion it is.
-- * __Failure diagnoses are tail-biased and two-stream.__  Compilers and
--   test runners put the verdict at the end, often on stdout; the first
--   line of stderr is usually a banner.  'diagnose' is the one spelling.
module Git
  ( GitFailure (..)
  , renderGitFailure
  , diagnose
  , gitProc
  , gitRun
  , changedPaths
  , statusEntries
  , isAncestor
  , RebaseAttempt (..)
  , attemptRebase
  , treeDir
  ) where

import qualified Data.Text as T
import HarnessTypes (RepoPath, gitPath)
import Tidepool.Effects (WorktreeHandle (..), runArgv)
import Tidepool.Harness (Harness)
import Tidepool.Prelude
import Tidepool.QQ (fmt)
import Tidepool.Shell (renderExecError)
import Tidepool.Worktree (WorktreeReceipt (..))

-- | One git operation that did not deliver: the argv (for the record) and
-- why — a spawn failure, or a nonzero exit with its diagnosis.
data GitFailure = GitFailure
  { gitCmd    :: Text
  , gitDetail :: Text
  }
  deriving (Show, Eq)

renderGitFailure :: GitFailure -> Text
renderGitFailure f = [fmt|git {f.gitCmd}: {f.gitDetail}|]

-- | Tail-biased capture of a finished command's output, both streams.
diagnose :: Proc -> Text
diagnose p = case stream "stderr" p.stderr <> stream "stdout" p.stdout of
  [] -> "(no output)"
  parts -> T.intercalate "; " parts
  where
    stream label t = case lastNonBlank 5 t of
      [] -> []
      ls -> [label <> ": " <> T.intercalate " | " ls]
    lastNonBlank n t =
      let ls = filter (not . T.null . T.strip) (T.lines t)
       in drop (length ls - n) ls

treeDir :: WorktreeHandle -> Text
treeDir tree = tree.handleReceipt.cwd

-- | One git invocation in a worktree.  'Left' is a genuine spawn/dir
-- failure ONLY; the 'Proc' carries any exit, which the caller must inspect
-- (or use 'gitRun', which does).
gitProc :: WorktreeHandle -> [Text] -> Harness (Either GitFailure Proc)
gitProc tree args =
  runArgv ("git" : "-C" : treeDir tree : args) >>= \case
    Left e -> pure (Left (GitFailure (T.unwords args) (renderExecError e)))
    Right p -> pure (Right p)

-- | As 'gitProc', but a nonzero exit is also a 'Left', with its diagnosis.
-- The right default for every operation whose only good answer is success.
gitRun :: WorktreeHandle -> [Text] -> Harness (Either GitFailure Proc)
gitRun tree args =
  gitProc tree args >>= \case
    Left f -> pure (Left f)
    Right p
      | ok p -> pure (Right p)
      | otherwise -> pure (Left (GitFailure (T.unwords args) [fmt|exit {p.exitCode}: {diagnose p}|]))

nulSplit :: Text -> [Text]
nulSplit = filter (not . T.null) . T.splitOn "\0"

-- | Every path changed between @base@ and this worktree's HEAD — rename
-- sources included, machine-clean.
changedPaths :: WorktreeHandle -> Text -> Harness (Either GitFailure [RepoPath])
changedPaths tree base =
  gitRun tree ["-c", "core.quotePath=false", "diff", "--no-renames", "--name-only", "-z", base <> "..HEAD"]
    >>= \case
      Left f -> pure (Left f)
      Right p -> pure (Right (map gitPath (nulSplit p.stdout)))

-- | The worktree's dirty entries (@status --porcelain -z@), staged or not.
-- @Right []@ is an OBSERVED clean tree.  Entries come back raw (@XY path@;
-- a rename's origin path is its own entry without the status columns) —
-- exactly what the two uses need: an emptiness check, and a set-difference
-- baseline.
statusEntries :: WorktreeHandle -> Harness (Either GitFailure [Text])
statusEntries tree =
  gitRun tree ["-c", "core.quotePath=false", "status", "--porcelain", "-z"] >>= \case
    Left f -> pure (Left f)
    Right p -> pure (Right (nulSplit p.stdout))

-- | @merge-base --is-ancestor@, with git's three answers kept apart: exits
-- 0 and 1 are the two honest booleans; anything else (128 — bad object, no
-- repository) is a failure, never "not an ancestor".
isAncestor :: WorktreeHandle -> Text -> Text -> Harness (Either GitFailure Bool)
isAncestor tree ancestor descendant =
  gitProc tree ["merge-base", "--is-ancestor", ancestor, descendant] >>= \case
    Left f -> pure (Left f)
    Right p -> case p.exitCode of
      0 -> pure (Right True)
      1 -> pure (Right False)
      _ -> pure (Left (GitFailure "merge-base --is-ancestor" [fmt|exit {p.exitCode}: {diagnose p}|]))

-- | Tier-1 mechanical rebase, as a total sum.  A conflict is ABORTED before
-- this returns (verified — a failed abort is 'RebaseBroken', so no tier ever
-- inherits a worktree parked mid-rebase), and the conflicted paths are
-- captured first so tier 2's brief can name them.
data RebaseAttempt
  = -- | @onto@ is already an ancestor of HEAD; nothing to do.
    RebaseUnneeded
  | -- | Clean mechanical rebase landed.
    Rebased
  | -- | Genuine conflict, aborted; the paths git reported unmerged, and the
    -- rebase's own diagnosis.
    RebaseConflicted [RepoPath] Text
  | -- | The machinery itself failed — bad object, unreachable repository, a
    -- failed abort.  Never a resolution agent's problem.
    RebaseBroken GitFailure

attemptRebase :: WorktreeHandle -> Text -> Harness RebaseAttempt
attemptRebase tree onto =
  isAncestor tree onto "HEAD" >>= \case
    Left f -> pure (RebaseBroken f)
    Right True -> pure RebaseUnneeded
    Right False ->
      gitProc tree ["rebase", onto] >>= \case
        Left f -> pure (RebaseBroken f)
        Right p
          | ok p -> pure Rebased
          | otherwise -> do
              -- Advisory read, before the abort erases it: which paths were
              -- left unmerged.  A failed read degrades to an empty list, not
              -- a failed rebase report.
              unmerged <-
                gitProc tree ["-c", "core.quotePath=false", "diff", "--name-only", "--diff-filter=U", "-z"] >>= \case
                  Right up | ok up -> pure (map gitPath (nulSplit up.stdout))
                  _ -> pure []
              gitRun tree ["rebase", "--abort"] >>= \case
                Left f ->
                  pure (RebaseBroken f {gitDetail = [fmt|abort after conflict failed — worktree may be parked mid-rebase: {f.gitDetail}|]})
                Right _ -> pure (RebaseConflicted unmerged (diagnose p))
