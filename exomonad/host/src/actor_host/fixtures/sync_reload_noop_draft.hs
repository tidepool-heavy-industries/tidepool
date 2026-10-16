import qualified Persona.Memory as Memory
Right () <- Memory.replaceBlock Memory.Human "Human memory updated without refreshing the published context."
display (bOnly == 42 && Work.answerFn 1 == 42)
