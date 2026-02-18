use core_bridge_derive::FromCore;
use core_effect::{EffectError, EffectHandler, EffectMachine};
use core_eval::value::Value;
use core_repr::{CoreFrame, DataConTable, Literal};
use rand::Rng;
use tidepool_macro::haskell_expr;

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Emit")]
    Emit(String),
    #[core(name = "AwaitInt")]
    AwaitInt,
}

struct ConsoleHandler;

impl ConsoleHandler {
    /// Construct a Haskell () value: Con("()", [])
    fn make_unit(table: &DataConTable) -> Value {
        match table.get_by_name("()") {
            Some(id) => Value::Con(id, vec![]),
            // Fallback: GHC optimizes `case x of () -> body` away for ()
            // so the continuation won't inspect this value
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
                Ok(Value::Lit(Literal::LitInt(n)))
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

    fn handle(&mut self, req: RngReq, _table: &DataConTable) -> Result<Value, EffectError> {
        match req {
            RngReq::RandInt(lo, hi) => {
                let n: i64 = self.0.gen_range(lo..=hi);
                Ok(Value::Lit(Literal::LitInt(n)))
            }
        }
    }
}

fn main() {
    let (expr, table) = haskell_expr!("Guess.hs::game");

    // Debug: dump DataConTable
    eprintln!("=== DataConTable ===");
    for dc in table.iter() {
        eprintln!("  {:?} -> {} (tag={}, arity={})", dc.id, dc.name, dc.tag, dc.rep_arity);
    }

    // Debug: show tree structure
    eprintln!("=== Tree: {} nodes, root = {} ===", expr.nodes.len(), expr.nodes.len() - 1);
    // Show root node
    eprintln!("Root node: {:?}", &expr.nodes[expr.nodes.len() - 1]);
    // Pretty-print (may be large)
    let pp = core_repr::pretty::pretty_print(&expr);
    eprintln!("=== Pretty (first 2000 chars) ===");
    eprintln!("{}", &pp[..pp.len().min(2000)]);

    // Find ALL bindings and their names by looking at what the harness emitted
    eprintln!("\n=== All LetNonRec binders ===");
    for (i, node) in expr.nodes.iter().enumerate() {
        if let CoreFrame::LetNonRec { binder, rhs, .. } = node {
            let rhs_summary = match &expr.nodes[*rhs] {
                CoreFrame::Con { tag, fields } => format!("Con({:?}, {} fields)", tag, fields.len()),
                CoreFrame::Lam { .. } => "Lam(...)".to_string(),
                CoreFrame::Lit(l) => format!("Lit({:?})", l),
                CoreFrame::Var(v) => format!("Var(v_{})", v.0),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            eprintln!("  [{}] v_{} = {}", i, binder.0, rhs_summary);
        }
    }

    // Find all Var nodes referencing data constructors (not bound in tree)
    let datacon_ids: std::collections::HashSet<u64> = table.iter().map(|dc| dc.id.0).collect();
    eprintln!("\n=== Var nodes referencing DataCon IDs (potential unbound) ===");
    for (i, node) in expr.nodes.iter().enumerate() {
        if let CoreFrame::Var(v) = node {
            if datacon_ids.contains(&v.0) {
                let name = table.iter().find(|dc| dc.id.0 == v.0).map(|dc| dc.name.as_str()).unwrap_or("?");
                eprintln!("  [{}] Var(v_{}) = DataCon '{}'", i, v.0, name);
            }
        }
    }

    // Find which binder matches the target VarId
    let target_var = 8286623314361716746u64;
    eprintln!("\n=== Looking for binder v_{} ===", target_var);
    for (i, node) in expr.nodes.iter().enumerate() {
        match node {
            CoreFrame::LetNonRec { binder, rhs, .. } if binder.0 == target_var => {
                eprintln!("  FOUND at [{}] LetNonRec binder=v_{} rhs=[{}]", i, binder.0, rhs);
                eprintln!("    RHS node: {:?}", &expr.nodes[*rhs]);
            }
            CoreFrame::LetRec { bindings, .. } => {
                for (b, rhs) in bindings {
                    if b.0 == target_var {
                        eprintln!("  FOUND in LetRec at [{}] binder=v_{} rhs=[{}]", i, b.0, rhs);
                        eprintln!("    RHS node: {:?}", &expr.nodes[*rhs]);
                    }
                }
            }
            _ => {}
        }
    }

    // Trace the body chain from root to find the innermost expression
    eprintln!("\n=== Body chain from root ===");
    let mut idx = expr.nodes.len() - 1;
    let mut depth = 0;
    loop {
        match &expr.nodes[idx] {
            CoreFrame::LetNonRec { body, binder, .. } => {
                if depth < 5 || depth > 40 {
                    eprintln!("  [{}] LetNonRec binder=v_{} body->{}", idx, binder.0, body);
                } else if depth == 5 {
                    eprintln!("  ... (skipping middle) ...");
                }
                idx = *body;
                depth += 1;
            }
            CoreFrame::LetRec { body, .. } => {
                eprintln!("  [{}] LetRec body->{}", idx, body);
                idx = *body;
                depth += 1;
            }
            other => {
                eprintln!("  [{}] LEAF: {:?}", idx, other);
                break;
            }
        }
    }

    // Debug: eval the expression and see what we get
    let mut heap = core_eval::heap::VecHeap::new();
    let env = core_eval::eval::env_from_datacon_table(&table);
    match core_eval::eval::eval(&expr, &env, &mut heap) {
        Ok(val) => eprintln!("=== eval result ===\n  {:?}", val),
        Err(e) => {
            eprintln!("=== eval error ===\n  {:?}", e);
            return;
        }
    }

    // Now try the EffectMachine
    let mut heap2 = core_eval::heap::VecHeap::new();
    let mut handlers = frunk::hlist![ConsoleHandler, RngHandler(rand::thread_rng())];

    let result = EffectMachine::new(&table, &mut heap2)
        .and_then(|mut machine| machine.run(&expr, &mut handlers));

    match result {
        Ok(_) => println!("Game finished!"),
        Err(e) => eprintln!("Effect error: {}", e),
    }
}
