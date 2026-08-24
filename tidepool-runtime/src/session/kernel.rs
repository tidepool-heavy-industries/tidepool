//! The resident-session suspension kernel (#22) — the generalized "park on a
//! typed hole, resolve externally, resume through one entry point" seam both
//! `tidepool-repl` and `tidepool-harness` reimplement independently today.
//! See `plans/resident-session-kernel-design.md` for the full survey; this
//! module is the operative artifact of that design's §3.2/§5.B.
//!
//! # Packaging: a module here, not a new crate
//!
//! Open Question 2 left the crate-vs-module call to implementation judgment,
//! binding only on three goals: one shared mechanism, cleanly factored
//! components, and abstraction boundaries expressed through the type system.
//! A module wins on the concrete argument the design doc's §4.3 already
//! made: [`super::resident::ResidentSession`]'s encapsulation (`HoleSeed`,
//! `PlainHole`/`BindingHole`'s absent public constructors) is deliberately
//! tight, and a crate outside `tidepool-runtime` would need real `pub`
//! promotions to reach it — a module needs none. The precedent (the
//! `session::registry` promotion) also landed as a module, not a crate, for
//! the identical shape of sharing problem.
//!
//! # What the kernel owns vs. what stays a consumer's own policy
//!
//! Per §3.2/§3.3: the kernel owns the SHAPE of "one obligation-carrying hole
//! token, one resume/abort entry point per token" ([`SuspendableSession`])
//! and the abandonment-liveness primitive ([`Aged`]) — never the domain
//! meaning of a hole (harness's `SuspensionRouting`), never a consumer's own
//! admission policy (repl's `admit_run`), never a background reaper.
//!
//! The kernel does NOT introduce a new shared error-taxonomy TYPE distinct
//! from what already exists: `tidepool_runtime::session::registry::
//! CheckoutError<H>` already IS repl's structural three-way distinction
//! (`Unknown`/`NoSession` ~ "no suspension at all", `WrongHole{attempted,
//! parked}` ~ "suspended on a different hole", `Running`/`Terminal` for the
//! busy/terminal cases) plus harness's breadth, carried as STRUCTURED data
//! all the way to a caller since the OQ4 fix
//! (`HarnessError::SessionMismatch` no longer flattens it to a `String`).
//! Inventing a second, kernel-owned error enum here would be exactly the
//! "third shape" the one-mechanism rule forbids — `CheckoutError<H>` is
//! already the shared taxonomy for the resume-rejection space; a consumer's
//! own `Error` associated type ([`SuspendableSession::Error`]) is free to
//! wrap it (as [`super::resident::ResidentError`] already does not, but
//! could) rather than re-deriving it.

use std::time::{Duration, Instant};

/// A value paired with the [`Instant`] it was minted — the one home for "how
/// long has this been sitting unanswered" (§3.2 item 5: the abandonment-
/// liveness contract). Generalizes `tidepool-repl`'s hand-rolled
/// `Suspension.since` field (`tidepool-repl/src/manager.rs`) into something
/// a second consumer (`tidepool-harness`'s `PendingSuspension`) can reuse
/// instead of re-deriving its own copy of "value + mint time + age query".
///
/// This is the WHOLE of the kernel's abandonment-liveness surface: hole age
/// is visible via [`Self::age`], and a consumer wires whatever sweep/TTL
/// policy it wants on top (repl's periodic reaper; a harness that later
/// wants one). The kernel drives no timer and reclaims nothing on its own —
/// per Open Question 3, indefinite park stays the default for every
/// consumer that never reads [`Self::age`] at all.
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

    /// The raw mint/touch instant — for a caller comparing against a
    /// snapshot `now` rather than calling [`Self::age`] twice at slightly
    /// different instants (mirrors repl's reaper, which reads `Instant::now()`
    /// once per sweep and compares every pending clock against it).
    pub fn since(&self) -> Instant {
        self.since
    }

    /// Reset the age clock without touching the value — repl's anti-
    /// starvation move: "a retrying continuation must not become the
    /// reaper's oldest-first eviction victim while its caller fixes an
    /// invalid reply" (`tidepool-repl/src/manager.rs`'s
    /// `refresh_suspension_since`).
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

/// The kernel's generalized suspension seam (§3.2 items 1-2, §5.B step 1): a
/// session that either runs a turn to completion or parks on an obligation-
/// carrying [`Self::Hole`] token, resumed or aborted through exactly one
/// entry point per operation — never a second, externally-dispatched family
/// the way repl's five parallel `resume_*` functions are today.
///
/// # Why `Hole` is an opaque associated type, not a kernel-defined enum
///
/// §6.3's constraint: the token design must not hard-code one payload per
/// hole kind, so a future materialization policy (or #20 step 3's
/// polymorphic fork/finalize answer types) never forces a second migration.
/// An associated type lets each implementor own its own token shape —
/// [`super::resident::ResidentHole`] for the harness (already a real sum
/// over completion obligations, `Plain`/`Binding`), and a session that needs
/// a THIRD obligation kind later widens its own `Hole` type, never this
/// trait.
///
/// # Why `resume`/`abort` take a `Context`
///
/// [`super::resident::ResidentSession`] owns its captured-output buffer and
/// handler stack as FIELDS, so its impl needs nothing extra per call
/// (`Context = ()`). `tidepool-repl`'s slot-path `Session` takes its abort
/// gate and output buffer as PER-CALL arguments instead (`run_turn`/
/// `resume_turn`/`abort_turn`'s existing signatures) — a structural
/// difference between the two mechanisms this trait must not paper over by
/// forcing repl to start storing per-request state on itself. `Context`
/// carries exactly that per-call context through the ONE entry point rather
/// than smuggling it into `Hole`/`Answer`, which stay pure domain data.
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
    /// Why a resume/abort was refused or failed. A consumer's own error type
    /// — this trait does not mandate one shared enum (see the module doc);
    /// a consumer wrapping [`super::registry::CheckoutError`] gets the
    /// shared structural taxonomy for free.
    type Error;

    /// The ONE resume entry point: `hole`'s own variant decides what
    /// re-entering it requires and what completing it must still do (e.g. a
    /// value-plane materialization) — never a second, caller-chosen
    /// function for a different obligation shape.
    fn resume(
        &mut self,
        hole: Self::Hole,
        answer: Self::Answer,
        cx: Self::Context,
    ) -> Result<Self::Outcome, Self::Error>;

    /// Abort the turn parked on `hole` WITHOUT running the continuation —
    /// the `ask` itself fails, the turn unwinds, and the session comes back
    /// usable with everything already accumulated intact.
    fn abort(
        &mut self,
        hole: Self::Hole,
        reason: String,
        cx: Self::Context,
    ) -> Result<Self::Outcome, Self::Error>;
}

#[cfg(test)]
mod tests {
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
}
