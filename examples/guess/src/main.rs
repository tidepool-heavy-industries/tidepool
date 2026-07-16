//! Number guessing game — JIT-compiled version.
//!
//! Demonstrates Tidepool's end-to-end workflow: a Haskell effect program is compiled
//! at build time via `haskell_inline!`, JIT-compiled to native code via Cranelift,
//! and driven by Rust-side effect handlers for console IO and random number generation.
//!
//! Both effects here (`Console`, `Rng`) stay hand-rolled rather than going through
//! `tidepool_handlers::build_base_stack`:
//!
//! - `Rng` has no standard-stack equivalent at all — it is this example's
//!   intentional custom-handler demo point (see [`RngHandler`]).
//! - `Console` LOOKS like it should reuse `tidepool_handlers::ConsoleHandler`, but
//!   that handler only exposes a fire-and-forget `Print` verb that appends to an
//!   MCP `CapturedOutput` buffer (for eval JSON responses) — it has no verb for a
//!   synchronous stdin read. This game needs `Prompt` (no-newline) and `AwaitInt`
//!   (blocking read + reprompt-on-bad-input) against the REAL terminal, which the
//!   MCP-oriented Console can't do. So it stays custom too (see [`ConsoleHandler`]).

use rand::Rng;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tidepool_bridge_derive::FromCore;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_macro::haskell_inline;

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Emit")]
    Emit(String),
    #[core(name = "Prompt")]
    Prompt(String),
    #[core(name = "AwaitInt")]
    AwaitInt,
}

/// Where `AwaitInt` reads its next line from. `main` uses `Stdin` (the real
/// terminal); tests use `Scripted` to drive the game deterministically.
enum InputSource {
    Stdin,
    #[cfg_attr(not(test), allow(dead_code))]
    Scripted(VecDeque<String>),
}

/// Every line `Emit`/`Prompt` writes, in order — always recorded (cheap:
/// `Arc<Mutex<Vec<String>>>`), only ever read back by the `#[cfg(test)]` smoke
/// test via [`EmitLog::drain`].
#[derive(Clone, Default)]
struct EmitLog(Arc<Mutex<Vec<String>>>);

impl EmitLog {
    fn push(&self, line: String) {
        self.0.lock().unwrap().push(line);
    }

    #[cfg(test)]
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Real-terminal Console: `Emit`/`Prompt` print to actual stdout, `AwaitInt`
/// blocks on a real (or scripted) line source. See the module doc comment for
/// why this can't be `tidepool_handlers::ConsoleHandler`.
struct ConsoleHandler {
    source: InputSource,
    log: EmitLog,
}

impl ConsoleHandler {
    fn new() -> Self {
        ConsoleHandler {
            source: InputSource::Stdin,
            log: EmitLog::default(),
        }
    }

    /// Test-only constructor: `AwaitInt` is served from `lines` instead of real
    /// stdin (exhausting `lines` behaves like stdin EOF). Returns the handler
    /// plus a cloned [`EmitLog`] handle for asserting on emitted output after
    /// the JIT run completes.
    #[cfg(test)]
    fn scripted(lines: &[&str]) -> (Self, EmitLog) {
        let log = EmitLog::default();
        let handler = ConsoleHandler {
            source: InputSource::Scripted(lines.iter().map(|s| s.to_string()).collect()),
            log: log.clone(),
        };
        (handler, log)
    }
}

impl EffectHandler for ConsoleHandler {
    type Request = ConsoleReq;

    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ConsoleReq::Emit(s) => {
                println!("{}", s);
                self.log.push(s);
                cx.respond(())
            }
            ConsoleReq::Prompt(s) => {
                print!("{}", s);
                std::io::stdout().flush().ok();
                self.log.push(s);
                cx.respond(())
            }
            ConsoleReq::AwaitInt => loop {
                let trimmed = match &mut self.source {
                    InputSource::Stdin => {
                        let mut input = String::new();
                        let bytes = std::io::stdin()
                            .read_line(&mut input)
                            .map_err(|e| EffectError::Handler(format!("stdin read failed: {e}")))?;
                        if bytes == 0 {
                            return Err(EffectError::Handler("stdin closed (EOF)".into()));
                        }
                        input.trim().to_string()
                    }
                    InputSource::Scripted(lines) => match lines.pop_front() {
                        Some(line) => line,
                        None => return Err(EffectError::Handler("stdin closed (EOF)".into())),
                    },
                };
                match trimmed.parse::<i64>() {
                    Ok(n) => return cx.respond(n),
                    Err(_) => {
                        println!("'{trimmed}' isn't a number — try again.");
                    }
                }
            },
        }
    }
}

#[derive(FromCore)]
enum RngReq {
    #[core(name = "RandInt")]
    RandInt(i64, i64),
}

/// This example's intentional custom-handler demo point: `Rng` has no
/// standard-stack equivalent (`build_base_stack` has nothing like it), so it
/// stays hand-rolled by design, not by omission. Holds a boxed generator
/// rather than `rand::rngs::ThreadRng` directly so a test can swap in a fixed
/// target without depending on any particular RNG algorithm/version.
struct RngHandler(Box<dyn FnMut(i64, i64) -> i64>);

impl RngHandler {
    fn thread_rng() -> Self {
        let mut rng = rand::thread_rng();
        RngHandler(Box::new(move |lo, hi| rng.gen_range(lo..=hi)))
    }

    /// Test-only constructor: every `RandInt` call returns `n`, regardless of
    /// range — makes the guessing game's target deterministic for a scripted
    /// smoke test.
    #[cfg(test)]
    fn fixed(n: i64) -> Self {
        RngHandler(Box::new(move |_, _| n))
    }
}

impl EffectHandler for RngHandler {
    type Request = RngReq;

    fn handle(
        &mut self,
        req: RngReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            RngReq::RandInt(lo, hi) => {
                let n = (self.0)(lo, hi);
                cx.respond(n)
            }
        }
    }
}

fn main() {
    let (expr, table) = haskell_inline! {
        target = "game",
        include = "haskell",
    };

    let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20).expect("JIT compilation failed");

    let mut handlers = frunk::hlist![ConsoleHandler::new(), RngHandler::thread_rng()];

    match vm.run(&table, &mut handlers, &()) {
        Ok(_) => println!("Game finished!"),
        Err(e) => {
            // Clean exit when the user closed stdin (ctrl-D / piped EOF).
            if format!("{e}").contains("stdin closed") {
                println!("Goodbye!");
            } else {
                eprintln!("Error: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real compiled effect program (same `haskell_inline!` source as
    /// `main`) through the JIT with a fixed Rng target and scripted guesses —
    /// a compile-and-run smoke test, not a unit test of `guessLoop` in
    /// isolation. Binary-searches 1..100 for `target`, so it's robust to the
    /// exact search strategy the Haskell source uses changing later.
    #[test]
    fn guess_reaches_correct_with_scripted_input() {
        let target: i64 = 37;
        let mut guesses = Vec::new();
        let (mut lo, mut hi) = (1i64, 100i64);
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            guesses.push(mid.to_string());
            if mid == target {
                break;
            } else if mid < target {
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        let guesses: Vec<&str> = guesses.iter().map(String::as_str).collect();

        let (expr, table) = haskell_inline! {
            target = "game",
            include = "haskell",
        };

        let mut vm =
            JitEffectMachine::compile(&expr, &table, 1 << 20).expect("JIT compilation failed");
        let (console, log) = ConsoleHandler::scripted(&guesses);
        let mut handlers = frunk::hlist![console, RngHandler::fixed(target)];

        vm.run(&table, &mut handlers, &())
            .expect("scripted game must run to completion, not hit EOF");

        let emitted = log.drain();
        assert_eq!(
            emitted.last().map(String::as_str),
            Some("Correct!"),
            "game did not reach the winning guess: {emitted:?}"
        );
    }
}
