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

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::{template_haskell, CapturedOutput, EffectDecl};
use tidepool_repr::{
    BindingName, DataConTable, Generation, SessionId, SessionModule, SessionVarId,
};
use tidepool_runtime::session::{
    classify_turn, compile_session_turn, ModuleEnv, SessionBind, SessionLib, ValueTier,
};
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
    /// The value-plane bridge: `name → (SessionVarId, RootSlot, Val.G<g>)`
    /// (Wave 3b). Empty until the first `x <- e` / `let x = e`.
    bindings: BindingTable,
    /// Monotonic value-binding generation. Each bind mints a fresh
    /// `Tidepool.Session.Val.G<g>` so its `stableVarId` is collision-free and a
    /// rebind shadows without clobbering the prior root.
    val_gen: Generation,
    /// The DataConTable accumulated across turns (union via `insert_checked`), so
    /// a custom-ADT value bound earlier renders with real con names later.
    session_table: DataConTable,
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
            bindings: BindingTable::new(),
            val_gen: Generation(0),
            session_table: DataConTable::new(),
        })
    }

    /// The directory the session's `Val.G<g>.hi` ifaces are written to / read
    /// from. The same include root the Lane-A `Lib` modules live under, so a
    /// reference turn's `import Tidepool.Session.Val.G<g>` resolves from the
    /// injected HPT and the `.hi` path lines up with `writeSessionIface`.
    fn session_root(&self) -> &Path {
        &self.cfg.root
    }

    /// Module names of every live value binding — what a turn injects
    /// (`--inject-val`) AND imports so a session reference typechecks.
    fn live_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .live_modules()
            .map(|m| m.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The CURRENT (newest) `Val.G<g>` module per still-live name — what a turn
    /// IMPORTS (unqualified). This EXCLUDES shadowed older gens: a rebound name
    /// `x` is rooted under both `Val.G1` and `Val.G2`, and importing both
    /// unqualified makes every later `x` an ambiguous occurrence. Old gens are
    /// still INJECTED (see [`Self::live_val_modules`]) so already-compiled
    /// fragments / closure captures keep resolving — they just are not imported.
    fn current_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter_current()
            .map(|(_, entry)| entry.module.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The `imports` block a turn prepends: the current `Lib.G<g>` decl module
    /// (if any) plus the current `Val.G<g>` module of each live name (newest
    /// gen only — shadowed gens are injected but not imported).
    fn session_imports(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some(m) = self.lib.current_module() {
            lines.push(m.module_name());
        }
        lines.extend(self.current_val_modules());
        lines.join("\n")
    }

    /// Merge a turn's DataCons into the session-accumulated table (loud on a
    /// genuine `stableVarId` collision — gen-versioned names make that a real
    /// bug, not churn).
    fn merge_table(&mut self, table: &DataConTable) -> Result<(), String> {
        for dc in table.iter() {
            self.session_table
                .insert_checked(dc.clone())
                .map_err(|e| format!("session DataConTable collision: {e}"))?;
        }
        Ok(())
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

    /// `session_eval`: the Wave-3b dispatcher. GHC's parser classifies the turn
    /// (bind vs expr); a BIND (`x <- e` / `let x = e`) roots a value on the live
    /// heap, a reference-with-live-bindings injects the session ifaces, and a
    /// plain expression (no bindings) stays on the proven Wave-2 path.
    fn run_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        // Bind-vs-expr + bound names come from GHC (parse-only), never a Rust
        // scanner. A classify failure (e.g. extractor unavailable) falls back to
        // the plain path, where GHC re-reports any real error.
        let classification = match classify_turn(expr_text) {
            Ok(c) => c,
            Err(_) => return self.run_plain_eval(expr_text, handlers, captured),
        };

        if classification.is_bind {
            match classification.binders.as_slice() {
                // A bind statement with no extractable binder — treat as plain.
                [] => self.run_plain_eval(expr_text, handlers, captured),
                [name] => {
                    let name = name.clone();
                    self.run_bind(expr_text, name, handlers, captured)
                }
                // Multi-binder flat-tuple pattern (`(a, b) <- …`, `let (x,y) = …`):
                // run the action, project each tuple field, root each component.
                names => {
                    let names: Vec<String> = names.to_vec();
                    self.run_multi_bind(expr_text, names, handlers, captured)
                }
            }
        } else if !self.bindings.is_empty() {
            self.run_session_reference(expr_text, handlers, captured)
        } else {
            self.run_plain_eval(expr_text, handlers, captured)
        }
    }

    /// The proven Wave-2 expression path: compile an `M a` expression against the
    /// session include and run it on the resident machine. Unchanged — used when
    /// the turn neither binds nor references a session binding.
    fn run_plain_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
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

    /// BIND path (`x <- action` / `let x = e`): wrap into an `Eff`-typed
    /// `result = do { <stmt>; pure x }`, compile through the session extract
    /// (earlier bindings injected so `action` may reference them), then
    /// `run_fragment_and_bind` to reduce the effect tree, strict-force (Tier-0)
    /// or store-as-is (Tier-1), tenure + root the value, and record it in the
    /// `BindingTable`. The thin `Val.G<g>` iface was already written by the
    /// extract (the type plane); this wires the value plane to the same id.
    fn run_bind<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        name: String,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let g = self.val_gen.next();
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        let wrapped =
            wrap_bind_source(&self.cfg.preamble, &self.cfg.effect_stack, &imports, turn_text, &name);

        let lib_dir = self.lib.include_dir().to_path_buf();
        let mut include: Vec<&Path> = self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(lib_dir.as_path());

        let single = vec![name.clone()];
        let turn = match compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind { names: &single, gen: g.0 }),
        ) {
            Ok(t) => t,
            Err(e) => return TurnOutcome::Error(format!("bind compile error: {e}")),
        };
        if turn.warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in bound value. IO operations are not supported.".into(),
            );
        }
        let binder = match turn.binders.into_iter().next() {
            Some(b) => b,
            None => return TurnOutcome::Error("bind turn produced no binder metadata".into()),
        };
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(e);
        }

        // Bootstrap the resident machine on the first turn from THIS turn's table
        // (an Eff module → carries the effect ConTags the machine's dispatch
        // needs). Later binds re-enter the live machine.
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size) {
                Ok(m) => self.machine = Some(m),
                Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
            }
        }

        // Seed the env from EXISTING bindings (so `action` resolves earlier x's);
        // the new binding is added after it is rooted.
        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_bind_{}", self.turn_counter);
        let forced = matches!(binder.tier, ValueTier::Tier0Data);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(format!("JIT bind add_function error: {e}")),
        };
        let slot = match machine.run_fragment_and_bind(
            fid,
            &self.session_table,
            handlers,
            captured,
            forced,
        ) {
            Ok(s) => s,
            Err(e) => return TurnOutcome::Error(format!("bind runtime error: {e}")),
        };

        self.val_gen = g;
        let value = if forced {
            BoundValue::Tier0Forced(slot)
        } else {
            BoundValue::Tier1Closure(slot)
        };
        self.bindings.bind(BindingEntry {
            name: BindingName(name.clone()),
            id: SessionVarId::from_extract(binder.var_id),
            module: SessionModule::val(g),
            value,
            type_display: Some(binder.type_display.clone()),
        });
        TurnOutcome::Bound {
            name,
            type_display: binder.type_display,
        }
    }

    /// MULTI-BIND path: `(a, b) <- action` / `let (x, y) = e`. Wraps the turn as
    /// `result = do { <stmt>; pure (a, b, …) }` so the fragment yields ONE tuple
    /// Con, then projects each field individually, tenures each as a separate root,
    /// and records one `BindingEntry` per component. The extract validates that the
    /// result type is an N-tuple (via `splitTupleType`); non-tuple patterns (e.g.
    /// constructor patterns with the wrong return type) get a loud compile error.
    fn run_multi_bind<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        names: Vec<String>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let g = self.val_gen.next();
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        let wrapped = wrap_multi_bind_source(
            &self.cfg.preamble,
            &self.cfg.effect_stack,
            &imports,
            turn_text,
            &names,
        );

        let lib_dir = self.lib.include_dir().to_path_buf();
        let mut include: Vec<&Path> =
            self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(lib_dir.as_path());

        let turn = match compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind { names: &names, gen: g.0 }),
        ) {
            Ok(t) => t,
            Err(e) => return TurnOutcome::Error(format!("multi-bind compile error: {e}")),
        };
        if turn.warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in bound value. IO operations are not supported.".into(),
            );
        }
        if turn.binders.len() != names.len() {
            return TurnOutcome::Error(format!(
                "multi-bind: extract returned {} binders, expected {}",
                turn.binders.len(),
                names.len()
            ));
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(e);
        }

        if self.machine.is_none() {
            match JitEffectMachine::compile_session(
                &turn.expr,
                &turn.table,
                self.cfg.nursery_size,
            ) {
                Ok(m) => self.machine = Some(m),
                Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
            }
        }

        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_multi_bind_{}", self.turn_counter);

        // Build forced_mask: Tier0 fields get deep-forced, Tier1 closures don't.
        let forced_mask: Vec<bool> = turn
            .binders
            .iter()
            .map(|b| matches!(b.tier, ValueTier::Tier0Data))
            .collect();

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(
            &frag_name,
            &turn.expr,
            &self.session_table,
            &env,
        ) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(format!("JIT multi-bind add_function error: {e}")),
        };
        let slots = match machine.run_fragment_and_bind_projected(
            fid,
            &self.session_table,
            handlers,
            captured,
            &forced_mask,
        ) {
            Ok(s) => s,
            Err(e) => return TurnOutcome::Error(format!("multi-bind runtime error: {e}")),
        };

        self.val_gen = g;
        // Zip binders with their slots and record each component.
        let mut bound_names: Vec<(String, String)> = Vec::new();
        for (binder, slot) in turn.binders.iter().zip(slots.into_iter()) {
            let value = if matches!(binder.tier, ValueTier::Tier0Data) {
                BoundValue::Tier0Forced(slot)
            } else {
                BoundValue::Tier1Closure(slot)
            };
            self.bindings.bind(BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(g),
                value,
                type_display: Some(binder.type_display.clone()),
            });
            bound_names.push((binder.name.clone(), binder.type_display.clone()));
        }
        TurnOutcome::MultiBound { components: bound_names }
    }

    /// REFERENCE path (a bare expression mentioning session bindings). Try the
    /// `Eff` wrap first (so `pure (...)` / effectful references work exactly like
    /// Wave-2); on a type error, retry as a PURE value (`result = <expr>`, run
    /// purely) so bare references like `x + 1` / `f 10` / `v ^? key …` resolve.
    /// Both wraps inject the live `Val` ifaces so the reference typechecks.
    fn run_session_reference<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        let lib_dir = self.lib.include_dir().to_path_buf();
        let mut include: Vec<&Path> = self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(lib_dir.as_path());

        // Eff-first.
        let eff_src = template_haskell(
            &self.cfg.preamble,
            &self.cfg.effect_stack,
            expr_text,
            &imports,
            "",
            None,
            None,
        );
        match compile_session_turn(&eff_src, &include, self.session_root(), &inject, None) {
            Ok(turn) => self.run_reference_fragment(turn, false, handlers, captured),
            Err(eff_err) => {
                // Pure fallback.
                let pure_src = wrap_pure_ref_source(&self.cfg.preamble, &imports, expr_text);
                match compile_session_turn(&pure_src, &include, self.session_root(), &inject, None) {
                    Ok(turn) => self.run_reference_fragment(turn, true, handlers, captured),
                    Err(pure_err) => TurnOutcome::Error(format!(
                        "reference compile error (as Eff: {eff_err}) (as pure value: {pure_err})"
                    )),
                }
            }
        }
    }

    /// Run a compiled reference fragment on the resident machine, resolving any
    /// session binders through the seeded `ExternalEnv` (load-through-slot).
    fn run_reference_fragment<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn: tidepool_runtime::session::SessionTurnResult,
        pure: bool,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        if turn.warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in result binding. IO operations are not supported.".into(),
            );
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(e);
        }
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size) {
                Ok(m) => self.machine = Some(m),
                Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
            }
        }
        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_ref_{}", self.turn_counter);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(format!("JIT reference add_function error: {e}")),
        };
        let run_result = if pure {
            machine.run_fragment_pure(fid)
        } else {
            machine.run_fragment(fid, &self.session_table, handlers, captured)
        };
        match run_result {
            Ok(value) => TurnOutcome::Value(value_to_json(&value, &self.session_table, 0)),
            Err(e) => TurnOutcome::Error(format!("runtime error: {e}")),
        }
    }

    /// `session_cmd`: meta-commands. `:bindings` / `:reset` work; `:t` / `:i`
    /// are Wave-4 stubs (no captured-type plane yet).
    fn run_meta(&mut self, meta: &MetaCommand) -> TurnOutcome {
        match meta {
            MetaCommand::Bindings => {
                let mut bindings: Vec<serde_json::Value> = self
                    .bindings
                    .iter_current()
                    .map(|(name, entry)| {
                        serde_json::json!({
                            "name": name.0,
                            "type": entry.type_display.clone().unwrap_or_default(),
                            "module": entry.module.module_name(),
                            "tier": if entry.value.is_forced() { "Tier0Data" } else { "Tier1Closure" },
                        })
                    })
                    .collect();
                bindings.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                TurnOutcome::Meta(serde_json::json!({
                    "bindings": bindings,
                    "generation": self.lib.generation().0,
                    "valGeneration": self.val_gen.0,
                }))
            }
            MetaCommand::Reset => {
                // Drop the resident machine (frees the session heap + every
                // persistent root) and clear both planes: the decl log (reopen
                // the lib at gen 0) and the value bindings + session table.
                self.machine = None;
                self.turn_counter = 0;
                self.bindings = BindingTable::new();
                self.val_gen = Generation(0);
                self.session_table = DataConTable::new();
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

// ---------------------------------------------------------------------------
// Turn-wrapping helpers (Wave 3b)
// ---------------------------------------------------------------------------

/// Insert `import <m>` lines into the eval preamble at the same point
/// `template_haskell` uses (after the standard imports, before `default (Int`),
/// so user/session imports are in scope. Mirrors `eval_prep::template_haskell`.
fn insert_imports(preamble: &str, imports: &str) -> String {
    if imports.trim().is_empty() {
        return preamble.to_string();
    }
    let insert_point = preamble.find("default (Int").unwrap_or(preamble.len());
    let mut out = String::new();
    out.push_str(&preamble[..insert_point]);
    for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
        out.push_str(&format!("import {imp}\n"));
    }
    out.push_str(&preamble[insert_point..]);
    out
}

/// Wrap a BIND turn into an `Eff`-typed module whose `result` runs the bind
/// statement and yields the bound value, so `run_fragment_and_bind` reduces the
/// effect tree and roots that value. The monad is pinned to the session effect
/// stack (`Eff <stack> _`, the value type inferred via `PartialTypeSignatures`,
/// which the preamble enables) — a bare `pure (42 :: Int)` action would
/// otherwise leave the monad ambiguous.
fn wrap_bind_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    binder: &str,
) -> String {
    let mut out = insert_imports(preamble, imports);
    out.push_str("-- [user]\n");
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do\n");
    for line in turn_text.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str(&format!("  pure {binder}\n"));
    out
}

/// Wrap a MULTI-BIND turn into an `Eff`-typed module whose `result` runs the
/// bind statement and yields a tuple of all bound names. For `(a, b) <- action`
/// with `names = ["a", "b"]` this emits:
/// ```haskell
/// result :: Eff <stack> _
/// result = do
///   (a, b) <- action
///   pure (a, b)
/// ```
/// The tuple field order matches the binder order in the JSON sidecar.
fn wrap_multi_bind_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    names: &[String],
) -> String {
    let tuple_expr = format!("({})", names.join(", "));
    let mut out = insert_imports(preamble, imports);
    out.push_str("-- [user]\n");
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do\n");
    for line in turn_text.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str(&format!("  pure {tuple_expr}\n"));
    out
}

/// Wrap a PURE reference turn as `result = <expr>` (no `Eff`), run via
/// `run_fragment_pure`. For bare value references like `x + 1` / `f 10` /
/// `v ^? key …` that are not monadic.
fn wrap_pure_ref_source(preamble: &str, imports: &str, expr_text: &str) -> String {
    let mut out = insert_imports(preamble, imports);
    out.push_str("-- [user]\n");
    out.push_str("result =\n");
    for line in expr_text.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out
}
