cellIdentity :: CellNominal -> CellNominal
cellIdentity value = value

data CellNominal = CellNominal Text
  deriving Show

h <- pure Nothing

let fixed = h :: Maybe CellNominal

fixed
