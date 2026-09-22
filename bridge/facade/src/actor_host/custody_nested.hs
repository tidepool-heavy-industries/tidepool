let leaves = "leaves" :: ForkGroupLabel
let leaf = "leaf" :: Label
nested <- unfold (subgroup leaves) (child (coding @Text boundHead (assignment leaf ("custody-leaf-reply" :: Text))))
