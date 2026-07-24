//! tidepool-harness — typed-yield session tree over the eval substrate.
//!
//! R0 scaffold: this crate currently defines the CROSS-SEGMENT CONTRACTS
//! (plans/harness-r0/00-scaffold/contracts.md) that the segment leaves
//! compile against. Segment 20 adds the resident-session registry, 30 the
//! event log / replay / forcing / protocol types' behavior, 60 the provider
//! implementations. Types here are the vocabulary, not the machinery —
//! keep them dependency-light.

pub mod log;
pub mod provider;
pub mod registry;
pub mod tree;
pub mod ui;

pub use registry::{Checkout, CheckoutError, SessionRegistry};
pub use tree::{FanBadge, HoleId, NodeId, NodeState, PriceClass, SiteId, Slot};
pub use ui::{BadgeKind, Ui};
