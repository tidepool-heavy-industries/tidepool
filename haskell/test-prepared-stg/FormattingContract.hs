module FormattingContract where

import Data.Text (Text)
import Data.Text qualified as Text
import Tidepool.Double (renderDouble, renderDoublePrec)

formatValue :: Double -> Text
formatValue value = renderDouble value

continuation :: Int
continuation = Text.length (renderDouble 1.5) + 7

positiveLazyPrecedence :: Text
positiveLazyPrecedence = renderDoublePrec (error "precedence must remain lazy") 1.5

negativeZero :: Text
negativeZero = renderDoublePrec 7 (-0.0)

partialApplication :: Double -> Text
partialApplication = renderDoublePrec 7
