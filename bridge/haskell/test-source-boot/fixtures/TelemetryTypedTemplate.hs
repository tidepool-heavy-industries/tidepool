{-# LANGUAGE DataKinds #-}
{{CELL_PRAGMAS}}
module TelemetryTyped where
import Control.Monad.Freer (Eff)
import Tidepool.Effects.Core ()
{{CELL_IMPORTS}}
{{CELL_DECLS}}
__tidepool_cell_check :: Eff '[] ()
__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }
