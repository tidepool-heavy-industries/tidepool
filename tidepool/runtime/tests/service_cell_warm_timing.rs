//! Two ordinary cells through one resident session, with machine-readable
//! timing and cross-cell reuse evidence.
//!
//! This is the completion campaign's early timing harness. It drives the
//! production route that exists at the recorded revision and reports what that
//! route actually emits. At `e36794e6f9` the only production route is the
//! finite resident route below; the service-cell route lands with R1/S1 and is
//! selected by the one constant [`CELL_ROUTE`].
//!
//! Evidence sources are production fields, not a parallel telemetry channel:
//!
//! * `tidepool_runtime::compile` — the always-on `compile summary` tracing
//!   event (`wall_ms`, `modules`, partial phase totals) and the raw
//!   `tidepool-compile-summary ...` compiler stderr line it is parsed from.
//! * `tidepool_codegen::prepared_compile` — native prepared compilation
//!   (`total_ms`, scale, and opt-in `TIDEPOOL_CODEGEN_DETAIL=1` definition
//!   rows carrying `unit`/`module`).
//! * `tidepool_runtime::prepared_install` — install attribution
//!   (`resolve_imports_ms`, `link_ms`, `compile_install_ms`, `imports`,
//!   `compiled_off_checkout`).
//! * `tidepool_codegen::image_registry` — native image decisions
//!   (`disposition`, `image_entry`, `image_instance`, and debug-level
//!   `group_unit`/`group_module`).
//! * compiler-side `tidepool-reuse` JSON lines retained in
//!   `<artifact-root>/compiler-transactions/*/compiler.stderr` — the worker's
//!   own reuse decisions (`stage`, `decision`, `reason`, `unit`, `module`,
//!   `version_kind`, `version`).
//!
//! The finite resident route has no actor `CallScope`, so
//! `exomonad_actor::call_timing` is absent. Fields that route cannot produce
//! are reported as `null` with an explicit `unavailable` list; they are not
//! invented from wall clocks. If a future route emits the actor summary, this
//! harness consumes it automatically and records that source instead.
//!
//! Run with a retained per-case artifact root:
//!
//! ```text
//! just test-target tidepool-runtime session \
//!   --exact service_cell_warm_timing::two_ordinary_cells_report_no_reuse_baseline \
//!   --expected-count 1 --output-dir /srv/swarm/evidence/.../runner \
//!   --retain-artifacts --trace-profile full
//! ```
//!
//! The JSONL path is `$TIDEPOOL_WARM_TIMING_OUT` when a direct invocation can
//! set it; otherwise it is `<TIDEPOOL_TEST_ARTIFACT_ROOT>/warm-timing.jsonl`
//! when the counted runner supplied one.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};
use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, ResidentOutcome, ResidentSession, SessionLib,
    TurnRequest, TurnResult, TurnTemplate,
};

/// Which production route drives the two cells.
///
/// `FiniteResident` is the only route at `e36794e6f9`: two ordinary turns over
/// one unbootstrapped resident session (`run_turn` + `run_bind_with_sites`).
///
/// `Service` is the R1/S1 service-owned cell route. It is intentionally not
/// implemented on this revision; selecting it fails loudly instead of silently
/// falling back.
#[allow(
    dead_code,
    reason = "Service is the named future route; selecting it before R1/S1 must refuse"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellRoute {
    FiniteResident,
    Service,
}

/// THE ONE-LINE ROUTE SWITCH.
///
/// Once R1/S1 publish the service-cell entry point, change the next line to
/// `const CELL_ROUTE: CellRoute = CellRoute::Service;` and implement the
/// `CellRoute::Service` arm in [`drive_service_cell`] with that entry point.
/// Nothing else in this harness selects a route.
const CELL_ROUTE: CellRoute = CellRoute::FiniteResident;

const ROUTE_SWITCH_INSTRUCTION: &str = "Change `const CELL_ROUTE: CellRoute = CellRoute::FiniteResident;` to `const CELL_ROUTE: CellRoute = CellRoute::Service;` once R1/S1 land, then implement the `CellRoute::Service` arm in `drive_service_cell` with the R1/S1 entry point.";

/// The revision this harness was written against. The counted runner filters
/// `TIDEPOOL_SOURCE_REVISION` out of its delegated environment, so the default
/// is the recorded base and an explicit env value only applies to direct runs.
const RECORDED_SOURCE_REVISION: &str = "e36794e6f9";

/// Hard bounds for captured events and compiler stderr parsing. A prior
/// unbounded worker-shape loop consumed 84 GiB; these are checked before
/// allocation and iteration.
const MAX_CAPTURE_BYTES: usize = 64 << 20;
const MAX_LOG_LINES: usize = 500_000;
const MAX_STDERR_BYTES: usize = 8 << 20;
const MAX_CELL_COMPILER_STDERR_BYTES: usize = 32 << 20;
const MAX_STDERR_LINES: usize = 200_000;
const MAX_REUSE_ROWS: usize = 20_000;
const MAX_MODULE_COUNT_ROWS: usize = 20_000;
const MAX_TRANSACTION_ENTRIES: usize = 4_096;
const MAX_CELL_TRANSACTIONS: usize = 64;

#[derive(Clone)]
struct CapturedLog {
    bytes: Arc<Mutex<Vec<u8>>>,
    truncated: Arc<AtomicBool>,
}

impl CapturedLog {
    fn new() -> Self {
        Self {
            bytes: Arc::new(Mutex::new(Vec::new())),
            truncated: Arc::new(AtomicBool::new(false)),
        }
    }

    fn offset(&self) -> usize {
        self.bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn slice_from(&self, start: usize) -> Vec<u8> {
        let guard = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(start..).unwrap_or_default().to_vec()
    }

    fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Relaxed)
    }
}

impl std::io::Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut guard = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let room = MAX_CAPTURE_BYTES.saturating_sub(guard.len());
        if room < bytes.len() {
            guard.extend_from_slice(&bytes[..room]);
            self.truncated.store(true, Ordering::Relaxed);
        } else {
            guard.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
    type Writer = CapturedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Process-wide capture: native install/compile events can be emitted from the
/// blocking pool, so a thread-local default would lose them.
fn install_event_capture() -> CapturedLog {
    let log = CapturedLog::new();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "warn,tidepool_runtime::compile=info,tidepool_runtime::prepared_install=info,\
             tidepool_codegen::prepared_compile=info,tidepool_codegen::image_registry=debug,\
             exomonad_actor::call_timing=info",
        ))
        .with_writer(log.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect(
        "the isolated timing case owns the process tracing subscriber; run it as an exact \
         single case",
    );
    log
}

/// Keep the diagnostic artifact root that compiler transactions are retained
/// under. The counted runner normally supplies it; a direct libtest invocation
/// gets a temporary one so the same harness still produces reuse evidence.
struct DiagnosticRoot {
    root: PathBuf,
    _owned: Option<tempfile::TempDir>,
}

impl DiagnosticRoot {
    fn discover() -> Self {
        if let Some(root) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            let root = PathBuf::from(root);
            assert!(
                root.is_absolute() && root.is_dir(),
                "TIDEPOOL_TEST_ARTIFACT_ROOT must be an existing absolute directory"
            );
            return Self {
                root,
                _owned: None,
            };
        }
        let owned = tempfile::tempdir().expect("timing diagnostic root");
        let root = owned.path().to_path_buf();
        std::env::set_var("TIDEPOOL_TEST_ARTIFACT_ROOT", &root);
        Self {
            root,
            _owned: Some(owned),
        }
    }

    fn transactions(&self) -> PathBuf {
        self.root.join("compiler-transactions")
    }
}

/// Force the production timing and compiler-detail channels this harness reads.
/// The counted runner sets most of these; forcing them keeps a direct run and a
/// differently-configured runner from silently reporting absent evidence.
fn ensure_timing_environment() {
    std::env::set_var("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE", "1");
    std::env::set_var("TIDEPOOL_KEEP_TEST_LOGS", "1");
    std::env::set_var("TIDEPOOL_TIMING", "1");
    std::env::set_var("TIDEPOOL_TIMING_SUMMARY", "1");
    std::env::set_var("TIDEPOOL_CODEGEN_DETAIL", "1");
    std::env::set_var("TIDEPOOL_TEST_TRACE_PROFILE", "full");
}

/// A compiler reuse decision identity. `stage` and the owner version are part
/// of the identity: two cells that compile the same `(unit, module)` name with
/// different source fingerprints are different units.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReuseIdentity {
    stage: String,
    unit: String,
    module: String,
    version_kind: String,
    version: String,
}

impl ReuseIdentity {
    fn from_row(row: &Value) -> Option<Self> {
        let unit = row["unit"].as_str()?;
        let module = row["module"].as_str()?;
        if unit == "null" || module == "null" {
            return None;
        }
        Some(Self {
            stage: row["stage"].as_str()?.to_owned(),
            unit: unit.to_owned(),
            module: module.to_owned(),
            version_kind: row["version_kind"].as_str().unwrap_or("unknown").to_owned(),
            version: row["version"].as_str().unwrap_or("unknown").to_owned(),
        })
    }

    /// Owner identity without the stage: a compiler-stage hit on a unit another
    /// cell compiled is still cross-cell reuse even if the exact stage differs.
    fn owner_key(&self) -> (String, String, String, String) {
        (
            self.unit.clone(),
            self.module.clone(),
            self.version_kind.clone(),
            self.version.clone(),
        )
    }

    fn to_json(&self) -> Value {
        json!({
            "stage": self.stage,
            "unit": self.unit,
            "module": self.module,
            "version_kind": self.version_kind,
            "version": self.version,
        })
    }
}

fn reuse_units_json(units: &BTreeSet<ReuseIdentity>) -> Value {
    Value::Array(units.iter().map(ReuseIdentity::to_json).collect())
}

fn module_units_json(units: &BTreeSet<(String, String)>) -> Value {
    Value::Array(
        units
            .iter()
            .map(|(unit, module)| json!({"unit": unit, "module": module}))
            .collect(),
    )
}

fn owner_set(units: &BTreeSet<ReuseIdentity>) -> BTreeSet<(String, String, String, String)> {
    units.iter().map(ReuseIdentity::owner_key).collect()
}

fn merge_json(chunks: Vec<Value>) -> Value {
    let mut merged = serde_json::Map::new();
    for chunk in chunks {
        match chunk {
            Value::Object(fields) => merged.extend(fields),
            _ => panic!("JSON chunk must be an object"),
        }
    }
    Value::Object(merged)
}

/// One cell's production attribution.
struct CellRow {
    cell: u8,
    label: &'static str,
    source: &'static str,
    compile_wall_ms: u128,
    run_wall_ms: u128,
    run_error: Option<String>,
    compile_ms: Option<u64>,
    compile_ms_source: &'static str,
    compile_count: Option<u64>,
    compile_summary_modules: Vec<u64>,
    stderr_compile_summaries: Vec<RawCompileSummary>,
    prepared_compile_ms: u64,
    prepared_compile_count: u64,
    prepared_compile_totals_ms: Vec<u64>,
    prepared_functions_defined: u64,
    prepared_code_bytes: u64,
    install_rows: usize,
    install_compile_ms: u64,
    install_resolve_imports_ms: u64,
    install_link_ms: u64,
    install_imports: Vec<u64>,
    compiled_off_checkout: usize,
    image_decisions: BTreeMap<String, u64>,
    image_live_hits: u64,
    image_group_units: BTreeSet<(String, String)>,
    actor_call_timing: Vec<Value>,
    checkout_wait_ms: Option<u64>,
    preparation_wait_ms: Option<u64>,
    publication_attempts: Option<u64>,
    semantic_rebases: Option<u64>,
    work_units: BTreeSet<ReuseIdentity>,
    miss_units: BTreeSet<ReuseIdentity>,
    hit_units: BTreeSet<ReuseIdentity>,
    seen_units: BTreeSet<ReuseIdentity>,
    module_count_units: BTreeSet<(String, String)>,
    reuse_rows: usize,
    reuse_decisions: BTreeMap<String, u64>,
    units_source: &'static str,
    codegen_functions: u64,
    codegen_code_bytes: u64,
    transaction_dirs: Vec<PathBuf>,
    compiler_stderr_bytes: usize,
    compiler_stderr_truncated: bool,
    transactions_truncated: bool,
    event_log_truncated: bool,
}

impl CellRow {
    fn to_json(&self, route: &str, revision: &str) -> Value {
        merge_json(vec![
            json!({
                "kind": "cell",
                "schema": 1,
                "route": route,
                "source_revision": revision,
                "cell": self.cell,
                "label": self.label,
                "source": self.source,
                "compile_ms": self.compile_ms,
                "compile_ms_source": self.compile_ms_source,
                "compile_count": self.compile_count,
                "compile_summary_modules": self.compile_summary_modules,
                "stderr_compile_summaries": self.stderr_compile_summaries.iter().map(RawCompileSummary::to_json).collect::<Vec<_>>(),
            }),
            json!({
                "prepared_compile_ms": self.prepared_compile_ms,
                "prepared_compile_count": self.prepared_compile_count,
                "prepared_compile_totals_ms": self.prepared_compile_totals_ms,
                "prepared_functions_defined": self.prepared_functions_defined,
                "prepared_code_bytes": self.prepared_code_bytes,
                "install_rows": self.install_rows,
                "install_compile_ms": self.install_compile_ms,
                "install_resolve_imports_ms": self.install_resolve_imports_ms,
                "install_link_ms": self.install_link_ms,
                "install_imports": self.install_imports,
                "compiled_off_checkout_count": self.compiled_off_checkout,
            }),
            json!({
                "image_decisions": self.image_decisions,
                "image_live_hits": self.image_live_hits,
                "image_group_units": module_units_json(&self.image_group_units),
                "checkout_wait_ms": self.checkout_wait_ms,
                "preparation_wait_ms": self.preparation_wait_ms,
                "publication_attempts": self.publication_attempts,
                "semantic_rebases": self.semantic_rebases,
                "actor_call_timing": self.actor_call_timing,
            }),
            json!({
                "units_compiled": self.work_units.len(),
                "units_compiled_identities": reuse_units_json(&self.work_units),
                "units_missed": self.miss_units.len(),
                "units_missed_identities": reuse_units_json(&self.miss_units),
                "units_hit": self.hit_units.len(),
                "units_hit_identities": reuse_units_json(&self.hit_units),
                "units_seen": self.seen_units.len(),
                "units_seen_identities": reuse_units_json(&self.seen_units),
                "module_count_units": module_units_json(&self.module_count_units),
                "reuse_rows": self.reuse_rows,
                "reuse_decisions": self.reuse_decisions,
                "units_source": self.units_source,
            }),
            json!({
                "harness_wall_ms": {
                    "compile": self.compile_wall_ms,
                    "run": self.run_wall_ms,
                },
                "run_error": self.run_error,
                "codegen": {
                    "functions": self.codegen_functions,
                    "code_bytes": self.codegen_code_bytes,
                },
                "transactions": {
                    "directories": self.transaction_dirs.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                    "compiler_stderr_bytes": self.compiler_stderr_bytes,
                    "compiler_stderr_truncated": self.compiler_stderr_truncated,
                    "transactions_truncated": self.transactions_truncated,
                },
                "event_log_truncated": self.event_log_truncated,
            }),
        ])
    }
}

struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    injected: Vec<String>,
    generation: u64,
    funcs: u64,
    code_bytes: u64,
}

impl Notebook {
    fn new() -> Self {
        tidepool_testing::eval_harness::require_extract();
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);
        let mut include = tidepool_testing::eval_harness::effects_include().to_vec();
        include.push(tidepool_testing::eval_harness::prelude_path());
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            SessionId(1),
            root.path().join("decl-lib"),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
            funcs: 0,
            code_bytes: 0,
        }
    }

    fn templates(&self) -> Vec<TurnTemplate> {
        resident_workbench_templates(
            &self.preamble,
            &self.effect_stack,
            &self.injected.join("\n"),
        )
    }

    fn compile(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let retained = self.session.prepared_retained();
        let templates = self.templates();
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: text,
                    templates: &templates,
                    include: &include,
                    session_root: self.root.path(),
                    inject_modules: &self.injected,
                    gen: self.generation,
                    verdict: None,
                    target: None,
                    retained_imports: &retained,
                },
                settlement,
            )
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}",
                tidepool_runtime::classify_compile(&failure.error).message
            )
        })
    }
}

fn transaction_dirs(root: &Path) -> BTreeSet<PathBuf> {
    match std::fs::read_dir(root) {
        Ok(entries) => entries
            .take(MAX_TRANSACTION_ENTRIES)
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

#[derive(Clone, Debug)]
struct RawCompileSummary {
    modules: u64,
    wall_ms: u64,
}

impl RawCompileSummary {
    fn to_json(&self) -> Value {
        json!({"modules": self.modules, "wall_ms": self.wall_ms})
    }
}

#[derive(Default)]
struct WorkerEvidence {
    reuse: Vec<Value>,
    module_counts: Vec<(String, String)>,
    compile_summaries: Vec<RawCompileSummary>,
    decisions: BTreeMap<String, u64>,
    bytes: usize,
    truncated: bool,
}

fn parse_compile_summary(rest: &str) -> Option<RawCompileSummary> {
    let mut modules = None;
    let mut wall_ms = None;
    for field in rest.split_whitespace() {
        if let Some(value) = field.strip_prefix("modules=") {
            modules = value.parse::<u64>().ok();
        } else if let Some(value) = field.strip_prefix("wall_ms=") {
            wall_ms = value.parse::<u64>().ok();
        }
    }
    Some(RawCompileSummary {
        modules: modules?,
        wall_ms: wall_ms?,
    })
}

fn parse_worker_stderr(text: &str) -> WorkerEvidence {
    let mut evidence = WorkerEvidence::default();
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_STDERR_LINES {
            evidence.truncated = true;
            break;
        }
        let line = line.trim();
        if let Some(row) = line.strip_prefix("tidepool-reuse ") {
            if evidence.reuse.len() >= MAX_REUSE_ROWS {
                evidence.truncated = true;
                break;
            }
            if let Ok(value) = serde_json::from_str::<Value>(row) {
                if let Some(decision) = value["decision"].as_str() {
                    *evidence.decisions.entry(decision.to_owned()).or_default() += 1;
                }
                evidence.reuse.push(value);
            }
        } else if let Some(rest) = line.strip_prefix("tidepool-module-counts ") {
            if evidence.module_counts.len() >= MAX_MODULE_COUNT_ROWS {
                evidence.truncated = true;
                break;
            }
            let mut unit = None;
            let mut module = None;
            for field in rest.split_whitespace() {
                if let Some(value) = field.strip_prefix("owner_unit=") {
                    unit = Some(value.to_owned());
                } else if let Some(value) = field.strip_prefix("owner_module=") {
                    module = Some(value.to_owned());
                }
            }
            if let (Some(unit), Some(module)) = (unit, module) {
                evidence.module_counts.push((unit, module));
            }
        } else if let Some(rest) = line.strip_prefix("tidepool-compile-summary ") {
            if let Some(summary) = parse_compile_summary(rest) {
                evidence.compile_summaries.push(summary);
            }
        }
    }
    evidence
}

fn read_bounded(path: &Path, limit: usize) -> (String, bool) {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return (String::new(), false);
    };
    let limit = limit.min(MAX_STDERR_BYTES);
    let mut bytes = Vec::new();
    let truncated = file
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > limit;
    bytes.truncate(limit);
    (String::from_utf8_lossy(&bytes).into_owned(), truncated)
}

struct CapturedEvents {
    compile_summary_wall_ms: Vec<u64>,
    compile_summary_modules: Vec<u64>,
    prepared_compile_totals_ms: Vec<u64>,
    prepared_functions_defined: u64,
    prepared_code_bytes: u64,
    installs: Vec<Value>,
    image_decisions: BTreeMap<String, u64>,
    image_live_hits: u64,
    image_group_units: BTreeSet<(String, String)>,
    actor_call_timing: Vec<Value>,
}

fn parse_events(bytes: &[u8]) -> CapturedEvents {
    let mut captured = CapturedEvents {
        compile_summary_wall_ms: Vec::new(),
        compile_summary_modules: Vec::new(),
        prepared_compile_totals_ms: Vec::new(),
        prepared_functions_defined: 0,
        prepared_code_bytes: 0,
        installs: Vec::new(),
        image_decisions: BTreeMap::new(),
        image_live_hits: 0,
        image_group_units: BTreeSet::new(),
        actor_call_timing: Vec::new(),
    };
    for line in String::from_utf8_lossy(bytes).lines().take(MAX_LOG_LINES) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let target = value["target"].as_str().unwrap_or_default();
        let fields = &value["fields"];
        let message = fields["message"].as_str().unwrap_or_default();
        match (target, message) {
            ("tidepool_runtime::compile", "compile summary") => {
                if let Some(wall_ms) = fields["wall_ms"].as_u64() {
                    captured.compile_summary_wall_ms.push(wall_ms);
                }
                if let Some(modules) = fields["modules"].as_u64() {
                    captured.compile_summary_modules.push(modules);
                }
            }
            ("tidepool_codegen::prepared_compile", "prepared compile") => {
                if let Some(total_ms) = fields["total_ms"].as_u64() {
                    captured.prepared_compile_totals_ms.push(total_ms);
                }
                captured.prepared_functions_defined +=
                    fields["functions_defined"].as_u64().unwrap_or(0);
                captured.prepared_code_bytes += fields["code_bytes"].as_u64().unwrap_or(0);
            }
            ("tidepool_runtime::prepared_install", "prepared install") => {
                captured.installs.push(fields.clone());
            }
            ("tidepool_codegen::image_registry", _) => {
                if let Some(disposition) = fields["disposition"].as_str() {
                    *captured
                        .image_decisions
                        .entry(disposition.to_owned())
                        .or_default() += 1;
                    if disposition == "live_hit" {
                        captured.image_live_hits += 1;
                    }
                }
                if let (Some(unit), Some(module)) =
                    (fields["group_unit"].as_str(), fields["group_module"].as_str())
                {
                    captured
                        .image_group_units
                        .insert((unit.to_owned(), module.to_owned()));
                }
            }
            ("exomonad_actor::call_timing", "call timing") => {
                captured.actor_call_timing.push(fields.clone());
            }
            _ => {}
        }
    }
    captured
}

/// Drive one cell through the selected production route.
fn drive(
    route: CellRoute,
    notebook: &mut Notebook,
    log: &CapturedLog,
    diagnostics: &DiagnosticRoot,
    cell: u8,
    label: &'static str,
    source: &'static str,
) -> CellRow {
    match route {
        CellRoute::FiniteResident => {
            drive_finite_cell(notebook, log, diagnostics, cell, label, source)
        }
        CellRoute::Service => {
            drive_service_cell(notebook, log, diagnostics, cell, label, source)
        }
    }
}

/// Drive one cell and capture exactly the events and transactions that cell
/// produced. The session is synchronous, so the byte offset and the new
/// transaction directories are real cell boundaries.
fn drive_finite_cell(
    notebook: &mut Notebook,
    log: &CapturedLog,
    diagnostics: &DiagnosticRoot,
    cell: u8,
    label: &'static str,
    source: &'static str,
) -> CellRow {
    let log_start = log.offset();
    let before = transaction_dirs(&diagnostics.transactions());
    let before_codegen = notebook
        .session
        .codegen_totals()
        .unwrap_or((notebook.funcs, notebook.code_bytes));

    let compile_started = Instant::now();
    let TurnResult::Bind {
        bound,
        compiled,
        ..
    } = notebook.compile(source)
    else {
        panic!("{source:?} did not classify as a bind");
    };
    let compile_wall_ms = compile_started.elapsed().as_millis();
    let [binder] = bound.as_slice() else {
        panic!("{source:?} bound {} names", bound.len());
    };

    let run_started = Instant::now();
    let run_result = tidepool_testing::with_settlement(|settlement| {
        notebook.session.run_bind_with_sites(
            "warm_timing_bind",
            compiled.code(),
            binder,
            Generation(notebook.generation),
            settlement,
        )
    });
    let run_wall_ms = run_started.elapsed().as_millis();
    // The compile half is the baseline this case exists to record. At
    // `e36794e6f9` the finite route's install/run half refuses source work
    // without an owning runtime transaction; record that exact refusal rather
    // than losing the compile evidence, but fail on any other error.
    let run_error = match run_result {
        Ok(ResidentOutcome::Completed { .. }) => None,
        Ok(other) => panic!("{source:?} did not complete: {other:?}"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("requires its owning runtime transaction"),
                "{source:?} failed to run: {message}"
            );
            Some(message)
        }
    };

    let after = transaction_dirs(&diagnostics.transactions());
    let after_codegen = notebook.session.codegen_totals().unwrap_or(before_codegen);
    notebook.injected.push(binder.module.clone());

    let bytes = log.slice_from(log_start);
    let events = parse_events(&bytes);

    let mut worker = WorkerEvidence::default();
    let mut transaction_paths = Vec::new();
    let mut stderr_truncated = false;
    let mut new_directories = after.difference(&before).cloned().collect::<Vec<_>>();
    new_directories.sort();
    let transactions_truncated = new_directories.len() > MAX_CELL_TRANSACTIONS;
    new_directories.truncate(MAX_CELL_TRANSACTIONS);
    for directory in new_directories {
        let stderr = directory.join("compiler.stderr");
        if !stderr.is_file() {
            continue;
        }
        let remaining = MAX_CELL_COMPILER_STDERR_BYTES.saturating_sub(worker.bytes);
        if remaining == 0 {
            worker.truncated = true;
            break;
        }
        let (text, truncated) = read_bounded(&stderr, remaining);
        if text.is_empty() && !truncated {
            continue;
        }
        stderr_truncated |= truncated;
        worker.bytes += text.len();
        let parsed = parse_worker_stderr(&text);
        worker.reuse.extend(parsed.reuse);
        worker.module_counts.extend(parsed.module_counts);
        worker.compile_summaries.extend(parsed.compile_summaries);
        for (decision, count) in parsed.decisions {
            *worker.decisions.entry(decision).or_default() += count;
        }
        worker.truncated |= parsed.truncated;
        transaction_paths.push(directory);
    }

    let mut work_units = BTreeSet::new();
    let mut miss_units = BTreeSet::new();
    let mut hit_units = BTreeSet::new();
    let mut seen_units = BTreeSet::new();
    for row in &worker.reuse {
        let Some(identity) = ReuseIdentity::from_row(row) else {
            continue;
        };
        seen_units.insert(identity.clone());
        match row["decision"].as_str() {
            Some("work") => {
                work_units.insert(identity);
            }
            Some("miss") => {
                miss_units.insert(identity);
            }
            Some("hit") => {
                hit_units.insert(identity);
            }
            _ => {}
        }
    }
    let module_count_units: BTreeSet<(String, String)> =
        worker.module_counts.iter().cloned().collect();

    let compiled_off_checkout = events
        .installs
        .iter()
        .filter(|fields| fields["compiled_off_checkout"].as_bool() == Some(true))
        .count();
    let install_compile_ms = events
        .installs
        .iter()
        .filter_map(|fields| fields["compile_install_ms"].as_u64())
        .sum();
    let install_resolve_imports_ms = events
        .installs
        .iter()
        .filter_map(|fields| fields["resolve_imports_ms"].as_u64())
        .sum();
    let install_link_ms = events
        .installs
        .iter()
        .filter_map(|fields| fields["link_ms"].as_u64())
        .sum();
    let install_imports = events
        .installs
        .iter()
        .filter_map(|fields| fields["imports"].as_u64())
        .collect::<Vec<_>>();

    // Prefer the actor summary when the route emits it (the service route);
    // otherwise use the always-on compile summary this route does emit, with
    // the raw compiler stderr line as a fallback when tracing capture is
    // unavailable.
    let actor_compile_ms = (!events.actor_call_timing.is_empty()).then(|| {
        events
            .actor_call_timing
            .iter()
            .filter_map(|fields| fields["compile_ms"].as_u64())
            .sum()
    });
    let actor_compile_count = (!events.actor_call_timing.is_empty()).then(|| {
        events
            .actor_call_timing
            .iter()
            .filter_map(|fields| fields["compile_count"].as_u64())
            .sum()
    });
    let actor_checkout_wait = (!events.actor_call_timing.is_empty()).then(|| {
        events
            .actor_call_timing
            .iter()
            .filter_map(|fields| fields["checkout_wait_ms"].as_u64())
            .sum()
    });
    let actor_preparation_wait = (!events.actor_call_timing.is_empty()).then(|| {
        events
            .actor_call_timing
            .iter()
            .filter_map(|fields| fields["preparation_wait_ms"].as_u64())
            .sum()
    });
    let actor_publication_attempts = (!events.actor_call_timing.is_empty()).then(|| {
        events
            .actor_call_timing
            .iter()
            .filter_map(|fields| fields["publication_attempts"].as_u64())
            .sum()
    });
    let actor_semantic_rebases = (!events.actor_call_timing.is_empty()).then(|| {
        events
            .actor_call_timing
            .iter()
            .filter_map(|fields| fields["semantic_rebases"].as_u64())
            .sum()
    });

    let (compile_ms, compile_ms_source) = if let Some(actor) = actor_compile_ms {
        (Some(actor), "exomonad_actor::call_timing.compile_ms")
    } else if !events.compile_summary_wall_ms.is_empty() {
        (
            Some(events.compile_summary_wall_ms.iter().sum()),
            "tidepool_runtime::compile.wall_ms",
        )
    } else if !worker.compile_summaries.is_empty() {
        (
            Some(worker.compile_summaries.iter().map(|row| row.wall_ms).sum()),
            "tidepool-compile-summary.wall_ms",
        )
    } else {
        (None, "unavailable")
    };
    let compile_count = actor_compile_count.or_else(|| {
        if !events.compile_summary_wall_ms.is_empty() {
            Some(
                u64::try_from(events.compile_summary_wall_ms.len())
                    .expect("event count fits u64"),
            )
        } else if !worker.compile_summaries.is_empty() {
            Some(
                u64::try_from(worker.compile_summaries.len())
                    .expect("summary count fits u64"),
            )
        } else {
            None
        }
    });

    let (work_units, miss_units, hit_units, units_source) =
        if worker.reuse.is_empty() && !module_count_units.is_empty() {
            // The fallback source has no decision, so report its modules as the
            // observed work without claiming a reuse decision.
            let fallback = module_count_units
                .iter()
                .map(|(unit, module)| ReuseIdentity {
                    stage: "raw_projection_output".to_owned(),
                    unit: unit.clone(),
                    module: module.clone(),
                    version_kind: "unavailable".to_owned(),
                    version: "unavailable".to_owned(),
                })
                .collect::<BTreeSet<_>>();
            (
                fallback.clone(),
                BTreeSet::new(),
                BTreeSet::new(),
                "tidepool-module-counts",
            )
        } else if work_units.is_empty() && !miss_units.is_empty() {
            // A lookup miss without a later work row still identifies the unit
            // whose compilation this cell caused.
            (
                miss_units.clone(),
                miss_units,
                hit_units,
                "tidepool-reuse(miss)",
            )
        } else if !work_units.is_empty() {
            (work_units, miss_units, hit_units, "tidepool-reuse")
        } else if !hit_units.is_empty() {
            (work_units, miss_units, hit_units, "tidepool-reuse(hit-only)")
        } else {
            (
                work_units,
                miss_units,
                hit_units,
                "unavailable",
            )
        };

    CellRow {
        cell,
        label,
        source,
        compile_wall_ms,
        run_wall_ms,
        run_error,
        compile_ms,
        compile_ms_source,
        compile_count,
        compile_summary_modules: events.compile_summary_modules,
        stderr_compile_summaries: worker.compile_summaries,
        prepared_compile_ms: events.prepared_compile_totals_ms.iter().sum(),
        prepared_compile_count: u64::try_from(events.prepared_compile_totals_ms.len())
            .expect("event count fits u64"),
        prepared_compile_totals_ms: events.prepared_compile_totals_ms,
        prepared_functions_defined: events.prepared_functions_defined,
        prepared_code_bytes: events.prepared_code_bytes,
        install_rows: events.installs.len(),
        install_compile_ms,
        install_resolve_imports_ms,
        install_link_ms,
        install_imports,
        compiled_off_checkout,
        image_decisions: events.image_decisions,
        image_live_hits: events.image_live_hits,
        image_group_units: events.image_group_units,
        actor_call_timing: events.actor_call_timing,
        checkout_wait_ms: actor_checkout_wait,
        preparation_wait_ms: actor_preparation_wait,
        publication_attempts: actor_publication_attempts,
        semantic_rebases: actor_semantic_rebases,
        work_units,
        miss_units,
        hit_units,
        seen_units,
        module_count_units,
        reuse_rows: worker.reuse.len(),
        reuse_decisions: worker.decisions,
        units_source,
        codegen_functions: after_codegen.0.saturating_sub(before_codegen.0),
        codegen_code_bytes: after_codegen.1.saturating_sub(before_codegen.1),
        transaction_dirs: transaction_paths,
        compiler_stderr_bytes: worker.bytes,
        compiler_stderr_truncated: stderr_truncated || worker.truncated,
        transactions_truncated,
        event_log_truncated: log.truncated(),
    }
}

/// Service route placeholder. It must fail loudly rather than silently reuse
/// the finite route: the whole point of the selector is that the baseline and
/// the warm-compiler result are distinguishable.
fn drive_service_cell(
    _notebook: &mut Notebook,
    _log: &CapturedLog,
    _diagnostics: &DiagnosticRoot,
    _cell: u8,
    _label: &'static str,
    _source: &'static str,
) -> CellRow {
    panic!(
        "CellRoute::Service is not available at this revision. R1/S1 must publish the \
         service-cell entry point; implement this arm with that owner, not the finite route."
    )
}

fn route_name(route: CellRoute) -> &'static str {
    match route {
        CellRoute::FiniteResident => "finite-resident-old-route",
        CellRoute::Service => "service-cell-r1s1",
    }
}

fn output_path(diagnostics: &DiagnosticRoot) -> PathBuf {
    if let Some(path) = std::env::var_os("TIDEPOOL_WARM_TIMING_OUT") {
        return PathBuf::from(path);
    }
    diagnostics.root.join("warm-timing.jsonl")
}

fn source_revision() -> String {
    std::env::var("TIDEPOOL_SOURCE_REVISION")
        .unwrap_or_else(|_| RECORDED_SOURCE_REVISION.to_owned())
}

fn write_jsonl(path: &Path, rows: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("warm timing output directory");
    }
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(row).expect("serialize warm timing row"));
        text.push('\n');
    }
    let mut file = std::fs::File::create(path).expect("create warm timing JSONL");
    file.write_all(text.as_bytes())
        .expect("write warm timing JSONL");
}

fn unavailable_fields(row: &CellRow) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if row.checkout_wait_ms.is_none() {
        fields.push("checkout_wait_ms");
    }
    if row.preparation_wait_ms.is_none() {
        fields.push("preparation_wait_ms");
    }
    if row.publication_attempts.is_none() {
        fields.push("publication_attempts");
    }
    if row.semantic_rebases.is_none() {
        fields.push("semantic_rebases");
    }
    fields
}

fn delta(cell_one: Option<u64>, cell_two: Option<u64>) -> Value {
    match (cell_one, cell_two) {
        (Some(first), Some(second)) => json!({
            "cell1": first,
            "cell2": second,
            "delta": i128::from(second) - i128::from(first),
        }),
        _ => Value::Null,
    }
}

/// Two ordinary cells in one resident session.
///
/// The old route reports no cross-cell compiler reuse of a cell's own unit:
/// each cell compiles its own generated module, and the earlier binding is
/// injected as an already compiled session value rather than recompiled. That
/// is the baseline this case records; it is not a warm-compiler result.
#[test]
#[serial_test::serial]
fn two_ordinary_cells_report_no_reuse_baseline() {
    ensure_timing_environment();
    let diagnostics = DiagnosticRoot::discover();
    let log = install_event_capture();
    let mut notebook = Notebook::new();
    let revision = source_revision();
    let route = route_name(CELL_ROUTE);

    let cell_one = drive(
        CELL_ROUTE,
        &mut notebook,
        &log,
        &diagnostics,
        1,
        "cell-one",
        "warmCellOne <- pure (1 :: Int)",
    );
    let cell_two = drive(
        CELL_ROUTE,
        &mut notebook,
        &log,
        &diagnostics,
        2,
        "cell-two",
        "warmCellTwo <- pure (2 :: Int)",
    );

    // A cell-2 compiler hit on a unit cell 1 compiled is cross-cell reuse.
    // Exact identities include the stage and the owner version, so equal
    // `(unit, module)` names with different source fingerprints do not alias.
    let cross_cell_reuse_units = cell_two
        .hit_units
        .intersection(&cell_one.work_units)
        .cloned()
        .collect::<BTreeSet<_>>();
    // The same question without stage alignment: any cell-2 hit whose owner
    // version cell 1 compiled.
    let cell_one_work_owners = owner_set(&cell_one.work_units);
    let cross_cell_owner_reuse = cell_two
        .hit_units
        .iter()
        .filter(|identity| cell_one_work_owners.contains(&identity.owner_key()))
        .cloned()
        .collect::<BTreeSet<_>>();
    // Units cell 2 freshly compiled that cell 1 had also freshly compiled.
    let cross_cell_recompiled_units = cell_two
        .work_units
        .intersection(&cell_one.work_units)
        .cloned()
        .collect::<BTreeSet<_>>();

    let cross_cell_reuse_count = cross_cell_reuse_units.len();
    let cross_cell_owner_reuse_count = cross_cell_owner_reuse.len();
    let cross_cell_recompiled_count = cross_cell_recompiled_units.len();

    let summary = merge_json(vec![
        json!({
            "kind": "summary",
            "schema": 1,
            "route": route,
            "source_revision": revision,
            "baseline_label": "old-route-no-reuse",
            "warm_compiler_result": false,
            "route_switch_instruction": ROUTE_SWITCH_INSTRUCTION,
        }),
        json!({
            "cell1_compile_ms": cell_one.compile_ms,
            "cell2_compile_ms": cell_two.compile_ms,
            "compile_ms_delta": delta(cell_one.compile_ms, cell_two.compile_ms),
            "cell1_compile_count": cell_one.compile_count,
            "cell2_compile_count": cell_two.compile_count,
            "compile_count_delta": delta(cell_one.compile_count, cell_two.compile_count),
            "cell1_prepared_compile_ms": cell_one.prepared_compile_ms,
            "cell2_prepared_compile_ms": cell_two.prepared_compile_ms,
            "prepared_compile_ms_delta": delta(Some(cell_one.prepared_compile_ms), Some(cell_two.prepared_compile_ms)),
            "cell1_run_error": cell_one.run_error,
            "cell2_run_error": cell_two.run_error,
            "cell1_install_compile_ms": cell_one.install_compile_ms,
            "cell2_install_compile_ms": cell_two.install_compile_ms,
        }),
        json!({
            "cell1_units_compiled": cell_one.work_units.len(),
            "cell2_units_compiled": cell_two.work_units.len(),
            "units_compiled_delta": (cell_two.work_units.len() as i128) - (cell_one.work_units.len() as i128),
            "cell1_units_hit": cell_one.hit_units.len(),
            "cell2_units_hit": cell_two.hit_units.len(),
            "cell1_units_seen": cell_one.seen_units.len(),
            "cell2_units_seen": cell_two.seen_units.len(),
            "cell1_units_source": cell_one.units_source,
            "cell2_units_source": cell_two.units_source,
        }),
        json!({
            "cross_cell_reuse_count": cross_cell_reuse_count,
            "cross_cell_reuse_units": reuse_units_json(&cross_cell_reuse_units),
            "cross_cell_reuse_definition": "distinct (stage, unit, module, version_kind, version) identities served as a compiler reuse hit (`decision=hit`) in cell 2 that cell 1 freshly compiled (`decision=work`)",
            "cross_cell_owner_reuse_count": cross_cell_owner_reuse_count,
            "cross_cell_owner_reuse_units": reuse_units_json(&cross_cell_owner_reuse),
            "cross_cell_owner_reuse_definition": "same as cross_cell_reuse_count but ignores the compiler stage (unit, module, version_kind, version must still match)",
            "cross_cell_recompiled_count": cross_cell_recompiled_count,
            "cross_cell_recompiled_units": reuse_units_json(&cross_cell_recompiled_units),
            "cross_cell_recompiled_definition": "distinct (stage, unit, module, version_kind, version) identities freshly compiled (`decision=work`) in both cell 1 and cell 2",
        }),
        json!({
            "cell1_checkout_wait_ms": cell_one.checkout_wait_ms,
            "cell2_checkout_wait_ms": cell_two.checkout_wait_ms,
            "cell1_preparation_wait_ms": cell_one.preparation_wait_ms,
            "cell2_preparation_wait_ms": cell_two.preparation_wait_ms,
            "cell1_publication_attempts": cell_one.publication_attempts,
            "cell2_publication_attempts": cell_two.publication_attempts,
            "cell1_semantic_rebases": cell_one.semantic_rebases,
            "cell2_semantic_rebases": cell_two.semantic_rebases,
            "cell1_unavailable_fields": unavailable_fields(&cell_one),
            "cell2_unavailable_fields": unavailable_fields(&cell_two),
        }),
        json!({
            "missing_production_fields": [
                "actor checkout_wait_ms/preparation_wait_ms/publication_attempts/semantic_rebases (no CallScope on the finite route)",
                "cell or execution identity on tidepool_codegen::prepared_compile and tidepool_runtime::prepared_install rows (cell boundaries are taken from the synchronous harness)",
                "compiler-side tidepool-reuse lines are retained only as transaction stderr; run_turn does not forward them as tracing events",
                "no production unit/module identity on the actor-equivalent compile summary (module identities come from tidepool-reuse or TIDEPOOL_CODEGEN_DETAIL definition rows)",
            ],
            "jsonl_path": output_path(&diagnostics).display().to_string(),
        }),
    ]);

    let path = output_path(&diagnostics);
    write_jsonl(
        &path,
        &[
            cell_one.to_json(route, &revision),
            cell_two.to_json(route, &revision),
            summary.clone(),
        ],
    );

    println!("[warm-timing] route={route} revision={revision}");
    println!(
        "[warm-timing] cell1 compile_ms={:?} compile_count={:?} units_compiled={} units_hit={} units_seen={} source={}",
        cell_one.compile_ms,
        cell_one.compile_count,
        cell_one.work_units.len(),
        cell_one.hit_units.len(),
        cell_one.seen_units.len(),
        cell_one.units_source
    );
    println!(
        "[warm-timing] cell2 compile_ms={:?} compile_count={:?} units_compiled={} units_hit={} units_seen={} source={}",
        cell_two.compile_ms,
        cell_two.compile_count,
        cell_two.work_units.len(),
        cell_two.hit_units.len(),
        cell_two.seen_units.len(),
        cell_two.units_source
    );
    println!(
        "[warm-timing] cross_cell_reuse_count={cross_cell_reuse_count} \
         cross_cell_owner_reuse_count={cross_cell_owner_reuse_count} \
         cross_cell_recompiled_count={cross_cell_recompiled_count}"
    );
    println!(
        "[warm-timing] summary {}",
        serde_json::to_string(&summary).expect("serialize summary")
    );
    println!("[warm-timing] jsonl {}", path.display());

    assert!(
        cell_one.compile_count.unwrap_or(0) > 0,
        "cell 1 emitted no compile summary; evidence is incomplete"
    );
    assert!(
        cell_two.compile_count.unwrap_or(0) > 0,
        "cell 2 emitted no compile summary; evidence is incomplete"
    );
    // `prepared_compile` is emitted by the install half. When that half is
    // unavailable on this revision the cell rows report `0`, not a guessed
    // number; the compile evidence above is still production-attributed.
    assert!(
        !cell_one.work_units.is_empty() && !cell_two.work_units.is_empty(),
        "both cells must report at least one compiled unit identity"
    );
}
