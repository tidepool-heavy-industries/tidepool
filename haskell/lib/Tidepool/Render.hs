{-# LANGUAGE NoImplicitPrelude, FlexibleInstances, UndecidableInstances, OverloadedStrings #-}
-- | The coercion class for @[fmt|...|]@ holes: turn a value into 'Text'.
--
-- Single-method, total instances (no error branches), so JIT-safe — the same
-- shape as 'Tidepool.Prelude.Pack'.  Kept in its own lens-free module (rather
-- than directly in "Tidepool.Prelude") so the test/regen extract session —
-- which cannot see @lens@ and therefore cannot load "Tidepool.Prelude" — can
-- still import @render@.  "Tidepool.Prelude" re-exports @Render(render)@, so
-- MCP eval code gets it through the usual @import Tidepool.Prelude@.
--
-- The numeric\/'Bool'\/'Char' instances route through @Data.Text.pack . show@,
-- exactly as "Tidepool.Prelude"'s @show@ does; @Double@ uses the stable
-- managed-Text intrinsic in "Tidepool.Double".
module Tidepool.Render (Render(..)) where

import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import Prelude (Int, Double, Bool(..), Char, String, Show, Maybe(..), Either(..), id, (.), (>), (<), (&&))
import qualified Prelude as P
import Tidepool.Double (renderDoublePrec)

class Render a where
  render :: a -> Text
  render = renderPrec 0

  renderPrec :: Int -> a -> Text
  renderPrec _ = render

instance Render Text where
  render = id
  {-# INLINE render #-}

instance {-# OVERLAPPING #-} Render String where
  render = T.pack
  {-# INLINE render #-}

instance Render Int where
  renderPrec p x = parenthesize (p > 6 && x < 0) (renderShow x)
  {-# INLINE renderPrec #-}

instance Render Double where
  renderPrec = renderDoublePrec
  {-# INLINE renderPrec #-}

instance Render Bool where
  render = renderShow
  {-# INLINE render #-}

instance Render Char where
  render = renderShow
  {-# INLINE render #-}

instance {-# OVERLAPPABLE #-} Render a => Render [a] where
  render xs = T.concat ["[", T.intercalate "," (P.map (renderPrec 0) xs), "]"]
  {-# INLINE render #-}

instance Render a => Render (Maybe a) where
  renderPrec _ Nothing = "Nothing"
  renderPrec p (Just x) = parenthesize (p > 10) (T.concat ["Just ", renderPrec 11 x])
  {-# INLINE renderPrec #-}

instance (Render a, Render b) => Render (Either a b) where
  renderPrec p (Left x) = parenthesize (p > 10) (T.concat ["Left ", renderPrec 11 x])
  renderPrec p (Right x) = parenthesize (p > 10) (T.concat ["Right ", renderPrec 11 x])
  {-# INLINE renderPrec #-}

instance {-# OVERLAPPABLE #-} Show a => Render a where
  render = renderShow
  {-# INLINE render #-}

-- | @Text@-returning 'show', matching "Tidepool.Prelude"'s @show@.
renderShow :: Show a => a -> Text
renderShow = T.pack . P.show
{-# INLINE renderShow #-}

parenthesize :: Bool -> Text -> Text
parenthesize True x = T.concat ["(", x, ")"]
parenthesize False x = x
{-# INLINE parenthesize #-}
