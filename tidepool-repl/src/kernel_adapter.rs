//! Implements `tidepool_runtime::session::SuspendableSession` for this
//! crate's slot-path [`Session`] (#22 design doc §5.B step 3) — a
//! THROWAWAY adapter: it exists only to let repl consume the kernel's
//! error taxonomy and admission-hook shape ahead of Phase 6
//! (`plans/one-session.md`), and is deleted the moment Phase 6 converts
//! repl onto `tidepool_runtime::session::ResidentSession` directly (at
//! which point repl uses the exact same trait impl the harness already
//! does — see `tidepool-runtime/src/session/resident.rs`'s own impl — no
//! adapter needed).
//!
//! [`Session`] is single-hole by construction (`Session::is_suspended`) and
//! has no per-hole identity of its own — that lives one layer up, in
//! `manager.rs`'s [`ContinuationId`] bookkeeping (repl's own policy, design
//! doc §3.3). So [`SlotHole`] carries the id purely for API symmetry with
//! the harness's `ResidentHole` (logging, routing) — it is never consulted
//! by this adapter to pick which of the five `PendingTail` variants to
//! resume. Presenting that five-way split as ONE token variant is exactly
//! this adapter's job, done by taking `Session`'s own stowed suspension
//! wholesale through [`Session::resume_turn`]/[`Session::abort_turn`],
//! unchanged.

use std::sync::Arc;

use tidepool_effect::pause::PauseGate;
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::session::SuspendableSession;

use crate::manager::ContinuationId;
use crate::session::{Session, TurnStep};

/// The repl slot-path's kernel token — see this module's doc for why it
/// carries only an id, never a `PendingTail` variant choice.
#[derive(Clone, Debug)]
pub struct SlotHole(pub ContinuationId);

/// Per-call context [`Session::resume_turn`]/[`Session::abort_turn`] need
/// beyond the hole and the answer: the turn's abort gate and its
/// output-capture buffer — both owned PER-REQUEST by repl's caller (the
/// server), never fields of [`Session`] itself. This is the structural
/// difference from `ResidentSession` (which owns its captured buffer as a
/// field) the kernel trait's `Context` associated type exists to cross —
/// see `tidepool_runtime::session::kernel`'s module doc.
pub struct SlotContext {
    pub gate: Arc<PauseGate>,
    pub captured: CapturedOutput,
}

/// Why a kernel-shaped resume/abort was refused. Repl's OWN error type, not
/// a kernel-defined enum (the trait's `Error` associated type is
/// deliberately open — see `kernel`'s module doc). `NotSuspended` is the one
/// new, structurally-typed outcome this adapter adds over what
/// [`Session::resume_turn`]/[`Session::abort_turn`] do today: a "no
/// suspended turn" call there folds into a stringly-typed
/// `TurnOutcome::Error` inside a `Completed` outcome; here it is a real
/// variant a caller can match on without string-sniffing, matching the
/// structural (never-flattened-to-`String`) discipline the design doc's
/// §3.2 item 3 / OQ4 asks of the shared resume-rejection space.
#[derive(Debug, thiserror::Error)]
pub enum SlotKernelError {
    #[error("session has no suspended turn to resume/abort")]
    NotSuspended,
}

impl SuspendableSession for Session {
    type Hole = SlotHole;
    type Answer = serde_json::Value;
    type Context = SlotContext;
    type Outcome = TurnStep;
    type Error = SlotKernelError;

    fn resume(
        &mut self,
        _hole: Self::Hole,
        answer: Self::Answer,
        cx: Self::Context,
    ) -> Result<Self::Outcome, Self::Error> {
        if !self.is_suspended() {
            return Err(SlotKernelError::NotSuspended);
        }
        Ok(self.resume_turn(answer, cx.gate, &cx.captured))
    }

    fn abort(
        &mut self,
        _hole: Self::Hole,
        reason: String,
        cx: Self::Context,
    ) -> Result<Self::Outcome, Self::Error> {
        if !self.is_suspended() {
            return Err(SlotKernelError::NotSuspended);
        }
        Ok(self.abort_turn(reason, cx.gate, &cx.captured))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{BoxedStack, SessionConfig, DEFAULT_NURSERY_SIZE};
    use tidepool_mcp::EffectRoster;
    use tidepool_repr::SessionId;

    fn test_session(dir: &std::path::Path) -> Session {
        let cfg = SessionConfig {
            id: SessionId(1),
            root: dir.join("session-1"),
            base_include: Vec::new(),
            roster: EffectRoster::from_handlers(&frunk::HNil),
            preamble: String::new(),
            effect_stack: String::new(),
            module_env: tidepool_runtime::session::ModuleEnv::standalone_default(),
            nursery_size: DEFAULT_NURSERY_SIZE,
        };
        Session::open(cfg, Box::new(|| Box::new(frunk::HNil) as BoxedStack))
            .expect("a session opens on a fresh dir")
    }

    /// The adapter's whole reason to exist: a caller driving `Session`
    /// through the kernel's `SuspendableSession` seam gets a REAL,
    /// structurally-typed refusal for "nothing is suspended" — never a
    /// silent `TurnStep::Completed(TurnOutcome::Error(..))` a caller has to
    /// string-match to notice.
    #[test]
    fn resume_on_an_idle_session_is_a_structural_not_suspended_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = test_session(dir.path());
        let cx = SlotContext {
            gate: PauseGate::new(),
            captured: CapturedOutput::new(),
        };

        let result = SuspendableSession::resume(
            &mut session,
            SlotHole(ContinuationId("scont_1".to_string())),
            serde_json::json!(null),
            cx,
        );

        assert!(matches!(result, Err(SlotKernelError::NotSuspended)));
    }

    #[test]
    fn abort_on_an_idle_session_is_a_structural_not_suspended_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = test_session(dir.path());
        let cx = SlotContext {
            gate: PauseGate::new(),
            captured: CapturedOutput::new(),
        };

        let result = SuspendableSession::abort(
            &mut session,
            SlotHole(ContinuationId("scont_1".to_string())),
            "abandoned".to_string(),
            cx,
        );

        assert!(matches!(result, Err(SlotKernelError::NotSuspended)));
    }
}
