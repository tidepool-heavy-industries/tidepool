let Right leaves = forkGroupLabel "leaves"
let Right leaf = branchLabel "leaf"
nested <- unfold (subgroup leaves) (child (coding @Text leaf boundHead ("custody-leaf-reply" :: Text)))
