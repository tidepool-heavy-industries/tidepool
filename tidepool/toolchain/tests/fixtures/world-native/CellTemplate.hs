{-# LANGUAGE DataKinds #-}
{{CELL_PRAGMAS}}
module CellCheck where
import Control.Monad.Freer (Eff)
{{CELL_IMPORTS}}
{{CELL_DECLS}}
__tidepool_cell_check :: Eff '[] ()
__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }
