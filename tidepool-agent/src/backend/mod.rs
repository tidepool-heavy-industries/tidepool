//! Backend adapters.
//!
//! One module per backend. A backend module owns its wire types, its process
//! lifecycle, and its correlation bookkeeping, and exposes only
//! [`crate::seam`] vocabulary.

pub mod codex;
