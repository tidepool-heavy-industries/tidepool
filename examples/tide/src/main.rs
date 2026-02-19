use codegen::context::VMContext;
use codegen::effect_machine::{CompiledEffectMachine, ConTags};
use codegen::emit::expr::compile_expr;
use codegen::heap_bridge::{heap_to_value, value_to_heap};
use codegen::host_fns;
use codegen::nursery::Nursery;
use codegen::pipeline::CodegenPipeline;
use codegen::yield_type::Yield;
use core_effect::DispatchEffect;
use tidepool_macro::haskell_inline;
use tidepool_tide::handlers::{
    ConsoleHandler, EnvHandler, FsHandler, NetHandler, ReplHandler,
};

fn main() {
    let repl_handler = match std::env::args().nth(1) {
        Some(path) => ReplHandler::from_file(&path),
        None => ReplHandler::new(),
    };
    let (expr, table) = haskell_inline! {
        target = "repl",
        include = "haskell",
        r#""#
    };

    let expr = codegen::datacon_env::wrap_with_datacon_env(&expr, &table);
    let mut pipeline = CodegenPipeline::new(&host_fns::host_fn_symbols());
    let func_id = compile_expr(&mut pipeline, &expr, "repl").unwrap();
    pipeline.finalize();

    let func_ptr: unsafe extern "C" fn(*mut VMContext) -> *mut u8 =
        unsafe { std::mem::transmute(pipeline.get_function_ptr(func_id)) };

    let mut nursery = Nursery::new(4 << 20); // 4 MB
    let vmctx = nursery.make_vmctx(host_fns::gc_trigger);

    host_fns::set_stack_map_registry(&pipeline.stack_maps);

    let tags = ConTags::from_table(&table).expect("missing freer-simple constructors in table");

    let mut machine = CompiledEffectMachine::new(func_ptr, vmctx, tags);
    let mut handlers = frunk::hlist![
        repl_handler,
        ConsoleHandler,
        EnvHandler::new(),
        NetHandler,
        FsHandler,
    ];

    eprintln!("[tide] compiled OK, stepping...");
    let mut yield_result = machine.step();
    eprintln!("[tide] first step returned");
    loop {
        match yield_result {
            Yield::Done(_) => {
                break;
            }
            Yield::Request {
                tag,
                request,
                continuation,
            } => {
                eprintln!("[tide] effect request: tag={}", tag);
                let req_val = unsafe { heap_to_value(request) }.unwrap();
                eprintln!("[tide] decoded request: {:?}", req_val);
                let resp_val = handlers.dispatch(tag, &req_val, &table).unwrap();
                eprintln!("[tide] handler responded: {:?}", resp_val);
                let resp_ptr =
                    unsafe { value_to_heap(&resp_val, machine.vmctx_mut()) }.unwrap();
                eprintln!("[tide] resuming...");
                yield_result = unsafe { machine.resume(continuation, resp_ptr) };
                eprintln!("[tide] resumed OK");
            }
            Yield::Error(e) => {
                eprintln!("Error: {:?}", e);
                break;
            }
        }
    }

    host_fns::clear_stack_map_registry();
}
