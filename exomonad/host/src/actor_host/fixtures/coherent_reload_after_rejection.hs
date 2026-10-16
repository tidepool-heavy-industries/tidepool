Right workBExpected <- Files.readFile "reload-inputs/work-b.txt"
Right (_, runningAfterRefusal) <- readSourceModule "Project.Work"
Right humanAfterReload <- Memory.recall Memory.Human
display (runningAfterRefusal == workBExpected && bOnly == 42
  && humanAfterReload == "Human memory updated without refreshing the published context.")
