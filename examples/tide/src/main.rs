use anyhow::{Context, Result};
use clap::Parser;
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_effect::EffectError;
use tidepool_macro::haskell_inline;
use tidepool_tide::handlers::{ConsoleHandler, EnvHandler, FsHandler, NetHandler, ReplHandler};

#[derive(Parser, Debug)]
#[command(author, version, about = "The Tide language interpreter")]
struct Args {
    /// Path to a file of expressions to run
    #[arg(short, long)]
    file: Option<String>,
}

fn main() -> Result<()> {
    // Initialize tracing for observability. Try `RUST_LOG=debug cargo run`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // emit_node uses ~22KB stack per recursive call in debug builds.
    // With ~420 tree depth (302 Core + 117 datacon bindings), that's ~9MB.
    let builder = std::thread::Builder::new()
        .name("tide-runtime".into())
        .stack_size(16 * 1024 * 1024);

    let handler = builder
        .spawn(|| {
            let args = Args::parse();
            run(args)
        })
        .context("Failed to spawn runtime thread")?;

    handler.join().expect("Runtime thread panicked")
}

fn run(args: Args) -> Result<()> {
    // Choose input source: file of expressions, or interactive REPL.
    let repl_handler = match args.file {
        Some(path) => ReplHandler::from_file(&path)?,
        None => ReplHandler::new()?,
    };

    // Compile the Haskell effect stack (haskell/ directory) into a CoreExpr.
    let (expr, table) = haskell_inline! {
        target = "repl",
        include = "haskell",
    };

    // JIT-compile the CoreExpr to native code.
    println!("Compiling with Cranelift...");
    let mut vm =
        JitEffectMachine::compile(&expr, &table, 4 << 20).context("JIT compilation failed")?;

    // Each handler in the HList corresponds to one effect in the Haskell stack:
    //   '[Repl, Console, Env, Net, Fs]
    let mut handlers = frunk::hlist![
        repl_handler,
        ConsoleHandler,
        EnvHandler::new(),
        NetHandler,
        FsHandler::new(std::env::current_dir()?),
    ];

    // Run: the JIT executes Haskell, yielding effect requests back to Rust.
    // Loop to keep the REPL alive on handler errors.
    loop {
        match vm.run(&table, &mut handlers, &()) {
            Ok(_) => break,
            Err(JitError::Effect(EffectError::Handler(e))) => {
                eprintln!("Error: {}", e);
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}
