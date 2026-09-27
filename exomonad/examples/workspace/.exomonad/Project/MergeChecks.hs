{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.MergeChecks (redPreserved) where

import Prelude hiding (writeFile)
import Control.Monad (void)
import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as Text
import Tidepool.Check

-- A failed integration check leaves its staged and unstaged diagnostics in
-- the managed checkout while the named publication branch stays at the
-- previous green head. Its next publish is refused by branch drift.
redPreserved :: Member RecipeCheck effects => Eff effects ()
redPreserved = do
  owner <- root
  before <- git owner ["rev-parse", "HEAD"]
  ownerPath <- git owner ["rev-parse", "--show-toplevel"]
  let externalEdit directory name line = do
        writeFile owner "fixture.patch" (Text.unlines
          [ "diff --git a/" <> name <> " b/" <> name
          , "new file mode 100644", "--- /dev/null", "+++ b/" <> name
          , "@@ -0,0 +1 @@", "+" <> line ])
        void $ git owner ["-C", directory, "apply", ownerPath <> "/fixture.patch"]
  let branch = "recipe/red-preserved"
  void $ git owner ["branch", branch, before]
  created <- turn owner $ Text.unlines
    [ "import qualified Project.Merge as M"
    , "Right sourceTree <- createWorktree (fromRef \"recipe/red-preserved\" \"red-source\")"
    , "Right integration <- createWorktree (fromRef \"recipe/red-preserved\" \"red-preserved\")"
    , "merger <- R.start (M.mergeInto (worktreeId integration) (Just \"recipe/red-preserved\") [\"sh\", \"-c\", \"printf 'staged-check\\n' > red-preserved.txt; git add -- red-preserved.txt; printf 'working-check\\n' > red-preserved.txt; printf intentional-red >&2; exit 7\"])"
    , "worktreeId integration"
    ]
  check "the integration actor owns a managed checkout"
    ("WorktreeId" `Text.isInfixOf` lastOutput created)
  pathResult <- turn owner "cwd (handleReceipt integration)"
  let integrationPath = Text.dropAround (== '"') (Text.strip (lastOutput pathResult))
  reflogBefore <- git owner ["-C", integrationPath, "reflog", "--format=%gs"]
  sourcePathResult <- turn owner "cwd (handleReceipt sourceTree)"
  let sourcePath = Text.dropAround (== '"') (Text.strip (lastOutput sourcePathResult))
  externalEdit sourcePath "red-preserved.txt" "candidate"
  void $ git owner ["-C", sourcePath, "add", "--", "red-preserved.txt"]
  void $ git owner ["-C", sourcePath, "commit", "-m", "red preservation candidate", "--", "red-preserved.txt"]
  candidate <- git owner ["-C", sourcePath, "rev-parse", "HEAD"]
  void $ turn owner $ Text.unlines
    [ "let request = M.PublishRequest \"red preservation\" (worktreeId sourceTree) " <> gitOidLiteral candidate <> " \"merge red preservation candidate\""
    , "first <- R.call (M.publish (R.client merger)) request"
    ]
  externalEdit integrationPath "post-check.txt" "post-check-edit"
  result <- turn owner $ Text.unlines
    [ "headAfter <- worktreeHead integration"
    , "second <- R.call (M.publish (R.client merger)) request"
    , "(first, headAfter == Right " <> gitOidLiteral candidate <> ", second)"
    ]
  let observed = lastOutput result
  check "the red result retains the previous and checked heads plus failed check evidence"
    (all (`Text.isInfixOf` observed)
      ["RedPreserved", before, candidate, "intentional-red"])
  retained <- turn owner $ "inspectFull (headAfter == Right " <> gitOidLiteral candidate
    <> " && case second of { M.MergeBlocked reason -> \"publication branch\" `T.isInfixOf` reason; _ -> False })"
  check ("the red head is retained and next publish refused; observed: " <> observed)
    (lastOutput retained == "True")
  published <- git owner ["rev-parse", "refs/heads/" <> branch]
  check "the named publication branch remains at the previous green head"
    (published == before)
  status <- git owner ["-C", integrationPath, "status", "--short", "--", "red-preserved.txt"]
  staged <- git owner ["-C", integrationPath, "diff", "--cached", "--", "red-preserved.txt"]
  working <- git owner ["-C", integrationPath, "diff", "--", "red-preserved.txt"]
  check "the failed check's staged and working edits survive in the integration checkout"
    (status == "MM red-preserved.txt"
      && "staged-check" `Text.isInfixOf` staged
      && "working-check" `Text.isInfixOf` working)
  reflog <- git owner ["-C", integrationPath, "reflog", "--format=%gs"]
  check "the red path never records a destructive reset"
    (not (any (Text.isInfixOf "reset:")
      (take (length (Text.lines reflog) - length (Text.lines reflogBefore)) (Text.lines reflog))))

  later <- git owner ["-C", integrationPath, "status", "--short", "--", "post-check.txt"]
  check "a later refused publish preserves edits made after the failed check"
    (later == "?? post-check.txt")
  void $ turn owner "R.finish merger"
