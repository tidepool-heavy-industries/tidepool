//! W4 lazy-consumption property suite — the bug MAP for lazy effect-result
//! materialization (home turf of OPEN #313: map/filter over `lines` of a
//! partially-consumed effect tuple → "undefined forced").
//!
//! ## Design
//!
//! The oracle for every case is a *total, exact* Rust reference answer. We
//! then compile+run the SAME Haskell program TWICE — once with
//! `TIDEPOOL_LAZY_RESULTS=1` (park/stream), once with `=0` (eager drain) —
//! and require lazy-ON == lazy-OFF == reference. Any disagreement is a bug
//! (B1 mismatch / B2 runtime error where reference succeeds / B3 fatal
//! signal / B4 lazy-vs-eager-or-nursery divergence).
//!
//! ## Why subprocesses
//!
//! `TIDEPOOL_LAZY_RESULTS` is read process-globally inside `jit_machine`, and
//! the parked-stream registry is a thread-local. Setting the env var in-process
//! is racy and forbidden. So each case runs in a fresh `#[ignore]`d worker test
//! (`worker_run_one`) re-spawned via `current_exe()`, with the env var set on
//! the `Command`. The driver classifies the worker's exit (signal vs clean),
//! captures stdout (the JSON result) and stderr (breadcrumbs: "undefined" /
//! "trap" / "SIGILL").
//!
//! ## Templates, not free-form Haskell
//!
//! A FIXED set of parameterized templates (keyed by producer shape × consumer
//! shape × a small fixed `k` splice set) keeps the GHC-extract disk cache hot:
//! after one warm-up each case is run-only. Sizes flow through *handler data*
//! (the dispatcher), never source — so threshold-straddling costs no compiles.
//!
//! See `plans/proptest-findings-lazy.md` for the bug table and #313 status.

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run_with_nursery_size;

/// Matches `tidepool_runtime::DEFAULT_NURSERY_SIZE` (private): 64 MiB.
const DEFAULT_NURSERY_SIZE: usize = 1 << 26;

/// Tiny nursery for the GC-stress property. 512 KiB runs the baseline machine
/// plus a couple of stream chunks for moderate `n` (≤512), forcing the
/// GC-and-retry path (`host_alloc_gc` → `gc_trigger`) to fire mid-chunk —
/// while staying above the floor where the baseline itself OOMs (which would
/// be noise, not a lazy bug). Larger `n` under tiny nursery exhausts in BOTH
/// modes because spine-retaining consumers (`length`) hold the whole list
/// live; the phase-3 size schedule keeps `n` small to avoid that.
const TINY_NURSERY_SIZE: usize = 512 * 1024;

// ===========================================================================
// The matrix: producer shapes × consumer shapes.
// ===========================================================================

/// How the handler delivers a list-shaped `[Text]` response. Pure handler-data
/// variation — all three present the SAME Haskell type, so they share templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    /// `cx.respond(Vec<String>)` — a fully materialized `Value` spine. Over the
    /// 2000-node lazy-spine threshold it is dismantled + re-parked.
    Complete,
    /// `cx.respond_stream(iter)` — parked iterator, converts per chunk (256).
    Stream,
    /// `cx.respond_list(Vec)` — indexed source, per-ELEMENT head thunks.
    IndexedList,
}

impl Method {
    fn tag(self) -> &'static str {
        match self {
            Method::Complete => "complete",
            Method::Stream => "stream",
            Method::IndexedList => "list",
        }
    }
    fn parse(s: &str) -> Method {
        match s {
            "complete" => Method::Complete,
            "stream" => Method::Stream,
            "list" => Method::IndexedList,
            other => panic!("bad method {other}"),
        }
    }
}

/// What the handler produces and the static Haskell type it presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Producer {
    /// `xs :: [Text]` via `glob "**"`, delivered by `method`.
    List(Method),
    /// `s :: Text` of `n` newline-joined lines via `readFile`.
    StringLines,
    /// `(c,o,e) :: (Int, Text, Text)` via `run` — stderr has `n` lines. #313.
    TupleStringList,
    /// Two `[Text]` effects via two `glob` calls, both delivered by `method`.
    TwoList(Method),
}

impl Producer {
    /// Canonical kind label for the coverage counter (size/method-agnostic for
    /// the priority cells, but method matters for list shapes).
    fn kind(self) -> String {
        match self {
            Producer::List(m) => format!("List/{}", m.tag()),
            Producer::StringLines => "StringLines".into(),
            Producer::TupleStringList => "TupleStringList".into(),
            Producer::TwoList(m) => format!("TwoList/{}", m.tag()),
        }
    }
    fn encode(self) -> String {
        match self {
            Producer::List(m) => format!("List:{}", m.tag()),
            Producer::StringLines => "StringLines".into(),
            Producer::TupleStringList => "TupleStringList".into(),
            Producer::TwoList(m) => format!("TwoList:{}", m.tag()),
        }
    }
    fn parse(s: &str) -> Producer {
        if let Some(m) = s.strip_prefix("List:") {
            Producer::List(Method::parse(m))
        } else if let Some(m) = s.strip_prefix("TwoList:") {
            Producer::TwoList(Method::parse(m))
        } else if s == "StringLines" {
            Producer::StringLines
        } else if s == "TupleStringList" {
            Producer::TupleStringList
        } else {
            panic!("bad producer {s}")
        }
    }
}

/// How the Haskell program consumes the produced value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Consumer {
    /// `length xs` — force the full spine, return an Int.
    Full,
    /// `take k xs` — prefix only.
    Prefix(usize),
    /// `take k xs`, THEN a second effect, return `len(take k) + len(ys)`.
    /// Exercises abandoning a parked stream and re-entering the registry.
    PrefixThenEffect(usize),
    /// `take k (filter ("item-1" `isPrefixOf`) xs)` — the #313 list shape.
    MapFilterPrefix(usize),
    /// `take k (lines field)` — #313's exact shape over Text/tuple.
    LinesOfPartial(usize),
    /// `take k (zip xs ys)` — two live parked sources advancing in lockstep.
    ZipInterleave(usize),
    /// `sum (map len (take k xs)) * 2` — force the same prefix twice.
    ForceTwice(usize),
    /// `case xs of [] -> 0; _ -> 1` — only the head is forced; laziness must
    /// not leak into the unforced branch.
    BranchOnly,
    // --- #313 bisection consumers (TupleStringList only) -------------------
    /// `filter (len>1) (lines e) <> [pack (show c), o]` — the EXACT inline
    /// equivalent of Probe.hs `t7` (binds all three tuple fields).
    TupleAllFields,
    /// `lines e <> [o]` — append the stdout field to the lines list (uses the
    /// 2nd tuple field, no `show`).
    TupleAppendStdout,
    /// `[pack (show c)]` — just the exit-code field, `show`n. (uses the 1st
    /// field; no lines, no list append).
    TupleShowCode,
}

impl Consumer {
    fn kind(self) -> &'static str {
        match self {
            Consumer::Full => "Full",
            Consumer::Prefix(_) => "Prefix",
            Consumer::PrefixThenEffect(_) => "PrefixThenEffect",
            Consumer::MapFilterPrefix(_) => "MapFilterPrefix",
            Consumer::LinesOfPartial(_) => "LinesOfPartial",
            Consumer::ZipInterleave(_) => "ZipInterleave",
            Consumer::ForceTwice(_) => "ForceTwice",
            Consumer::BranchOnly => "BranchOnly",
            Consumer::TupleAllFields => "TupleAllFields",
            Consumer::TupleAppendStdout => "TupleAppendStdout",
            Consumer::TupleShowCode => "TupleShowCode",
        }
    }
    fn k(self) -> Option<usize> {
        match self {
            Consumer::Full
            | Consumer::BranchOnly
            | Consumer::TupleAllFields
            | Consumer::TupleAppendStdout
            | Consumer::TupleShowCode => None,
            Consumer::Prefix(k)
            | Consumer::PrefixThenEffect(k)
            | Consumer::MapFilterPrefix(k)
            | Consumer::LinesOfPartial(k)
            | Consumer::ZipInterleave(k)
            | Consumer::ForceTwice(k) => Some(k),
        }
    }
    fn encode(self) -> String {
        match self.k() {
            Some(k) => format!("{}:{k}", self.kind()),
            None => self.kind().to_string(),
        }
    }
    fn parse(s: &str) -> Consumer {
        let (name, k) = match s.split_once(':') {
            Some((n, k)) => (n, Some(k.parse::<usize>().expect("k"))),
            None => (s, None),
        };
        match (name, k) {
            ("Full", None) => Consumer::Full,
            ("BranchOnly", None) => Consumer::BranchOnly,
            ("TupleAllFields", None) => Consumer::TupleAllFields,
            ("TupleAppendStdout", None) => Consumer::TupleAppendStdout,
            ("TupleShowCode", None) => Consumer::TupleShowCode,
            ("Prefix", Some(k)) => Consumer::Prefix(k),
            ("PrefixThenEffect", Some(k)) => Consumer::PrefixThenEffect(k),
            ("MapFilterPrefix", Some(k)) => Consumer::MapFilterPrefix(k),
            ("LinesOfPartial", Some(k)) => Consumer::LinesOfPartial(k),
            ("ZipInterleave", Some(k)) => Consumer::ZipInterleave(k),
            ("ForceTwice", Some(k)) => Consumer::ForceTwice(k),
            other => panic!("bad consumer {other:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Case {
    producer: Producer,
    consumer: Consumer,
    /// Element count produced (flows through handler data — no recompile).
    n: usize,
    /// Tiny nursery to provoke GC mid-materialization.
    tiny_nursery: bool,
}

impl Case {
    /// Coverage-cell label: producer-kind × consumer-kind (size/k-agnostic).
    fn cell(&self) -> String {
        format!("{} × {}", self.producer.kind(), self.consumer.kind())
    }
    fn encode(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.producer.encode(),
            self.consumer.encode(),
            self.n,
            if self.tiny_nursery { "tiny" } else { "default" }
        )
    }
    fn parse(s: &str) -> Case {
        let mut it = s.split('|');
        let producer = Producer::parse(it.next().unwrap());
        let consumer = Consumer::parse(it.next().unwrap());
        let n = it.next().unwrap().parse().unwrap();
        let tiny_nursery = it.next().unwrap() == "tiny";
        Case {
            producer,
            consumer,
            n,
            tiny_nursery,
        }
    }
}

// ===========================================================================
// Reference evaluator (~the oracle). Total and exact.
// ===========================================================================

/// The conceptual produced item at index `i`: `"item-{i}"`.
fn item(i: usize) -> String {
    format!("item-{i}")
}

/// Does `"item-{i}"` start with the literal `"item-1"`? (decimal index begins
/// with `1`, OR i==1's degenerate `item-1` itself — same test).
fn matches_item1(i: usize) -> bool {
    item(i).starts_with("item-1")
}

/// Compute the exact reference JSON answer for a case, purely in Rust.
fn reference(case: &Case) -> serde_json::Value {
    use serde_json::json;
    let n = case.n;
    // The conceptual list the consumer sees (identical for list/lines/tuple
    // shapes — only delivery differs). TwoList templates touch TWO copies, so
    // Full / Prefix differ there (noted inline).
    let at = |i: usize| -> String { item(i) };
    let two = matches!(case.producer, Producer::TwoList(_));
    match case.consumer {
        // TwoList Full = length xs + length ys = 2n; others = n.
        Consumer::Full => json!(if two { 2 * n } else { n }),
        Consumer::BranchOnly => json!(if n > 0 { 1 } else { 0 }),
        // TwoList Prefix = take k xs ++ take k ys (two prefixes concatenated);
        // single-source Prefix/LinesOfPartial = one prefix.
        Consumer::Prefix(k) | Consumer::LinesOfPartial(k) => {
            let m = k.min(n);
            let mut v: Vec<String> = (0..m).map(at).collect();
            if two {
                v.extend((0..m).map(at));
            }
            json!(v)
        }
        Consumer::PrefixThenEffect(k) => json!(k.min(n) + n),
        Consumer::MapFilterPrefix(k) => {
            let v: Vec<String> = (0..n).filter(|&i| matches_item1(i)).map(at).take(k).collect();
            json!(v)
        }
        Consumer::ZipInterleave(k) => {
            let m = k.min(n);
            json!((0..m).map(|i| vec![at(i), at(i)]).collect::<Vec<_>>())
        }
        Consumer::ForceTwice(k) => {
            let m = k.min(n);
            let s: usize = (0..m).map(|i| item(i).chars().count()).sum();
            json!(s * 2)
        }
        // #313 bisection: dispatcher yields (c=0, o="stdout", e=items joined).
        Consumer::TupleAllFields => {
            // filter (len>1) (lines e) = all items (each "item-N" has len>=6),
            // then <> ["0", "stdout"].
            let mut v: Vec<String> = (0..n).map(at).collect();
            v.push("0".to_string());
            v.push("stdout".to_string());
            json!(v)
        }
        Consumer::TupleAppendStdout => {
            let mut v: Vec<String> = (0..n).map(at).collect();
            v.push("stdout".to_string());
            json!(v)
        }
        Consumer::TupleShowCode => json!(["0"]),
    }
}

// ===========================================================================
// Haskell template generation (the FIXED template set).
// ===========================================================================

/// The verb the template invokes, picked so its static return type matches the
/// dispatcher's produced shape. The dispatcher ignores the effect tag.
fn result_type_and_body(case: &Case) -> (&'static str, String) {
    let k = case.consumer.k().unwrap_or(0);
    match (case.producer, case.consumer) {
        // ---- list-shaped producers: xs :: [Text] via `glob` -----------------
        (Producer::List(_), Consumer::Full) => {
            ("Int", "xs <- glob \"**\"\npure (length xs)".into())
        }
        (Producer::List(_), Consumer::BranchOnly) => (
            "Int",
            "xs <- glob \"**\"\npure (case xs of { [] -> 0; _ -> 1 })".into(),
        ),
        (Producer::List(_), Consumer::Prefix(_)) => {
            ("[Text]", format!("xs <- glob \"**\"\npure (take {k} xs)"))
        }
        (Producer::List(_), Consumer::PrefixThenEffect(_)) => (
            "Int",
            format!(
                "xs <- glob \"**\"\nlet p = take {k} xs\nys <- glob \"**\"\npure (length p + length ys)"
            ),
        ),
        (Producer::List(_), Consumer::MapFilterPrefix(_)) => (
            "[Text]",
            format!(
                "xs <- glob \"**\"\npure (take {k} (filter (\\x -> \"item-1\" `isPrefixOf` x) xs))"
            ),
        ),
        (Producer::List(_), Consumer::ForceTwice(_)) => (
            "Int",
            format!(
                "xs <- glob \"**\"\nlet p = take {k} xs\npure (sum (map len p) + sum (map len p))"
            ),
        ),
        // ---- StringLines: s :: Text via `readFile`, then `lines` -------------
        (Producer::StringLines, Consumer::Full) => (
            "Int",
            "s <- readFile \"x\"\npure (length (lines s))".into(),
        ),
        (Producer::StringLines, Consumer::BranchOnly) => (
            "Int",
            "s <- readFile \"x\"\npure (case lines s of { [] -> 0; _ -> 1 })".into(),
        ),
        (Producer::StringLines, Consumer::LinesOfPartial(_)) => (
            "[Text]",
            format!("s <- readFile \"x\"\npure (take {k} (lines s))"),
        ),
        // ---- TupleStringList: (c,o,e) via `run`, then `lines e` — #313 -------
        (Producer::TupleStringList, Consumer::Full) => (
            "Int",
            "(_, _, e) <- run \"x\"\npure (length (lines e))".into(),
        ),
        (Producer::TupleStringList, Consumer::LinesOfPartial(_)) => (
            "[Text]",
            format!("(_, _, e) <- run \"x\"\npure (take {k} (lines e))"),
        ),
        (Producer::TupleStringList, Consumer::MapFilterPrefix(_)) => (
            "[Text]",
            format!(
                "(_, _, e) <- run \"x\"\npure (take {k} (filter (\\l -> \"item-1\" `isPrefixOf` l) (lines e)))"
            ),
        ),
        // #313 bisection: t7 inline + the field-isolating sub-cases.
        (Producer::TupleStringList, Consumer::TupleAllFields) => (
            "[Text]",
            "(c, o, e) <- run \"x\"\npure (filter (\\l -> len l > 1) (lines e) <> [pack (show c), o])".into(),
        ),
        (Producer::TupleStringList, Consumer::TupleAppendStdout) => (
            "[Text]",
            "(_, o, e) <- run \"x\"\npure (lines e <> [o])".into(),
        ),
        (Producer::TupleStringList, Consumer::TupleShowCode) => (
            "[Text]",
            "(c, _, _) <- run \"x\"\npure [pack (show c)]".into(),
        ),
        // ---- TwoList: xs, ys :: [Text] via two `glob`s -----------------------
        (Producer::TwoList(_), Consumer::Full) => (
            "Int",
            "xs <- glob \"**\"\nys <- glob \"**\"\npure (length xs + length ys)".into(),
        ),
        (Producer::TwoList(_), Consumer::ZipInterleave(_)) => (
            "[(Text, Text)]",
            format!("xs <- glob \"**\"\nys <- glob \"**\"\npure (take {k} (zip xs ys))"),
        ),
        (Producer::TwoList(_), Consumer::Prefix(_)) => (
            "[Text]",
            format!("xs <- glob \"**\"\nys <- glob \"**\"\npure (take {k} xs ++ take {k} ys)"),
        ),
        (p, c) => panic!("invalid producer×consumer pairing: {p:?} × {c:?}"),
    }
}

/// Assemble the full module: real MCP preamble + real effect stack + a custom
/// `result` of the consumer's type (manual assembly, like lazy_bisect — so the
/// raw bridge `.to_json()` is the answer, no paginator truncation).
fn build_source(case: &Case) -> String {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let (ty, body) = result_type_and_body(case);
    let mut source = preamble;
    source.push_str("-- [user]\n");
    source.push_str(&format!("result :: Eff {stack} {ty}\n"));
    source.push_str("result = do\n");
    for line in body.lines() {
        source.push_str("  ");
        source.push_str(line);
        source.push('\n');
    }
    source
}

// ===========================================================================
// The worker dispatcher: produces the case-configured shape for EVERY effect.
// ===========================================================================

struct WorkerDispatcher {
    producer: Producer,
    n: usize,
}

impl DispatchEffect<()> for WorkerDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let n = self.n;
        match self.producer {
            Producer::List(m) | Producer::TwoList(m) => match m {
                Method::Complete => {
                    let items: Vec<String> = (0..n).map(item).collect();
                    cx.respond(items)
                }
                Method::Stream => cx.respond_stream((0..n).map(item)),
                Method::IndexedList => {
                    let items: Vec<String> = (0..n).map(item).collect();
                    cx.respond_list(items)
                }
            },
            Producer::StringLines => {
                let s: String = (0..n).map(item).collect::<Vec<_>>().join("\n");
                cx.respond(s)
            }
            Producer::TupleStringList => {
                let stderr: String = (0..n).map(item).collect::<Vec<_>>().join("\n");
                cx.respond((0i64, "stdout".to_string(), stderr))
            }
        }
    }
}

// ===========================================================================
// Include dirs (mirrors lazy_bisect / repro313).
// ===========================================================================

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}
fn prelude_dir() -> &'static Path {
    root().join("haskell/lib").leak()
}
fn user_lib_dir() -> &'static Path {
    root().join(".tidepool/lib").leak()
}

/// Run ONE case in-process (called by the worker). Reads no env beyond what the
/// parent set on the `Command`; never sets `TIDEPOOL_LAZY_RESULTS` itself.
fn run_case_inproc(case: &Case) -> Result<serde_json::Value, String> {
    let source = build_source(case);
    let decls = tidepool_mcp::standard_decls();
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let include = [prelude_dir(), user_lib_dir(), effects_dir];
    let nursery = if case.tiny_nursery {
        TINY_NURSERY_SIZE
    } else {
        DEFAULT_NURSERY_SIZE
    };
    let mut dispatcher = WorkerDispatcher {
        producer: case.producer,
        n: case.n,
    };
    compile_and_run_with_nursery_size(&source, "result", &include, &mut dispatcher, &(), nursery)
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

// ===========================================================================
// Worker test: spawned per (case, lazy-flag) via current_exe().
// ===========================================================================

#[test]
#[ignore = "subprocess worker — driven by the property tests via current_exe()"]
fn worker_run_one() {
    let serialized = std::env::var("TIDEPOOL_FUZZ_CASE").expect("TIDEPOOL_FUZZ_CASE unset");
    let case = Case::parse(&serialized);
    match run_case_inproc(&case) {
        Ok(v) => {
            // STDOUT marker + compact JSON, parsed by the driver.
            println!("WORKER_RESULT_OK {}", serde_json::to_string(&v).unwrap());
        }
        Err(e) => {
            println!("WORKER_RESULT_ERR {e}");
        }
    }
}

/// Outcome of one worker subprocess.
#[derive(Debug)]
enum WorkerOutcome {
    Ok(serde_json::Value),
    /// Clean runtime/compile error (Rust `Err`, no fatal signal).
    Err(String),
    /// Fatal signal (SIGSEGV/SIGILL/etc.) — B3.
    Signal(i32, String),
    /// Worker exited nonzero without a result marker and without a signal.
    Unknown(String),
}

/// Spawn a worker test by exact name with the given environment, parse its
/// `WORKER_RESULT_*` marker / fatal signal.
fn spawn_named(test: &str, envs: &[(&str, String)]) -> WorkerOutcome {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test, "--ignored", "--nocapture"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn worker");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if let Some(sig) = out.status.signal() {
        return WorkerOutcome::Signal(sig, stderr);
    }
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("WORKER_RESULT_OK ") {
            let v: serde_json::Value = serde_json::from_str(rest).expect("worker json");
            return WorkerOutcome::Ok(v);
        }
        if let Some(rest) = line.strip_prefix("WORKER_RESULT_ERR ") {
            return WorkerOutcome::Err(rest.to_string());
        }
    }
    WorkerOutcome::Unknown(format!(
        "no result marker; status={:?}\nstderr:\n{stderr}",
        out.status.code()
    ))
}

fn spawn_worker(case: &Case, lazy: bool) -> WorkerOutcome {
    spawn_named(
        "worker_run_one",
        &[
            ("TIDEPOOL_FUZZ_CASE", case.encode()),
            (
                "TIDEPOOL_LAZY_RESULTS",
                if lazy { "1" } else { "0" }.to_string(),
            ),
        ],
    )
}

// --- lib-module consumer probe (the #313 trigger path) ---------------------
//
// #313's repro consumes the partially-bound effect result through a function
// DEFINED IN A `.tidepool/lib` MODULE (Probe.hs), not inline Prelude. This
// worker calls such a function so we can A/B it against the inline templates.

/// Assemble a module that calls an existing Probe.hs lib function `fn_name`
/// (in scope via the `Library` re-export) and returns its result.
fn build_lib_source(fn_name: &str, ty: &str) -> String {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    // `import Probe` must land after the standard imports and before the
    // `default` decl — same insertion point template_haskell uses (repro313
    // passes "Probe" as its import arg to bring t1..t11 into scope).
    let insert = preamble.find("default (Int").unwrap_or(preamble.len());
    let mut source = String::new();
    source.push_str(&preamble[..insert]);
    source.push_str("import Probe\n");
    source.push_str(&preamble[insert..]);
    source.push_str("-- [user]\n");
    source.push_str(&format!("result :: Eff {stack} {ty}\n"));
    source.push_str("result = do\n");
    source.push_str(&format!("  r <- {fn_name}\n"));
    source.push_str("  pure r\n");
    source
}

#[test]
#[ignore = "subprocess worker — driven by repro_313_boundary"]
fn worker_lib_probe() {
    let fn_name = std::env::var("TIDEPOOL_LIB_FN").expect("TIDEPOOL_LIB_FN");
    let ty = std::env::var("TIDEPOOL_LIB_TY").expect("TIDEPOOL_LIB_TY");
    let producer = Producer::parse(&std::env::var("TIDEPOOL_LIB_PRODUCER").expect("producer"));
    let n: usize = std::env::var("TIDEPOOL_LIB_N").expect("n").parse().unwrap();

    let source = build_lib_source(&fn_name, &ty);
    let decls = tidepool_mcp::standard_decls();
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let include = [prelude_dir(), user_lib_dir(), effects_dir];
    let mut dispatcher = WorkerDispatcher { producer, n };
    let r = compile_and_run_with_nursery_size(
        &source,
        "result",
        &include,
        &mut dispatcher,
        &(),
        DEFAULT_NURSERY_SIZE,
    )
    .map(|v| v.to_json())
    .map_err(|e| format!("{e}"));
    match r {
        Ok(v) => println!("WORKER_RESULT_OK {}", serde_json::to_string(&v).unwrap()),
        Err(e) => println!("WORKER_RESULT_ERR {e}"),
    }
}

/// Run one lib-function probe (lazy on or off). Returns the outcome.
fn spawn_lib_probe(fn_name: &str, ty: &str, producer: Producer, n: usize, lazy: bool) -> WorkerOutcome {
    spawn_named(
        "worker_lib_probe",
        &[
            ("TIDEPOOL_LIB_FN", fn_name.to_string()),
            ("TIDEPOOL_LIB_TY", ty.to_string()),
            ("TIDEPOOL_LIB_PRODUCER", producer.encode()),
            ("TIDEPOOL_LIB_N", n.to_string()),
            (
                "TIDEPOOL_LAZY_RESULTS",
                if lazy { "1" } else { "0" }.to_string(),
            ),
        ],
    )
}

// ===========================================================================
// Bug collection + coverage counters.
// ===========================================================================

#[derive(Debug, Clone)]
struct Bug {
    class: &'static str, // B1..B4
    cell: String,
    case: String,
    detail: String,
}

static BUGS: Mutex<Vec<Bug>> = Mutex::new(Vec::new());
static COVERAGE: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());

fn record_cell(cell: &str) {
    *COVERAGE.lock().unwrap().entry(cell.to_string()).or_insert(0) += 1;
}

fn record_bug(class: &'static str, case: &Case, detail: String) {
    let bug = Bug {
        class,
        cell: case.cell(),
        case: case.encode(),
        detail,
    };
    eprintln!(
        "\n*** BUG {} [{}] case={}\n    {}\n",
        bug.class, bug.cell, bug.case, bug.detail
    );
    BUGS.lock().unwrap().push(bug);
}

/// Run one case through the oracle: reference vs lazy-ON vs lazy-OFF. Records
/// any divergence as a bug (non-fatal — the suite stays green and reports the
/// full map at the end). Returns true if all three agreed.
fn check_case(case: &Case) -> bool {
    record_cell(&case.cell());
    let want = reference(case);
    let lazy_on = spawn_worker(case, true);
    let lazy_off = spawn_worker(case, false);

    let mut ok = true;

    // B3: fatal signals first (most severe).
    if let WorkerOutcome::Signal(sig, ref bc) = lazy_on {
        record_bug("B3", case, format!("lazy-ON fatal signal {sig}\n{}", tail(bc)));
        ok = false;
    }
    if let WorkerOutcome::Signal(sig, ref bc) = lazy_off {
        record_bug("B3", case, format!("lazy-OFF fatal signal {sig}\n{}", tail(bc)));
        ok = false;
    }

    // Compare each mode against the reference.
    let lon = classify(case, &lazy_on, &want, "lazy-ON", &mut ok);
    let loff = classify(case, &lazy_off, &want, "lazy-OFF", &mut ok);

    // B4: lazy-ON vs lazy-OFF divergence (even if both differ from ref in the
    // same way, that's already B1/B2; this catches them differing from EACH
    // OTHER).
    if let (Some(a), Some(b)) = (lon, loff) {
        if a != b {
            record_bug(
                "B4",
                case,
                format!("lazy-ON ({a}) != lazy-OFF ({b})"),
            );
            ok = false;
        }
    }
    ok
}

/// Compare one mode's outcome to the reference; returns a normalized result
/// string for the B4 cross-check (None if signal/unknown already recorded).
fn classify(
    case: &Case,
    outcome: &WorkerOutcome,
    want: &serde_json::Value,
    label: &str,
    ok: &mut bool,
) -> Option<String> {
    match outcome {
        WorkerOutcome::Ok(got) => {
            if got != want {
                record_bug(
                    "B1",
                    case,
                    format!("{label} mismatch: got {got}, want {want}"),
                );
                *ok = false;
            }
            Some(format!("ok:{got}"))
        }
        WorkerOutcome::Err(e) => {
            record_bug(
                "B2",
                case,
                format!("{label} runtime error where reference={want}: {}", tail(e)),
            );
            *ok = false;
            Some(format!("err:{e}"))
        }
        WorkerOutcome::Signal(..) => None, // already recorded as B3
        WorkerOutcome::Unknown(u) => {
            record_bug("B2", case, format!("{label} unknown exit: {}", tail(u)));
            *ok = false;
            None
        }
    }
}

fn tail(s: &str) -> String {
    let breadcrumbs: Vec<&str> = s
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("undefined")
                || l.contains("trap")
                || l.contains("sigill")
                || l.contains("sigsegv")
                || l.contains("panic")
                || l.contains("[bug]")
        })
        .collect();
    let mut out = String::new();
    if !breadcrumbs.is_empty() {
        out.push_str("breadcrumbs: ");
        out.push_str(&breadcrumbs.join(" | "));
        out.push('\n');
    }
    let last: Vec<&str> = s.lines().rev().take(4).collect();
    out.push_str("…");
    out.push_str(&last.into_iter().rev().collect::<Vec<_>>().join("\n"));
    out
}

// ===========================================================================
// Cell enumeration (deterministic coverage spine).
// ===========================================================================

/// The fixed `k` splice set: 1 and the chunk-boundary fenceposts 255/256/257.
/// Keeping `k` in this small set bounds the distinct compiled templates so the
/// disk cache stays hot. (Used across the phase generators below.)
#[allow(dead_code)]
const KS: [usize; 4] = [1, 255, 256, 257];

/// Consumers valid over a list-shaped producer (`[Text]`).
fn list_consumers(k: usize) -> Vec<Consumer> {
    vec![
        Consumer::Full,
        Consumer::BranchOnly,
        Consumer::Prefix(k),
        Consumer::PrefixThenEffect(k),
        Consumer::MapFilterPrefix(k),
        Consumer::ForceTwice(k),
    ]
}

/// Every valid (producer, consumer) pairing with `k` applied to k-bearing
/// consumers. This is THE matrix: its length is the number of coverage cells.
fn all_pairs(k: usize) -> Vec<(Producer, Consumer)> {
    let mut v = Vec::new();
    for m in [Method::Complete, Method::Stream, Method::IndexedList] {
        let p = Producer::List(m);
        for c in list_consumers(k) {
            v.push((p, c));
        }
    }
    for c in [
        Consumer::Full,
        Consumer::BranchOnly,
        Consumer::LinesOfPartial(k),
    ] {
        v.push((Producer::StringLines, c));
    }
    for c in [
        Consumer::Full,
        Consumer::LinesOfPartial(k),
        Consumer::MapFilterPrefix(k),
    ] {
        v.push((Producer::TupleStringList, c));
    }
    for m in [Method::Complete, Method::Stream, Method::IndexedList] {
        let p = Producer::TwoList(m);
        for c in [Consumer::Full, Consumer::ZipInterleave(k), Consumer::Prefix(k)] {
            v.push((p, c));
        }
    }
    v
}

/// The sorted, de-duplicated list of coverage-cell labels.
fn coverage_targets() -> Vec<String> {
    let mut cells: Vec<String> = all_pairs(1)
        .into_iter()
        .map(|(p, c)| format!("{} × {}", p.kind(), c.kind()))
        .collect();
    cells.sort();
    cells.dedup();
    cells
}

/// Threshold-straddling sizes per producer, indexed by `variant` (0..3). The
/// sizes are chosen to bracket: the 2000-node Complete spine threshold, the
/// 256-element stream chunk boundary, and small/empty edges.
fn size_for(p: Producer, variant: usize) -> usize {
    let schedule: [usize; 4] = match p {
        Producer::List(Method::Complete) | Producer::TwoList(Method::Complete) => {
            [1999, 2000, 2001, 300]
        }
        Producer::List(Method::Stream) | Producer::TwoList(Method::Stream) => [255, 256, 257, 512],
        Producer::List(Method::IndexedList) | Producer::TwoList(Method::IndexedList) => {
            [256, 257, 1000, 255]
        }
        Producer::StringLines | Producer::TupleStringList => [255, 256, 257, 1000],
    };
    schedule[variant % 4]
}

fn mk(producer: Producer, consumer: Consumer, n: usize, tiny: bool) -> Case {
    Case {
        producer,
        consumer,
        n,
        tiny_nursery: tiny,
    }
}

/// Property 1 — full-matrix sweep: every cell once, primary threshold size,
/// k=255 (a chunk-boundary fencepost). Plus a few empty/singleton edges.
fn phase1_cases() -> Vec<Case> {
    let mut cases: Vec<Case> = all_pairs(255)
        .into_iter()
        .map(|(p, c)| mk(p, c, size_for(p, 0), false))
        .collect();
    // Empty / singleton edges (the classic "empty stream at chunk boundary 0").
    cases.push(mk(Producer::List(Method::Stream), Consumer::Full, 0, false));
    cases.push(mk(Producer::List(Method::Stream), Consumer::BranchOnly, 0, false));
    cases.push(mk(Producer::StringLines, Consumer::BranchOnly, 0, false));
    cases.push(mk(Producer::List(Method::IndexedList), Consumer::Full, 1, false));
    cases.truncate(60);
    cases
}

/// Property 2 — hot-cells weighted: a full base pass (different size variant,
/// k=257) PLUS extra runs on the priority cells across sizes and k. Capped 60.
fn phase2_cases() -> Vec<Case> {
    // Base pass: every cell once at variant 1, k=257.
    let mut cases: Vec<Case> = all_pairs(257)
        .into_iter()
        .map(|(p, c)| mk(p, c, size_for(p, 1), false))
        .collect();

    // Priority 1: #313 — Tuple × {MapFilterPrefix, LinesOfPartial} over sizes.
    for &k in &[255usize, 256, 257] {
        cases.push(mk(Producer::TupleStringList, Consumer::MapFilterPrefix(k), k, false));
        cases.push(mk(Producer::TupleStringList, Consumer::LinesOfPartial(k), k, false));
    }
    // Priority 2: stream × Prefix(256±1).
    for &k in &[255usize, 256, 257] {
        cases.push(mk(Producer::List(Method::Stream), Consumer::Prefix(k), 256, false));
    }
    // Priority 3: any × PrefixThenEffect (re-enter registry).
    for m in [Method::Complete, Method::Stream, Method::IndexedList] {
        cases.push(mk(Producer::List(m), Consumer::PrefixThenEffect(256), 256, false));
    }
    // Priority 4: TwoList × ZipInterleave (two live parked streams).
    for m in [Method::Complete, Method::Stream, Method::IndexedList] {
        cases.push(mk(Producer::TwoList(m), Consumer::ZipInterleave(256), 257, false));
    }
    // Priority 5: Complete × Full at the spine fencepost.
    for &n in &[1999usize, 2000, 2001] {
        cases.push(mk(Producer::List(Method::Complete), Consumer::Full, n, false));
    }
    // Priority 7: lazy producer × ForceTwice.
    cases.push(mk(Producer::List(Method::Stream), Consumer::ForceTwice(256), 256, false));
    cases.push(mk(Producer::List(Method::IndexedList), Consumer::ForceTwice(256), 256, false));
    // Priority 8: StringLines × BranchOnly (laziness leak into unforced branch).
    for &n in &[0usize, 1, 256] {
        cases.push(mk(Producer::StringLines, Consumer::BranchOnly, n, false));
    }
    cases.truncate(60);
    cases
}

/// Tiny-nursery-safe size: moderate `n` (≤512) so the baseline machine fits at
/// `TINY_NURSERY_SIZE` while still crossing the 256 chunk boundary (forcing the
/// GC-and-retry path mid-materialization). Larger `n` would OOM the baseline in
/// both modes — noise, not a lazy bug.
fn tiny_size_for(p: Producer) -> usize {
    match p {
        // Just over one chunk → a 2nd chunk builds, GC may fire between.
        Producer::List(Method::Stream) | Producer::TwoList(Method::Stream) => 300,
        Producer::List(Method::IndexedList) | Producer::TwoList(Method::IndexedList) => 257,
        Producer::List(Method::Complete) | Producer::TwoList(Method::Complete) => 300,
        Producer::StringLines | Producer::TupleStringList => 300,
    }
}

/// Property 3 — tiny-nursery: full base pass at tiny nursery (k=1, tiny-safe
/// sizes) plus the hottest GC-stress cells. Capped 40.
fn phase3_cases() -> Vec<Case> {
    let mut cases: Vec<Case> = all_pairs(1)
        .into_iter()
        .map(|(p, c)| mk(p, c, tiny_size_for(p), true))
        .collect();
    // Extra chunk-boundary stress (GC fires while building the 2nd chunk).
    cases.push(mk(Producer::TupleStringList, Consumer::LinesOfPartial(255), 300, true));
    cases.push(mk(Producer::List(Method::Stream), Consumer::Prefix(257), 300, true));
    cases.push(mk(Producer::TwoList(Method::Stream), Consumer::ZipInterleave(256), 300, true));
    cases.truncate(40);
    cases
}

/// All cases across the three properties — used to warm the compile cache.
fn all_property_cases() -> Vec<Case> {
    let mut v = phase1_cases();
    v.extend(phase2_cases());
    v.extend(phase3_cases());
    v
}

// ===========================================================================
// Warm-up: compile every DISTINCT template source once (in-process). Populates
// the GHC-extract disk cache so subprocess workers are run-only, and validates
// (compile succeeds) ∧ (reference == lazy-default) for every source/size used.
// ===========================================================================

fn warm_cache() {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    let cases = all_property_cases();
    let mut compiled = 0usize;
    for case in &cases {
        // One representative per distinct SOURCE. The source ignores delivery
        // method, nursery, and `n` (size flows through handler data) — so we
        // normalize to a SMALL, default-nursery probe: enough to validate the
        // template compiles and its in-process reference matches, without the
        // baseline-OOM noise a tiny-nursery large-`n` case would inject here.
        let src = build_source(case);
        if !seen.insert(src) {
            continue;
        }
        let probe = Case {
            n: 4,
            tiny_nursery: false,
            ..*case
        };
        compiled += 1;
        match run_case_inproc(&probe) {
            Ok(v) => {
                let want = reference(&probe);
                assert_eq!(
                    v,
                    want,
                    "warmup in-process reference mismatch for {}",
                    probe.encode()
                );
            }
            Err(e) => panic!(
                "warmup template failed to compile/run {}: {e}",
                probe.encode()
            ),
        }
    }
    eprintln!("warmup: compiled {compiled} distinct template sources");
}

#[test]
fn warmup_compile_all_templates() {
    warm_cache();
}

// ===========================================================================
// Smoke test — proves the subprocess harness end-to-end on one cell.
// ===========================================================================

#[test]
fn smoke_subprocess_roundtrip() {
    let case = mk(Producer::List(Method::Complete), Consumer::Full, 50, false);
    assert!(check_case(&case), "smoke case diverged (see stderr)");
}

// ===========================================================================
// THE PROPERTY SUITE: warm → 3 phases → coverage assert → bug-map dump.
// One orchestrating test guarantees ordering, counter correctness, and a
// single consolidated report regardless of cargo's test scheduling.
// ===========================================================================

#[test]
fn lazy_consumption_property_suite() {
    warm_cache();

    let phases = [
        ("P1 full-matrix", phase1_cases()),
        ("P2 hot-weighted", phase2_cases()),
        ("P3 tiny-nursery", phase3_cases()),
    ];
    let mut total = 0usize;
    for (name, cases) in &phases {
        assert!(
            cases.len() <= 60,
            "property {name} exceeds 60-case budget: {}",
            cases.len()
        );
        eprintln!("=== {name}: {} cases ===", cases.len());
        for case in cases {
            check_case(case);
            total += 1;
        }
    }

    // --- coverage: every matrix cell executed >= 3 times -------------------
    let cov = COVERAGE.lock().unwrap().clone();
    let targets = coverage_targets();
    let mut under = Vec::new();
    for cell in &targets {
        let count = cov.get(cell).copied().unwrap_or(0);
        if count < 3 {
            under.push(format!("{cell}: {count}"));
        }
    }
    eprintln!(
        "coverage: {} cells, {} total executions",
        targets.len(),
        total
    );
    assert!(
        under.is_empty(),
        "cells executed < 3 times:\n  {}",
        under.join("\n  ")
    );

    // --- bug-map dump (suite stays GREEN; bugs are characterized, not fatal) -
    let bugs = BUGS.lock().unwrap();
    if bugs.is_empty() {
        eprintln!("\nBUG MAP: no divergences found across {total} cases.");
    } else {
        eprintln!("\n===== BUG MAP ({} divergences) =====", bugs.len());
        let mut by_cell: BTreeMap<String, Vec<&Bug>> = BTreeMap::new();
        for b in bugs.iter() {
            by_cell.entry(b.cell.clone()).or_default().push(b);
        }
        for (cell, list) in &by_cell {
            eprintln!("  [{cell}] {} bug(s)", list.len());
            for b in list {
                eprintln!("    {} case={} :: {}", b.class, b.case, first_line(&b.detail));
            }
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

// ===========================================================================
// Dedicated: #313 reproduction + boundary characterization.
// ===========================================================================

/// Sweep the #313 cell (Tuple stderr → `lines` → map/filter/take, partially
/// consumed) across consumers, sizes, and k. Reports which combinations
/// diverge (the boundary map for the fix wave). Never fails the suite — it
/// records into the shared BUG collector and prints a focused table.
#[test]
fn repro_313_boundary() {
    eprintln!("\n===== #313 BOUNDARY SWEEP (Tuple/StringLines × lines-consumers) =====");
    let mut trials = Vec::new();
    for shape in [Producer::TupleStringList, Producer::StringLines] {
        // LinesOfPartial: take k (lines field)
        for &k in &[1usize, 2, 255, 256, 257] {
            for &n in &[2usize, 256, 257, 600] {
                trials.push(mk(shape, Consumer::LinesOfPartial(k), n, false));
            }
        }
    }
    // MapFilterPrefix over lines (Tuple only template).
    for &k in &[1usize, 255, 256] {
        for &n in &[20usize, 256, 600] {
            trials.push(mk(Producer::TupleStringList, Consumer::MapFilterPrefix(k), n, false));
        }
    }
    // Full over lines (force the whole derived list).
    for &n in &[2usize, 256, 600] {
        trials.push(mk(Producer::TupleStringList, Consumer::Full, n, false));
        trials.push(mk(Producer::StringLines, Consumer::Full, n, false));
    }

    let mut triggered = 0usize;
    for case in &trials {
        let ok = check_case(case);
        if !ok {
            triggered += 1;
            eprintln!("  TRIGGER: {}", case.encode());
        }
    }
    eprintln!(
        "#313 INLINE sweep: {}/{} cases diverged. (See plans/proptest-findings-lazy.md.)",
        triggered,
        trials.len()
    );

    // --- lib-module consumer probe: the SAME partial-consumption shape, but
    // the map/filter/lines lives in a `.tidepool/lib` module (Probe.hs). This
    // is the documented #313 trigger. Contrast with the inline sweep above.
    eprintln!("\n--- #313 LIB-MODULE probe (Probe.hs tN over the tuple/string) ---");
    // (fn, result-type, producer-shape, n)
    let lib_probes: &[(&str, &str, Producer, usize)] = &[
        ("t1", "[Text]", Producer::TupleStringList, 4), // lines e
        ("t2", "[Text]", Producer::TupleStringList, 4), // filter "h"-prefix (lines e)
        ("t5", "[Text]", Producer::TupleStringList, 4), // filter len>1 (lines e)
        ("t6", "[Bool]", Producer::TupleStringList, 4), // map ("h"-prefix) (lines e)
        ("t7", "[Text]", Producer::TupleStringList, 4), // filter ++ [code, out]
    ];
    let mut lib_trapped = 0usize;
    for (f_name, ty, producer, n) in lib_probes {
        let on = spawn_lib_probe(f_name, ty, *producer, *n, true);
        let off = spawn_lib_probe(f_name, ty, *producer, *n, false);
        let describe = |o: &WorkerOutcome| -> String {
            match o {
                WorkerOutcome::Ok(v) => format!("OK {v}"),
                WorkerOutcome::Err(e) => format!("ERR {}", first_line(e)),
                WorkerOutcome::Signal(s, _) => format!("SIGNAL {s}"),
                WorkerOutcome::Unknown(_) => "UNKNOWN".into(),
            }
        };
        let trapped = matches!(on, WorkerOutcome::Err(_) | WorkerOutcome::Signal(..))
            || matches!(off, WorkerOutcome::Err(_) | WorkerOutcome::Signal(..));
        if trapped {
            lib_trapped += 1;
        }
        eprintln!(
            "  {f_name}: lazy-ON={} | lazy-OFF={} {}",
            describe(&on),
            describe(&off),
            if trapped { "  <<< #313 TRIGGER" } else { "" }
        );
    }
    eprintln!(
        "#313 LIB-MODULE probe: {}/{} lib functions trapped (inline equivalents: {} of {} clean).",
        lib_trapped,
        lib_probes.len(),
        trials.len() - triggered,
        trials.len()
    );

    // --- t7 bisection: the ONLY trapping lib function is t7, which binds ALL
    // three tuple fields and `<>`-appends [show c, o] to the filtered lines.
    // Bisect that shape INLINE to pin which ingredient trips the case trap.
    eprintln!("\n--- #313 t7 INLINE bisection (which ingredient traps?) ---");
    let bisect = [
        (Consumer::TupleShowCode, "[pack (show c)] only (1st field)"),
        (Consumer::TupleAppendStdout, "lines e <> [o] (2nd field append)"),
        (Consumer::TupleAllFields, "EXACT inline t7 (all 3 fields + show + <>)"),
    ];
    for (consumer, desc) in bisect {
        let case = mk(Producer::TupleStringList, consumer, 4, false);
        let ok = check_case(&case);
        eprintln!(
            "  {desc}: {}",
            if ok { "clean" } else { "DIVERGED <<<" }
        );
    }
    // Characterization only — do NOT fail. The suite documents the boundary.
}

// ===========================================================================
// Dedicated: 100k node-cap boundary. lazy-ON parks (no cap); lazy-OFF drains
// through the cap and must give a CLEAN error (not a trap/signal) just over,
// and succeed just under. This is the one place lazy-ON vs lazy-OFF are
// EXPECTED to differ (documented kill-switch semantic), so it is NOT part of
// the equality property.
// ===========================================================================

#[test]
fn cap_boundary_clean_error() {
    // Each "item-N" Text contributes ~3 nodes + 3 spine nodes ≈ 6/elem. The
    // 100_000-node cap is crossed near ~16.6k elements; bracket generously.
    let under = mk(Producer::List(Method::Complete), Consumer::Full, 10_000, false);
    let over = mk(Producer::List(Method::Complete), Consumer::Full, 40_000, false);

    // Under the cap: both modes succeed and equal the reference.
    let on = spawn_worker(&under, true);
    let off = spawn_worker(&under, false);
    assert!(
        matches!(on, WorkerOutcome::Ok(ref v) if *v == reference(&under)),
        "under-cap lazy-ON should equal reference, got {on:?}"
    );
    assert!(
        matches!(off, WorkerOutcome::Ok(ref v) if *v == reference(&under)),
        "under-cap lazy-OFF should equal reference, got {off:?}"
    );

    // Over the cap: lazy-ON parks (length forces the spine but the parked
    // path has no node cap) → succeeds; lazy-OFF must give a CLEAN error
    // mentioning the limit — NOT a fatal signal.
    let on = spawn_worker(&over, true);
    let off = spawn_worker(&over, false);
    match on {
        WorkerOutcome::Ok(ref v) if *v == reference(&over) => {}
        WorkerOutcome::Signal(sig, ref bc) => {
            record_bug("B3", &over, format!("over-cap lazy-ON fatal signal {sig}\n{}", tail(bc)));
            panic!("over-cap lazy-ON crashed with signal {sig}");
        }
        other => panic!("over-cap lazy-ON expected reference, got {other:?}"),
    }
    match off {
        WorkerOutcome::Err(ref e)
            if e.contains("too large") || e.contains("TooLarge") || e.contains("100000") => {}
        WorkerOutcome::Signal(sig, ref bc) => {
            record_bug(
                "B3",
                &over,
                format!("over-cap lazy-OFF fatal signal {sig} (should be clean TooLarge)\n{}", tail(bc)),
            );
            panic!("over-cap lazy-OFF crashed with signal {sig} instead of clean error");
        }
        other => panic!("over-cap lazy-OFF expected clean TooLarge error, got {other:?}"),
    }
}

// ===========================================================================
// #313 named repro. Run with:
//   cargo test -p tidepool-runtime --test proptest_lazy_consumption -- \
//     --ignored --exact repro_313_lib_t7_case_trap --nocapture
//
// MATRIX CELL: TupleStringList × (lib-module consumer `t7`)
//   t7 = \(c,o,e) -> filter (\l -> len l > 1) (lines e) <> [pack (show c), o]
//        defined in `.tidepool/lib/Probe.hs` (NOT inline).
// OBSERVED:  yield error — "case trap: scrutinee constructor not among case
//            alternatives (tag mismatch)" — IDENTICAL under lazy-ON and
//            lazy-OFF.
// EXPECTED:  ["item-0", … , "item-{n-1}", "0", "stdout"]  (the inline-t7
//            reference, which this harness confirms is produced when the
//            SAME source is written inline — see `repro_313_boundary`).
// CLASS:     B2 (runtime trap where the reference succeeds).
// COMPONENT: cross-module unfolding of a `.tidepool/lib` function. NOT the
//            lazy effect-results machinery — every inline equivalent (incl.
//            the exact t7 body) is clean, and both lazy modes trap
//            identically, so the lazy channel is exonerated. The trigger is
//            the module boundary × t7's use of `show @Int` / `<>` / `pack`
//            (typeclass dictionaries resolved through Probe's `.hi`
//            unfolding). Sibling lib fns t1/t2/t5/t6 (Prelude-only:
//            lines/filter/map/isPrefixOf) do NOT trap.
// SEED:      deterministic — (fn=t7, Producer=TupleStringList, n=4); no
//            random proptest seed (the lazy property suite found zero
//            divergences, so there is no `.proptest-regressions` entry).
#[test]
#[ignore = "BUG #313: lib-module t7 cross-module unfolding case-traps; inline-identical source is clean"]
fn repro_313_lib_t7_case_trap() {
    let on = spawn_lib_probe("t7", "[Text]", Producer::TupleStringList, 4, true);
    let off = spawn_lib_probe("t7", "[Text]", Producer::TupleStringList, 4, false);
    for (label, o) in [("lazy-ON", &on), ("lazy-OFF", &off)] {
        match o {
            WorkerOutcome::Err(e) if e.contains("case trap") => {
                eprintln!("{label}: reproduced #313 — {}", first_line(e));
            }
            other => panic!(
                "{label}: expected #313 case trap, got {other:?}\n\
                 (if this no longer traps, #313 may be FIXED — update/retire this repro)"
            ),
        }
    }
}

/// Control for the repro above: the EXACT same source written INLINE in the
/// eval module is clean. This is the load-bearing half of the #313 boundary —
/// it localizes the bug to cross-module compilation, exonerating both the
/// lazy-results channel and the t7 shape itself.
#[test]
#[ignore = "BUG #313 control: inline-identical t7 is clean (localizes the bug to the module boundary)"]
fn repro_313_inline_t7_is_clean() {
    let case = mk(Producer::TupleStringList, Consumer::TupleAllFields, 4, false);
    let want = reference(&case);
    for lazy in [true, false] {
        match spawn_worker(&case, lazy) {
            WorkerOutcome::Ok(got) => assert_eq!(got, want, "inline t7 lazy={lazy}"),
            other => panic!("inline t7 lazy={lazy} expected {want}, got {other:?}"),
        }
    }
}
