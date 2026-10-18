import qualified Persona.Memory as Memory

Right () <- Memory.replaceBlock Memory.Human "Human memory updated without refreshing the published context."
Right noChange <- reloadSource []
case noChange of
  ReloadUnchanged _ -> display (bOnly == 42 && Work.answerFn 1 == 42) >> pure ()
  unexpected -> display (show unexpected) >> pure ()
