use core_bridge_derive::FromCore;
use core_effect::{EffectError, EffectHandler, EffectMachine};
use core_eval::value::Value;
use core_repr::{DataConTable, Literal};
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

impl ConsoleHandler {
    fn make_unit(table: &DataConTable) -> Value {
        match table.get_by_name("()") {
            Some(id) => Value::Con(id, vec![]),
            None => Value::Lit(Literal::LitInt(0)),
        }
    }
}

impl EffectHandler for ConsoleHandler {
    type Request = ConsoleReq;

    fn handle(&mut self, req: ConsoleReq, table: &DataConTable) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Emit(s) => {
                println!("{}", s);
                Ok(Self::make_unit(table))
            }
            ConsoleReq::AwaitInt => {
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).unwrap();
                let n: i64 = input.trim().parse().unwrap_or(0);
                let result = match table.get_by_name("I#") {
                    Some(id) => Value::Con(id, vec![Value::Lit(Literal::LitInt(n))]),
                    None => Value::Lit(Literal::LitInt(n)),
                };
                Ok(result)
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

    fn handle(&mut self, req: RngReq, table: &DataConTable) -> Result<Value, EffectError> {
        match req {
            RngReq::RandInt(lo, hi) => {
                let n: i64 = self.0.gen_range(lo..=hi);
                let result = match table.get_by_name("I#") {
                    Some(id) => Value::Con(id, vec![Value::Lit(Literal::LitInt(n))]),
                    None => Value::Lit(Literal::LitInt(n)),
                };
                Ok(result)
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

    let mut heap = core_eval::heap::VecHeap::new();
    let mut handlers = frunk::hlist![ConsoleHandler, RngHandler(rand::thread_rng())];

    let result = EffectMachine::new(&table, &mut heap)
        .and_then(|mut machine| machine.run(&expr, &mut handlers));

    match result {
        Ok(_) => println!("Game finished!"),
        Err(e) => eprintln!("Effect error: {}", e),
    }
}
