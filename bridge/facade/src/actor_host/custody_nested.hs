let leaves = "leaves" :: ForkGroupLabel
let leaf = [label|leaf|]
nested <- unfold (subgroup leaves) (child (coding @Text currentCheckout (assignment leaf ("custody-leaf-reply" :: Text))))
