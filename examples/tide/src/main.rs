use codegen::jit_machine::JitEffectMachine;
use tidepool_macro::haskell_inline;
use tidepool_tide::handlers::{ConsoleHandler, EnvHandler, FsHandler, NetHandler, ReplHandler};

fn main() {
    // Choose input source: file of expressions, or interactive REPL.
    let repl_handler = match std::env::args().nth(1) {
        Some(path) => ReplHandler::from_file(&path),
        None => ReplHandler::new(),
    };

    // Compile the Haskell effect stack (haskell/ directory) into a CoreExpr.
    let (expr, table) = haskell_inline! {
        target = "repl",
        include = "haskell",
    };

    // JIT-compile the CoreExpr to native code.
    let mut vm =
        JitEffectMachine::compile(&expr, &table, 4 << 20).expect("JIT compilation failed");

    // Each handler in the HList corresponds to one effect in the Haskell stack:
    //   '[Repl, Console, Env, Net, Fs]
    let mut handlers = frunk::hlist![
        repl_handler,
        ConsoleHandler,
        EnvHandler::new(),
        NetHandler,
        FsHandler,
    ];

    // Run: the JIT executes Haskell, yielding effect requests back to Rust.
    if let Err(e) = vm.run(&table, &mut handlers, &()) {
        eprintln!("Error: {}", e);
    }
}
