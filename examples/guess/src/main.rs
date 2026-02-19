use codegen::context::VMContext;
use codegen::effect_machine::{CompiledEffectMachine, ConTags};
use codegen::emit::expr::compile_expr;
use codegen::heap_bridge::{heap_to_value, value_to_heap};
use codegen::host_fns;
use codegen::nursery::Nursery;
use codegen::pipeline::CodegenPipeline;
use codegen::yield_type::Yield;
use core_bridge_derive::FromCore;
use core_effect::{DispatchEffect, EffectContext, EffectError, EffectHandler};
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

    let expr = codegen::datacon_env::wrap_with_datacon_env(&expr, &table);
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, &expr, "game").unwrap();
    pipeline.finalize();

    let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
        unsafe { std::mem::transmute(pipeline.get_function_ptr(func_id)) };

    // Set up nursery + VMContext
    let mut nursery = Nursery::new(1 << 20); // 1 MB
    let vmctx = nursery.make_vmctx(host_fns::gc_trigger);

    // Register stack maps for GC (required before calling JIT code)
    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    // Resolve freer-simple constructor tags from DataConTable
    let tags = ConTags::from_table(&table).expect("missing freer-simple constructors in table");

    let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
    let mut handlers = frunk::hlist![ConsoleHandler, RngHandler(rand::thread_rng())];

    // Step/resume loop
    let mut yield_result = machine.step();
    loop {
        match yield_result {
            Yield::Done(_) => {
                println!("Game finished!");
                break;
            }
            Yield::Request {
                tag,
                request,
                continuation,
            } => {
                let req_val = unsafe { heap_to_value(request) }.unwrap();
                let cx = core_effect::EffectContext::with_user(&table, &());
                let resp_val = handlers.dispatch(tag, &req_val, &cx).unwrap();
                let resp_ptr =
                    unsafe { value_to_heap(&resp_val, machine.vmctx_mut()) }.unwrap();
                yield_result = unsafe { machine.resume(continuation, resp_ptr) };
            }
            Yield::Error(e) => {
                eprintln!("Error: {:?}", e);
                break;
            }
        }
    }

    // Cleanup
    host_fns::clear_stack_map_registry();
}
