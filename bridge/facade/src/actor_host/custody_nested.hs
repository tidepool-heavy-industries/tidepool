let leaves = "leaves" :: ForkGroupLabel
let leaf = [label|leaf|]
nested <- unfold (subgroup leaves) (child (coding @Text boundHead (assignment leaf ("custody-leaf-reply" :: Text))))
