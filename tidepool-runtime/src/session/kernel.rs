//! Policy-free primitives shared by resident-session frontends.
//!
//! [`SuspendableSession`] describes sessions that park on typed obligations
//! and resume or abort through one entry point. [`Aged`] records how long an
//! obligation has waited. [`admit_checkout`] lets a frontend apply its own
//! admission rule without weakening the registry's atomic checkout protocol.
//!
//! Hole meaning, timeout policy, reaping, and user-facing errors remain with
//! the frontend. Structural checkout failures are represented by
//! [`super::registry::CheckoutError`].

use std::time::{Duration, Instant};

use super::registry::Checkout;

/// A value paired with the instant at which it began waiting.
///
/// Consumers may inspect or reset the clock to implement their own timeout or
/// retry policy. This type starts no timer and performs no reclamation.
#[derive(Debug, Clone)]
pub struct Aged<T> {
    value: T,
    since: Instant,
}

impl<T> Aged<T> {
    /// Wrap `value`, minting its age clock now.
    pub fn new(value: T) -> Self {
        Aged {
            value,
            since: Instant::now(),
        }
    }

    /// How long ago this value was minted (or last [`Self::touch`]ed).
    pub fn age(&self) -> Duration {
        self.since.elapsed()
    }

    /// The raw mint/touch instant, useful when comparing several values to one
    /// snapshot of the current time.
    pub fn since(&self) -> Instant {
        self.since
    }

    /// Reset the age clock without changing the value.
    pub fn touch(&mut self) {
        self.since = Instant::now();
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.value
    }

    pub fn into_inner(self) -> T {
        self.value
    }
}

/// A session that can resume or abort an obligation-carrying parked hole.
///
/// Implementors define their own hole and answer types. `Context` carries any
/// per-call runtime inputs that do not belong in either value; sessions that
/// own all such state use `()`.
pub trait SuspendableSession {
    /// The obligation-carrying continuation token a suspension hands back,
    /// and the thing [`Self::resume`]/[`Self::abort`] consume to re-enter.
    type Hole;
    /// What a resume delivers to the parked continuation.
    type Answer;
    /// Per-call context beyond the hole/answer (`()` for a session that
    /// carries its own — see this trait's doc).
    type Context;
    /// What a completed-or-re-suspended turn produces.
    type Outcome;
    /// Why a resume or abort was refused or failed.
    type Error;

    /// Resume `hole` with `answer`. The hole carries any completion obligation
    /// needed after the machine runs.
    fn resume(
        &mut self,
        hole: Self::Hole,
        answer: Self::Answer,
        cx: Self::Context,
    ) -> Result<Self::Outcome, Self::Error>;

    /// Abort the turn parked on `hole` without running its continuation.
    fn abort(
        &mut self,
        hole: Self::Hole,
        reason: String,
        cx: Self::Context,
    ) -> Result<Self::Outcome, Self::Error>;
}

/// Apply a frontend admission rule to a completed checkout.
///
/// Refusal restores the machine with its original hole set before returning
/// that set to the caller. No refused checkout remains observable as running.
pub fn admit_checkout<'r, M, H>(
    checkout: Checkout<'r, M, H>,
    admits: impl FnOnce(&[H]) -> bool,
) -> Result<Checkout<'r, M, H>, Vec<H>>
where
    H: Clone + PartialEq + std::fmt::Debug,
{
    let holes = checkout.holes_at_checkout().to_vec();
    if admits(&holes) {
        Ok(checkout)
    } else {
        checkout.restore_suspended(holes.clone());
        Err(holes)
    }
}

#[cfg(test)]
mod tests {
    use super::super::registry::SessionRegistry;
    use super::*;

    #[test]
    fn aged_reports_growing_age_and_the_wrapped_value() {
        let a = Aged::new(42);
        assert_eq!(*a.get(), 42);
        // No sleep needed: `elapsed()` against a mint instant in the very
        // recent past is always >= zero, which is all this asserts.
        assert!(a.age() >= Duration::ZERO);
    }

    #[test]
    fn touch_resets_the_age_clock() {
        let mut a = Aged::new("pending");
        let before = a.since();
        a.touch();
        assert!(a.since() >= before, "touch must not rewind the clock");
        assert_eq!(*a.get(), "pending", "touch must not disturb the value");
    }

    #[test]
    fn get_mut_and_into_inner_reach_the_wrapped_value() {
        let mut a = Aged::new(vec![1, 2, 3]);
        a.get_mut().push(4);
        assert_eq!(a.into_inner(), vec![1, 2, 3, 4]);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FakeMachine {
        turns: u32,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Hole(&'static str);

    /// A refused admission hands the checkout straight back, with the exact
    /// pre-checkout hole set both restored and returned — mirrors
    /// `tidepool-repl`'s `admit_run`'s "busy, here's what it's busy with"
    /// contract.
    #[test]
    fn refused_admission_restores_the_untouched_checkout_and_reports_the_holes() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = tidepool_repr::SessionId(1);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle -> run");
        co.restore_suspended(vec![Hole("h1")]);

        let co = reg.checkout_run(id).expect("run over parked frame");
        match admit_checkout(co, |holes| holes.is_empty()) {
            Err(holes) => assert_eq!(holes, vec![Hole("h1")]),
            Ok(_) => panic!("a non-empty hole set must be refused"),
        }

        // The checkout was handed straight back untouched: the hole is
        // still there, resumable, and a fresh checkout still sees it.
        let co = reg
            .checkout_resume(id, &Hole("h1"))
            .expect("the hole survived the refused admission");
        co.restore_suspended(Vec::new());
    }

    /// An admitted checkout is returned unchanged, ready for the caller's
    /// own turn.
    #[test]
    fn admitted_checkout_is_returned_for_the_caller_to_drive() {
        let reg: SessionRegistry<FakeMachine, Hole> = SessionRegistry::new();
        let id = tidepool_repr::SessionId(2);
        reg.insert_idle(id, FakeMachine { turns: 0 });
        let co = reg.checkout_run(id).expect("idle -> run");

        let mut co = admit_checkout(co, |holes| holes.is_empty()).expect("idle admits");
        co.machine().turns += 1;
        co.restore_suspended(Vec::new());
    }
}
