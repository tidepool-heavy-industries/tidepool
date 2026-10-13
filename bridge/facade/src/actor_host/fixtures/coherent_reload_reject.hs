Right invalidWork <- Files.readFile "reload-inputs/work-invalid.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" invalidWork
Right refused <- reloadSource []
display (case refused of
  ReloadRejected _ _ _ -> bOnly == 42 && Work.answerFn 1 == 42
  _ -> False)
