Right invalidWork <- Files.readFile "reload-inputs/work-invalid.txt"
Right () <- Files.writeFile ".exomonad/Project/Work.hs" invalidWork
display (bOnly == 42 && Work.answerFn 1 == 42)
