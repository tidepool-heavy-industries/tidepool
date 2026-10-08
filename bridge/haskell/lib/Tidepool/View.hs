-- | Pure presentation combinators. Domain renderers are ordinary functions.
module Tidepool.View (View, text, markdown, row, column, caption, inspect, svg, image, ImageData(..), ImageFormat(..), SvgDocument(..)) where
import Data.Text (Text)
import Tidepool.View.Types
import Prelude ((.))
import Tidepool.Inspection.Display (Display(..))
text, markdown :: Text -> View
text = PlainText
markdown = Markdown
row, column :: [View] -> View
row = Row
column = Column
caption :: View -> Text -> View
caption = Caption
inspect :: Display a => a -> View
inspect = Inspection . displayTree
svg :: SvgDocument -> View
svg = Vector
image :: ImageData -> Text -> View
image = Image
