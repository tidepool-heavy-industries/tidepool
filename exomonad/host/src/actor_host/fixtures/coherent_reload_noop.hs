import qualified Persona.Memory as Memory

Right () <- Memory.replaceBlock Memory.Human "Human memory updated without refreshing the published context."
Right noChange <- reloadSource []
display (case noChange of
  ReloadUnchanged _ -> bOnly == 42 && Work.answerFn 1 == 42
  _ -> False)
