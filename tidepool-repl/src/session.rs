//! The resident session: ONE live [`JitEffectMachine`] + the Lane-A decl
//! library, driven turn-by-turn. The value plane (heap) persists across
//! `session_eval` turns via the Wave-1 re-entry APIs (`compile_session` for the
//! first turn, then `add_function` + `run_fragment` for each later turn on the
//! SAME machine).
//!
//! The `Session<Open>` / `Session<Closed>` type-state (domain §5) is applied
//! through [`SessionHandle`]: `close` consumes the open handle and returns a
//! `Closed` one with no `run` method, so post-close turns don't typecheck.
//!
//! No value binding here (`x <- e` / the BindingTable / the C iface) — that is
//! Wave 3. A `session_eval` turn compiles a fresh `M a` against the session
//! include and runs it; the heap persisting is the mechanism Wave 3b builds on.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::{template_haskell, CapturedOutput, EffectDecl};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_runtime::{compile_haskell_salted, value_to_json};

use crate::command::{MetaCommand, SessionCommand, TurnOutcome};

/// Default session nursery: 64 MiB (matches the eval runtime default).
pub const DEFAULT_NURSERY_SIZE: usize = 1 << 26;

/// Static configuration for a resident session, assembled once at open.
#[derive(Clone)]
pub struct SessionConfig {
    pub id: SessionId,
    /// Root of the session include tree (`Tidepool/Session/Lib/G<g>.hs` live here).
    pub root: PathBuf,
    /// Base GHC include dirs (generated `Tidepool.Effects` dir + prelude/stdlib).
    pub base_include: Vec<PathBuf>,
    /// Effect decls for this server (`[Console, Ask]` for the Wave-2 MVP).
    pub decls: Vec<EffectDecl>,
    /// The assembled eval preamble (from `tidepool_mcp::build_preamble`).
    pub preamble: String,
    /// The effect-stack type string (e.g. `'[Console, Ask]`).
    pub effect_stack: String,
    /// The `Ask` effect's tag (its index in `decls`).
    pub ask_tag: u64,
    /// Import/pragma surface for the generated `Lib.G<g>` decl modules.
    pub module_env: ModuleEnv,
    /// Session nursery size in bytes.
    pub nursery_size: usize,
}

/// The resident session — the value plane + the decl plane + a generation.
/// Owned by the worker thread; reached only through a [`SessionHandle`].
pub struct Session {
    cfg: SessionConfig,
    lib: SessionLib,
    /// The live machine. `None` until the first `session_eval` bootstraps it
    /// (via `compile_session`); later turns re-enter it via `add_function`.
    machine: Option<JitEffectMachine>,
    /// Monotonic per-turn counter, for unique fragment function names.
    turn_counter: u64,
}

impl Session {
    /// Open a fresh session rooted at `cfg.root`.
    pub fn open(cfg: SessionConfig) -> std::io::Result<Session> {
        let lib = SessionLib::open(cfg.id, cfg.root.clone(), cfg.module_env.clone())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Session {
            cfg,
            lib,
            machine: None,
            turn_counter: 0,
        })
    }

    /// Run one non-`Close` turn. Errors are folded into [`TurnOutcome::Error`]
    /// (the worker maps that to an MCP error result); an in-turn `ask` suspends
    /// through `handlers` (the [`crate::ask::ReplAskDispatcher`]), not here.
    pub fn run_turn<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        cmd: &SessionCommand,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        match cmd {
            SessionCommand::Def(decl) => self.run_def(&decl.0),
            SessionCommand::Eval(expr) => self.run_eval(&expr.0, handlers, captured),
            SessionCommand::Cmd(meta) => self.run_meta(meta),
            // Close is handled by SessionHandle::close (type-state); the worker
            // never routes it here.
            SessionCommand::Close => TurnOutcome::Error(
                "internal: Close must be handled via SessionHandle::close".into(),
            ),
        }
    }

    /// `session_def`: append the declaration to the Lane-A log + regenerate the
    /// gen-versioned `Lib.G<g>` module.
    fn run_def(&mut self, decl_text: &str) -> TurnOutcome {
        match self.lib.define(decl_text) {
            Ok(gen) => TurnOutcome::Defined {
                generation: gen.0,
                module: tidepool_repr::SessionModule::lib(gen).module_name(),
            },
            Err(e) => TurnOutcome::Error(format!("session_def failed: {e}")),
        }
    }

    /// `session_eval`: compile the `M a` expression against the current session
    /// include and run it on the resident machine (bootstrapping the machine on
    /// the first turn). The heap persists across turns — that is the point.
    fn run_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        // Bring the accumulated declarations into scope (if any). `import_line`
        // carries the leading `import` keyword; template_haskell re-adds it, so
        // pass the bare module name.
        let imports = self
            .lib
            .current_module()
            .map(|m| format!("{}\n", m.module_name()))
            .unwrap_or_default();
        let source = template_haskell(
            &self.cfg.preamble,
            &self.cfg.effect_stack,
            expr_text,
            &imports,
            "",
            None,
            None,
        );

        // Include the session lib dir at highest precedence so `Lib.G<g>`
        // resolves; salt by (session, gen) so per-session modules don't collide.
        let lib_dir = self.lib.include_dir().to_path_buf();
        let mut include: Vec<&Path> = self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(lib_dir.as_path());
        let salt = self.lib.cache_salt();

        let (expr, mut table, warnings) =
            match compile_haskell_salted(&source, "result", &include, Some(&salt)) {
                Ok(triple) => triple,
                Err(e) => return TurnOutcome::Error(format!("compile error: {e}")),
            };
        if warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in result binding. IO operations are not supported.".into(),
            );
        }
        table.populate_siblings_from_expr(&expr);

        // First turn bootstraps the resident machine; later turns re-enter it
        // (a fresh fragment in the SAME live JITModule, heap preserved). The
        // fragment name is minted unconditionally so it's unique per turn.
        self.turn_counter += 1;
        let frag_name = format!("repl_turn_{}", self.turn_counter);
        let run_result = match self.machine {
            Some(ref mut machine) => {
                match machine.add_function(&frag_name, &expr, &table, &ExternalEnv::new()) {
                    Ok(fid) => machine.run_fragment(fid, &table, handlers, captured),
                    Err(e) => return TurnOutcome::Error(format!("JIT re-entry error: {e}")),
                }
            }
            None => {
                let mut m =
                    match JitEffectMachine::compile_session(&expr, &table, self.cfg.nursery_size) {
                        Ok(m) => m,
                        Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
                    };
                let r = m.run(&table, handlers, captured);
                self.machine = Some(m);
                r
            }
        };

        match run_result {
            Ok(value) => TurnOutcome::Value(value_to_json(&value, &table, 0)),
            Err(e) => TurnOutcome::Error(format!("runtime error: {e}")),
        }
    }

    /// `session_cmd`: meta-commands. `:bindings` / `:reset` work; `:t` / `:i`
    /// are Wave-4 stubs (no captured-type plane yet).
    fn run_meta(&mut self, meta: &MetaCommand) -> TurnOutcome {
        match meta {
            MetaCommand::Bindings => TurnOutcome::Meta(serde_json::json!({
                // Value bindings arrive in Wave 3b; for now this is always empty.
                "bindings": [],
                "generation": self.lib.generation().0,
            })),
            MetaCommand::Reset => {
                // Drop the resident machine (frees the session heap) and clear
                // the in-memory decl log by reopening the lib at generation 0.
                self.machine = None;
                self.turn_counter = 0;
                match SessionLib::open(
                    self.cfg.id,
                    self.cfg.root.clone(),
                    self.cfg.module_env.clone(),
                ) {
                    Ok(lib) => {
                        self.lib = lib;
                        TurnOutcome::Meta(serde_json::json!({"reset": true}))
                    }
                    Err(e) => TurnOutcome::Error(format!("reset failed: {e}")),
                }
            }
            MetaCommand::Type(_) | MetaCommand::Info(_) => TurnOutcome::Meta(serde_json::json!({
                "note": ":t / :i are not yet implemented (Wave 4 — captured-type plane)",
            })),
        }
    }

    /// Drop the resident machine, freeing the session heap. Called from
    /// [`SessionHandle::close`].
    fn free(&mut self) {
        // JitEffectMachine::drop calls free_session_heap for session machines.
        self.machine = None;
    }
}

// ---------------------------------------------------------------------------
// Type-state: Open vs Closed (domain §5)
// ---------------------------------------------------------------------------

/// Phantom marker: the session is open and accepts turns.
pub struct Open;
/// Phantom marker: the session is closed; no turns can be run.
pub struct Closed;

/// A session handle parameterized by lifecycle state. Only `SessionHandle<Open>`
/// has `run`; `close` consumes it and yields a `SessionHandle<Closed>`, so a
/// post-close turn is a compile error (the kimi-r2 #11 type-state mandate).
pub struct SessionHandle<S> {
    inner: Session,
    _state: PhantomData<S>,
}

impl SessionHandle<Open> {
    /// Wrap a freshly-opened session as an open handle.
    pub fn new(session: Session) -> SessionHandle<Open> {
        SessionHandle {
            inner: session,
            _state: PhantomData,
        }
    }

    /// Run one turn against the open session.
    pub fn run<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        cmd: &SessionCommand,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        self.inner.run_turn(cmd, handlers, captured)
    }

    /// The current declaration generation (0 until the first `session_def`).
    pub fn generation(&self) -> u64 {
        self.inner.lib.generation().0
    }

    /// Consume the open handle: free the resident machine and transition to
    /// `Closed`. The returned handle has no `run`.
    pub fn close(mut self) -> SessionHandle<Closed> {
        self.inner.free();
        SessionHandle {
            inner: self.inner,
            _state: PhantomData,
        }
    }
}

// SessionHandle<Closed> deliberately has NO `run` — post-close turns don't
// typecheck. (It is otherwise inert; the worker drops it after close.)
