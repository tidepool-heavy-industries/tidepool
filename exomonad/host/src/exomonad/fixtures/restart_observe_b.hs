import qualified Project.Work as Work
import qualified Project.ExtraCheck as Extra
import qualified Tidepool.Effects.Authored as Files
import qualified Data.Text as T
Right (_, workB) <- readSourceModule "Project.Work"
Right expectedB <- Files.readFile "reload-inputs/work-b.txt"
Right initialized <- Files.readFile "initializer-count.txt"
Right expectedEffects <- Files.readFile "expected-effects.txt"
Right effects <- Files.readFile "effect-count.txt"
rejectedDraft <- reloadSource ["Project.ExtraCheck"]
let checks =
      [ ("selected-module", workB == expectedB)
      , ("compiled-work", Work.answerFn 1 == 42)
      , ("compiled-extra", Extra.extraCheck == 73)
      , ("retained-declaration", bRemembered == 42)
      , ("initializer-once", initialized == "x")
      , ("effects-once", effects == expectedEffects)
      , ("invalid-draft-rejected", case rejectedDraft of { Right (ReloadRejected _ _ _) -> True; _ -> False })
      ] :: [(T.Text, Bool)]
let failedChecks = [name | (name, passed) <- checks, not passed]
if null failedChecks
  then display True
  else error ("restart checks failed: " <> T.intercalate ", " failedChecks)
