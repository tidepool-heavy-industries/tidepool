//! This crate's instantiation of the promoted session-ownership registry —
//! see `tidepool_runtime::session::registry` for the mechanism itself (the
//! one home, per the root `CLAUDE.md` Mechanism Index) and [`crate::tree::Slot`]
//! for this crate's `Slot` alias. Fixes the hole-identity type parameter to
//! [`crate::tree::HoleId`] so every existing `SessionRegistry<M>`/
//! `Checkout<'_, M>`/`CheckoutError` call site in this crate keeps its
//! single-type-parameter shape unchanged.

use crate::tree::HoleId;

pub type SessionRegistry<M> = tidepool_runtime::session::registry::SessionRegistry<M, HoleId>;
pub type Checkout<'r, M> = tidepool_runtime::session::registry::Checkout<'r, M, HoleId>;
pub type CheckoutError = tidepool_runtime::session::registry::CheckoutError<HoleId>;
