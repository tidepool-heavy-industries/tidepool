use codegen::jit_machine::JitEffectMachine;
use core_bridge_derive::FromCore;
use core_effect::{EffectContext, EffectError, EffectHandler};
use core_eval::value::Value;
use rand::Rng;
use tidepool_macro::haskell_inline;

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Emit")]
    Emit(String),
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
            ConsoleReq::AwaitInt => {
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).unwrap();
                let n: i64 = input.trim().parse().unwrap_or(0);
                cx.respond(n)
            }
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
  emit "Your guess? "
  guess <- awaitInt
  if guess == target
    then emit "Correct!"
    else do
      emit (if guess < target then "Too low!" else "Too high!")
      guessLoop target
        "#
    };

    let mut vm =
        JitEffectMachine::compile(&expr, &table, 1 << 20).expect("JIT compilation failed");

    let mut handlers = frunk::hlist![ConsoleHandler, RngHandler(rand::thread_rng())];

    match vm.run(&table, &mut handlers, &()) {
        Ok(_) => println!("Game finished!"),
        Err(e) => eprintln!("Error: {}", e),
    }
}
