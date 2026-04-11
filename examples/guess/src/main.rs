//! Number guessing game — JIT-compiled version.
//!
//! Demonstrates Tidepool's end-to-end workflow: a Haskell effect program is compiled
//! at build time via `haskell_inline!`, JIT-compiled to native code via Cranelift,
//! and driven by Rust-side effect handlers for console IO and random number generation.

use rand::Rng;
use std::io::Write;
use tidepool_bridge_derive::FromCore;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
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

struct ConsoleHandler;

impl EffectHandler for ConsoleHandler {
    type Request = ConsoleReq;

    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Emit(s) => {
                println!("{}", s);
                cx.respond(())
            }
            ConsoleReq::Prompt(s) => {
                print!("{}", s);
                std::io::stdout().flush().ok();
                cx.respond(())
            }
            ConsoleReq::AwaitInt => loop {
                let mut input = String::new();
                let bytes = std::io::stdin()
                    .read_line(&mut input)
                    .map_err(|e| EffectError::Handler(format!("stdin read failed: {e}")))?;
                if bytes == 0 {
                    return Err(EffectError::Handler("stdin closed (EOF)".into()));
                }
                let trimmed = input.trim();
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

struct RngHandler(rand::rngs::ThreadRng);

impl EffectHandler for RngHandler {
    type Request = RngReq;

    fn handle(&mut self, req: RngReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            RngReq::RandInt(lo, hi) => {
                let n = self.0.gen_range(lo..=hi);
                cx.respond(n)
            }
        }
    }
}

fn main() {
    let (expr, table) = haskell_inline! {
        target = "game",
        include = "haskell",
        r#"
game :: Eff '[Console, Rng] ()
game = do
  target <- randInt 1 100
  emit "I'm thinking of a number between 1 and 100."
  guessLoop target

guessLoop :: Int -> Eff '[Console, Rng] ()
guessLoop target = do
  prompt "Your guess? "
  guess <- awaitInt
  if guess == target
    then emit "Correct!"
    else do
      emit (if guess < target then "Too low!" else "Too high!")
      guessLoop target
        "#
    };

    let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20).expect("JIT compilation failed");

    let mut handlers = frunk::hlist![ConsoleHandler, RngHandler(rand::thread_rng())];

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
