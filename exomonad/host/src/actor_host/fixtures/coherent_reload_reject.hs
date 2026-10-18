Right invalidWork <- Files.readFile "reload-inputs/work-invalid.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" invalidWork
Right refused <- reloadSource []
case refused of
  ReloadRejected _ _ _ -> display (bOnly == 42 && Work.answerFn 1 == 42) >> pure ()
  unexpected -> display (show unexpected) >> pure ()
