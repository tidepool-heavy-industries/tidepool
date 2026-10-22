{-# LANGUAGE DataKinds, QuasiQuotes #-}
{-# LANGUAGE FlexibleContexts, UndecidableInstances #-}
{{CELL_PRAGMAS}}
module CellCheck where
import Control.Monad.Freer (Eff)
import Data.List (sort)
{{CELL_IMPORTS}}
__tidepoolInEffectRow :: Eff '[] value -> Eff '[] value
__tidepoolInEffectRow = id
{{CELL_DECLS}}
__tidepool_cell_check :: Eff '[] ()
__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }
