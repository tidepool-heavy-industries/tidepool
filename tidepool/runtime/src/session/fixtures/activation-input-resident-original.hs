data Input = HiddenOriginal Int

make :: Int -> Input
make = HiddenOriginal

project :: Input -> Int
project (HiddenOriginal value) = value

let originalProject = project :: Input -> Int
let originalInput = make 42
