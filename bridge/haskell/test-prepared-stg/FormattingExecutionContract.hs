module FormattingExecutionContract where

import Data.Text (Text)
import FormattingContract qualified as Formatting
import Tidepool.Double (renderDoublePrec)

formatValue :: Text
formatValue = Formatting.formatValue 1.5

formatContinuation :: Int
formatContinuation = Formatting.continuation

positiveLazyPrecedence :: Text
positiveLazyPrecedence = renderDoublePrec (let precedence = precedence in precedence) 1.5

negativeZeroPrec :: Text
negativeZeroPrec = Formatting.negativeZero

partialValue :: Text
partialValue = Formatting.partialApplication (-0.0)
