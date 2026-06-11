{-# LANGUAGE NoImplicitPrelude, FlexibleInstances #-}
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
-- exactly as "Tidepool.Prelude"'s @show@ does; @Double@ thereby goes through
-- the existing @ShowDoubleAddr@ interception in Translate.hs.
module Tidepool.Render (Render(..)) where

import Data.Text (Text)
import qualified Data.Text as T
import Prelude (Int, Double, Bool, Char, String, Show, id, (.))
import qualified Prelude as P

class Render a where
  render :: a -> Text

instance Render Text where
  render = id
  {-# INLINE render #-}

instance Render String where
  render = T.pack
  {-# INLINE render #-}

instance Render Int where
  render = renderShow
  {-# INLINE render #-}

instance Render Double where
  render = renderShow
  {-# INLINE render #-}

instance Render Bool where
  render = renderShow
  {-# INLINE render #-}

instance Render Char where
  render = renderShow
  {-# INLINE render #-}

-- | @Text@-returning 'show', matching "Tidepool.Prelude"'s @show@.
renderShow :: Show a => a -> Text
renderShow = T.pack . P.show
{-# INLINE renderShow #-}
