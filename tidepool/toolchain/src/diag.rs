//! Compiler-worker response policy and human-facing diagnostic rendering.
//!
//! The schema itself lives in `tidepool-extract-report` so the procedural
//! macro and runtime compile paths cannot drift. This module owns the process
//! status/outcome contract and source-aware rendering policy.

use crate::CompileError;

pub use tidepool_extract_report::{
    DiagnosticSeverity, DiagnosticSpan as DiagSpan, ExtractDiagnostic as ExtractDiag,
    ExtractOutcome, ExtractReport,
};

/// Parse the extract binary's stdout as the fixed-shape diagnostics report.
/// Fails LOUD (never falls back to reading stderr) on malformed JSON or an
/// unexpected `version` — the error names the likely cause (a stale deployed
/// `tidepool-extract-bin` vs. this server's expectations) and includes a short
/// stderr tail for debugging.
pub fn parse_extract_report(stdout: &[u8], stderr: &[u8]) -> Result<ExtractReport, String> {
    let text = String::from_utf8_lossy(stdout);
    let malformed_err = |e: &dyn std::fmt::Display| {
        format!(
            "extract stdout did not parse as the diagnostics report ({e}) — likely a stale \
             deployed `tidepool-extract-bin` predating the structured-diagnostics contract; \
             rebuild/redeploy it. stdout: {}\nstderr tail: {}",
            truncate_tail(&text, 500),
            truncate_tail(&String::from_utf8_lossy(stderr), 500)
        )
    };
    tidepool_extract_report::decode_report(stdout).map_err(|e| malformed_err(&e))
}

/// Decode and validate the response from one completed extractor process.
///
/// Every accepted request emits exactly one report. The process status and
/// typed outcome must agree; disagreement is a deployed protocol mismatch,
/// never a source error. This is the sole conversion from worker outcomes to
/// [`CompileError`].
pub fn decode_extract_result(
    process_succeeded: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<ExtractReport, CompileError> {
    let report =
        parse_extract_report(stdout, stderr).map_err(CompileError::MalformedDiagnostics)?;
    match (process_succeeded, report.outcome) {
        (true, ExtractOutcome::Success) => Ok(report),
        (false, ExtractOutcome::InputRejected) => {
            Err(CompileError::InputRejected(report.diagnostics))
        }
        (false, ExtractOutcome::SourceFailure) => {
            Err(CompileError::Diagnostics(report.diagnostics))
        }
        (false, ExtractOutcome::WorkerFailure) => {
            let mut diagnostics = report.diagnostics;
            let stderr = String::from_utf8_lossy(stderr);
            let stderr = stderr.trim();
            if !stderr.is_empty() {
                diagnostics.push(ExtractDiag {
                    span: None,
                    severity: DiagnosticSeverity::Error,
                    message: format!(
                        "compiler worker stderr:\n{}",
                        truncate_head(&without_machine_lines(stderr), 4_000)
                    ),
                });
            }
            Err(CompileError::WorkerFailure(diagnostics))
        }
        (succeeded, outcome) => Err(CompileError::MalformedDiagnostics(format!(
            "extract process/report disagreement: process {} but report outcome was {outcome:?}",
            if succeeded { "succeeded" } else { "failed" }
        ))),
    }
}

fn truncate_tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Byte-slicing a str must land on a char boundary; walk forward from the
    // naive cut point to the next one so a multi-byte character never splits.
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Machine-readable measurements belong in the daemon's structured log, not
/// in a source diagnostic. Filter before applying the bounded diagnostic
/// budget so a long measurement run cannot push the GHC error out of view.
/// The prefixes are the single list in
/// `tidepool_extract_cmd::diagnostics`, shared with the daemon's own
/// transaction log.
fn without_machine_lines(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !tidepool_extract_cmd::diagnostics::is_machine_stderr_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Which generated session-library warnings survive presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedWarningPolicy<'a> {
    KeepAll,
    DropAll,
    /// Keep only the exact compiler-reported path, without suffix matching.
    KeepOnly(&'a str),
}

/// Options controlling how [`render_diagnostics`] partitions/remaps/labels a
/// diagnostics report for display.
pub struct RenderOpts<'a> {
    /// Suffix the generated file's path must end in to count as "the target
    /// module" for partitioning/remap purposes (e.g. `"Expr.hs"` for eval/repl
    /// wrapper compiles, or the candidate's own relative `.hs` path for decl
    /// validation). A diagnostic whose span's file does not match this anchor
    /// is left with its raw file:line:col (foreign path — e.g. a GHC panic
    /// backtrace into `compiler/GHC/...`).
    pub anchor: &'a str,
    /// Display name to show instead of the generated path (e.g. `"<item>"`,
    /// `"<decl>"`).
    pub label: &'a str,
    /// `Some(ranges)` — 1-based inclusive line ranges of the user's OWN
    /// authored text within the anchor file: the primary code/expr block
    /// plus (when present) the `helpers` and `imports` params — each is its
    /// own, generally non-contiguous, range (`imports` lands in the
    /// preamble; `helpers` sits between the `-- [user]` marker and the code;
    /// see [`extract_user_code_ranges`]). A diagnostic anchored in-file but
    /// OUTSIDE every range is wrapper-scaffold fallout: dropped, counted in
    /// one footer line, UNLESS dropping would leave zero survivors (never
    /// render "no diagnostics" when there is at least one) — in which case
    /// the batch is ALL wrapper-origin, and [`render_diagnostics`] renders
    /// one synthetic, plain-language framing line PLUS the cleaned
    /// underlying diagnostic(s) (see that function's doc) — never outright
    /// suppression. `None` — no such partition (e.g. decl-candidate
    /// validation, which never wraps user code in scaffold).
    pub user_lines: Option<&'a [(usize, usize)]>,
    /// Subtract from a diagnostic's line before display (generated preamble
    /// length). A line already `<=` this offset (preamble region) is shown
    /// raw, unshifted.
    pub line_offset: usize,
    /// Subtract from a diagnostic's column before display (the wrapper's
    /// indent). A column already `<=` this indent is shown raw, unshifted.
    pub col_indent: usize,
    /// Filter warnings whose file contains `Tidepool/Session/Lib/G`.
    /// Session-library generation warnings from dependencies would otherwise
    /// ride every later item's errors. Errors and unlocated warnings survive.
    pub generated_warnings: GeneratedWarningPolicy<'a>,
    /// The assembled module source text (or candidate source), for building
    /// a gutter/caret excerpt by indexing into it by line number.
    pub source: &'a str,
}

/// Severity as the rendered header spells it. A projection of the wire
/// [`DiagnosticSeverity`] into the model-facing schema; the wire crate stays
/// dependency-light and owns no serialization direction but decoding.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticLevel {
    Error,
    Warning,
}

impl From<DiagnosticSeverity> for DiagnosticLevel {
    fn from(severity: DiagnosticSeverity) -> Self {
        match severity {
            DiagnosticSeverity::Error => Self::Error,
            DiagnosticSeverity::Warning => Self::Warning,
        }
    }
}

impl std::fmt::Display for DiagnosticLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Warning => f.write_str("warning"),
        }
    }
}

/// Where a rendered diagnostic block says it points, as data rather than as a
/// `label:line:col` prefix the reader has to parse back out. Exactly the
/// coordinate the header printed, in exactly the space the header printed it:
/// never the raw generated-module line when the header showed a remapped one,
/// and never a remapped line when the header showed the raw one.
///
/// [`Unlocated`](Self::Unlocated) is the case that keeps an unrecognised
/// diagnostic representable — GHC supplied no usable span, or the worker died
/// before one existed. The diagnostic still carries its whole message; only
/// the pointer is missing. Nothing here requires a diagnostic to be
/// classified beyond "did the header have a coordinate, and was it in the
/// submitted text's own space".
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiagnosticLocation {
    /// A coordinate in the submitted text's own space. `label` is the display
    /// name the header printed (`<cell>`, `<item>`, `<cell item 3>`), and the
    /// line/column are already offset-corrected the same way the header
    /// corrected them.
    #[serde(rename_all = "camelCase")]
    Authored {
        label: String,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    },
    /// The header printed a raw `file:line:col` because the span is not in
    /// the submitted text: the generated preamble, a library module, a
    /// compiler backtrace. Raw coordinates, unremapped, as rendered.
    #[serde(rename_all = "camelCase")]
    Foreign {
        file: String,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    },
    /// No span. The message is intact; there is nothing to point at.
    Unlocated,
}

/// One rendered diagnostic block, kept as data: the same severity, the same
/// message body, and the same coordinate its text shows. Produced alongside
/// the rendered text by [`render_diagnostics_structured`] so the two can
/// never disagree, and so a reader never has to recover by regex what the
/// renderer already knew.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct StructuredDiagnostic {
    pub severity: DiagnosticLevel,
    pub location: DiagnosticLocation,
    /// The message body as the block prints it — scaffold-relevant bindings
    /// already scrubbed — without the four-space block indent.
    pub message: String,
}

/// The rendered text and the same diagnostics as data. One pass, one set of
/// partition/remap decisions, two views of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedDiagnostics {
    /// Byte-for-byte what [`render_diagnostics`] returns.
    pub text: String,
    /// One entry per surviving original diagnostic, in presentation order.
    /// Repeated warning text is compacted in `text`, while every warning
    /// occurrence remains here. Diagnostics
    /// the render policy dropped (foreign gen warnings, the wrapper-fallout
    /// partition's suppressed tail) are absent here exactly as they are
    /// absent from the text; the fallout footer line, being a count and not a
    /// diagnostic, has no entry.
    pub diagnostics: Vec<StructuredDiagnostic>,
}

/// Render surviving diagnostics (after the fallout partition + foreign-gen
/// warning drop) into human-facing text: one block per error or distinct
/// warning (a
/// `{label}:{line}:{col}[-{end_col}]: {severity}:` header, the message body,
/// and a gutter source excerpt when the span is single-line, the line exists
/// in `opts.source`, and that line isn't itself wrapper-template scaffold),
/// joined by a blank line. Repeated warnings include their occurrence count;
/// structured diagnostics retain every surviving occurrence. A wrapper-fallout
/// footer is appended when the
/// `user_lines` partition dropped anything — UNLESS every surviving
/// diagnostic was reported only on generated wrapper lines, in which case
/// this renders one synthetic, plain-language framing line PLUS the cleaned underlying
/// diagnostic(s) (raw `file:line:col` header, no snippet — the model never
/// wrote those lines, so a coordinate remapped against user code would be
/// misleading) instead of the ordinary per-diagnostic blocks. The recipient
/// always gets SOMETHING actionable: never bare suppression.
#[must_use]
pub fn render_diagnostics(diags: &[ExtractDiag], opts: &RenderOpts<'_>) -> String {
    render_diagnostics_structured(diags, opts).text
}

/// [`render_diagnostics`]'s text plus the same diagnostics as data.
///
/// The text is produced by the identical code path — [`render_diagnostics`]
/// is this function's `.text` — so the rendered form cannot drift from the
/// structured one. Ordering and warning grouping are presentation only;
/// the compiler's diagnostic collection is never mutated.
#[must_use]
pub fn render_diagnostics_structured(
    diags: &[ExtractDiag],
    opts: &RenderOpts<'_>,
) -> RenderedDiagnostics {
    // Foreign-gen-warning drop: no "never empty" guard — dropping a warning
    // can legitimately leave zero total diagnostics.
    let after_gen_drop: Vec<&ExtractDiag> = diags
        .iter()
        .filter(|d| !is_dropped_foreign_gen_warning(d, opts.generated_warnings))
        .collect();

    // User-lines fallout partition.
    let (mut kept, mut fallout): (Vec<&ExtractDiag>, Vec<&ExtractDiag>) = match opts.user_lines {
        Some(ranges) => {
            let mut kept = Vec::new();
            let mut fallout = Vec::new();
            for d in &after_gen_drop {
                if is_in_anchor_file(d, opts.anchor) && !span_in_any_range(d, ranges) {
                    fallout.push(*d);
                } else {
                    kept.push(*d);
                }
            }
            (kept, fallout)
        }
        None => (after_gen_drop.clone(), Vec::new()),
    };
    let priority = |d: &&ExtractDiag| {
        (
            d.severity != DiagnosticSeverity::Error,
            !d.span
                .as_ref()
                .and_then(|span| display_coord(span, opts))
                .is_some_and(|coord| coord.in_user_region),
        )
    };
    kept.sort_by_key(priority);
    fallout.sort_by_key(priority);

    // Every surviving diagnostic points at generated wrapper text. That span
    // does not identify the root cause, so describe only the known location
    // and retain the underlying diagnostics for diagnosis.
    if kept.is_empty() && !fallout.is_empty() {
        let mut out = format!(
            "GHC reported {} only on generated workbench wrapper lines. Those \
             locations do not identify whether the root cause is in the authored input or \
             generated framing. Underlying GHC diagnostic(s):",
            severity_counts(&fallout)
        );
        let (blocks, structured) = diagnostic_blocks(&fallout, opts, true);
        for block in blocks {
            out.push_str("\n\n");
            out.push_str(&block);
        }
        return RenderedDiagnostics {
            text: out,
            diagnostics: structured,
        };
    }

    let (mut blocks, structured) = diagnostic_blocks(&kept, opts, false);
    if !fallout.is_empty() {
        blocks.push(format!(
            "(Further diagnostics on generated workbench wrapper lines: {}; locations suppressed)",
            severity_counts(&fallout)
        ));
    }
    RenderedDiagnostics {
        text: blocks.join("\n\n"),
        diagnostics: structured,
    }
}

fn diagnostic_blocks(
    diags: &[&ExtractDiag],
    opts: &RenderOpts<'_>,
    raw: bool,
) -> (Vec<String>, Vec<StructuredDiagnostic>) {
    let mut blocks = Vec::new();
    let mut structured = Vec::new();
    let mut warning_blocks: Vec<(StructuredDiagnostic, usize, usize)> = Vec::new();
    for d in diags {
        let projection = project_diagnostic(d, opts, raw);
        let diagnostic = &projection.structured;
        if diagnostic.severity == DiagnosticLevel::Warning {
            if let Some((_, _, count)) = warning_blocks
                .iter_mut()
                .find(|(previous, _, _)| previous == diagnostic)
            {
                *count += 1;
                structured.push(projection.structured);
                continue;
            }
            warning_blocks.push((diagnostic.clone(), blocks.len(), 1));
        }
        blocks.push(projection.render(opts.source));
        structured.push(projection.structured);
    }
    for (_, index, count) in warning_blocks {
        if count > 1 {
            blocks[index].push_str(&format!("\n(repeated {count} times)"));
        }
    }
    (blocks, structured)
}

fn severity_counts(diags: &[&ExtractDiag]) -> String {
    let errors = diags
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .count();
    let warnings = diags.len() - errors;
    match (errors, warnings) {
        (_, 0) => format!("{errors} error(s)"),
        (0, _) => format!("{warnings} warning(s)"),
        _ => format!("{errors} error(s), {warnings} warning(s)"),
    }
}

fn is_dropped_foreign_gen_warning(d: &ExtractDiag, policy: GeneratedWarningPolicy<'_>) -> bool {
    let Some(span) = &d.span else {
        return false;
    };
    d.severity == DiagnosticSeverity::Warning
        && span.file.contains("Tidepool/Session/Lib/G")
        && match policy {
            GeneratedWarningPolicy::KeepAll => false,
            GeneratedWarningPolicy::DropAll => true,
            GeneratedWarningPolicy::KeepOnly(keep) => span.file != keep,
        }
}

fn is_in_anchor_file(d: &ExtractDiag, anchor: &str) -> bool {
    match &d.span {
        Some(span) => path_ends_with_anchor(&span.file, anchor),
        None => false,
    }
}

/// The anchor must be preceded by a path separator, or the whole path must
/// simply equal it — never an embedded suffix (`SomeExpr.hs` must not match
/// anchor `Expr.hs`).
fn path_ends_with_anchor(path: &str, anchor: &str) -> bool {
    if path == anchor {
        return true;
    }
    path.ends_with(anchor)
        && path.len() > anchor.len()
        && matches!(path.as_bytes()[path.len() - anchor.len() - 1], b'/' | b'\\')
}

/// Membership across every user-authored range at once — a diagnostic falls
/// out of scope only when it matches NONE of them.
fn span_in_any_range(d: &ExtractDiag, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|&(start, end)| span_in_range(d, start, end))
}

fn span_in_range(d: &ExtractDiag, start: usize, end: usize) -> bool {
    match &d.span {
        Some(span) => {
            let l = span.start_line as usize;
            l >= start && l <= end
        }
        None => false,
    }
}

/// Anchor-relative display coordinates. Only authored lines have their
/// wrapper indentation removed; generated preamble coordinates stay raw.
struct DisplayCoord<'a> {
    label: &'a str,
    line: usize,
    in_user_region: bool,
}

fn display_coord<'a>(span: &DiagSpan, opts: &RenderOpts<'a>) -> Option<DisplayCoord<'a>> {
    if !path_ends_with_anchor(&span.file, opts.anchor) {
        return None;
    }
    let line = span.start_line as usize;
    let in_user_region = line > opts.line_offset;
    Some(if in_user_region {
        DisplayCoord {
            label: opts.label,
            line: line - opts.line_offset,
            in_user_region: true,
        }
    } else {
        DisplayCoord {
            label: opts.anchor,
            line,
            in_user_region: false,
        }
    })
}

/// Wrapper-template scaffold text that must never be shown as if it were the
/// user's own source — e.g. a diagnostic whose GHC-reported span numerically
/// falls inside the user-code line range but whose CONTENT is nonetheless one
/// of the generated wrapper lines (`__anchor`'s signature, the `__user = let
/// {`/`} in __b` bracket lines, the `paginateResult` render call). Detected
/// textually because these are the only line shapes a legitimate user could
/// never have authored (double-underscore-prefixed template binders, the
/// literal marker comment).
fn looks_like_wrapper_scaffold(line: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "__anchor",
        "__user = let {",
        "} in __b",
        "paginateResult",
        "[user-lines]",
    ];
    MARKERS.iter().any(|m| line.contains(m))
}

/// One presentation decision, shared by the text and structured output.
/// Raw compiler diagnostics remain borrowed and unchanged.
struct DiagnosticProjection {
    structured: StructuredDiagnostic,
    // Anchored single-line headers include an end column, even if stripping
    // indentation makes it equal to the start column. Foreign/raw headers do
    // not. Remapped coordinates alone cannot recover either distinction.
    show_end_col: bool,
    excerpt: Option<SourceExcerpt>,
}

struct SourceExcerpt {
    raw_line: usize,
    caret_start: usize,
    caret_width: usize,
}

fn project_diagnostic(d: &ExtractDiag, opts: &RenderOpts<'_>, raw: bool) -> DiagnosticProjection {
    // The all-wrapper appendix retains raw paths and coordinates, with no
    // excerpt: its spans identify generated framing rather than authored text.
    let coord = (!raw)
        .then(|| d.span.as_ref().and_then(|span| display_coord(span, opts)))
        .flatten();
    let location = match (&d.span, &coord) {
        (Some(span), Some(coord)) if coord.in_user_region => {
            let strip_col = |c: u32| {
                let c = c as usize;
                if c > opts.col_indent {
                    c - opts.col_indent
                } else {
                    c
                }
            };
            let strip_line = |l: u32| {
                let l = l as usize;
                if l > opts.line_offset {
                    l - opts.line_offset
                } else {
                    l
                }
            };
            DiagnosticLocation::Authored {
                label: coord.label.to_string(),
                start_line: coord.line as u32,
                start_col: strip_col(span.start_col) as u32,
                end_line: strip_line(span.end_line) as u32,
                end_col: strip_col(span.end_col) as u32,
            }
        }
        (Some(span), _) => DiagnosticLocation::Foreign {
            file: coord
                .as_ref()
                .map_or_else(|| span.file.clone(), |coord| coord.label.to_string()),
            start_line: span.start_line,
            start_col: span.start_col,
            end_line: span.end_line,
            end_col: span.end_col,
        },
        (None, _) => DiagnosticLocation::Unlocated,
    };
    let show_end_col = coord.is_some()
        && d.span
            .as_ref()
            .is_some_and(|span| span.start_line == span.end_line && span.end_col != span.start_col);
    let excerpt = d.span.as_ref().and_then(|span| {
        if !coord.as_ref().is_some_and(|coord| coord.in_user_region)
            || span.start_line != span.end_line
        {
            return None;
        }
        Some(SourceExcerpt {
            raw_line: (span.start_line as usize).checked_sub(1)?,
            caret_start: span.start_col.saturating_sub(1) as usize,
            caret_width: (span.end_col.saturating_sub(span.start_col)).max(1) as usize,
        })
    });
    DiagnosticProjection {
        structured: StructuredDiagnostic {
            severity: d.severity.into(),
            location,
            message: drop_scaffold_relevant_binds(&d.message, opts),
        },
        show_end_col,
        excerpt,
    }
}

impl DiagnosticProjection {
    fn render(&self, source: &str) -> String {
        let diagnostic = &self.structured;
        let mut out = match &diagnostic.location {
            DiagnosticLocation::Authored {
                label,
                start_line,
                start_col,
                end_col,
                ..
            }
            | DiagnosticLocation::Foreign {
                file: label,
                start_line,
                start_col,
                end_col,
                ..
            } => {
                if self.show_end_col {
                    format!(
                        "{label}:{start_line}:{start_col}-{end_col}: {}:",
                        diagnostic.severity
                    )
                } else {
                    format!("{label}:{start_line}:{start_col}: {}:", diagnostic.severity)
                }
            }
            DiagnosticLocation::Unlocated => format!("{}:", diagnostic.severity),
        };
        if let (Some(excerpt), DiagnosticLocation::Authored { start_line, .. }) =
            (&self.excerpt, &diagnostic.location)
        {
            if let Some(line) = source
                .lines()
                .nth(excerpt.raw_line)
                .filter(|line| !looks_like_wrapper_scaffold(line))
            {
                let gutter = start_line.to_string();
                out.push_str(&format!("\n{:>width$} |\n", "", width = gutter.len()));
                out.push_str(&format!("{gutter} | {line}\n"));
                out.push_str(&format!(
                    "{:>gwidth$} | {:>start$}{}",
                    "",
                    "",
                    "^".repeat(excerpt.caret_width),
                    gwidth = gutter.len(),
                    start = excerpt.caret_start
                ));
            }
        }
        for line in diagnostic.message.lines() {
            out.push_str("\n    ");
            out.push_str(line);
        }
        out
    }
}

/// Parse a `(bound at <file>:<line>:<col>)` suffix, searched from the right
/// since it's the LAST thing GHC prints for an entry (after any wrapped
/// continuation lines). `None` = unspannable (e.g. GHC printed `(bound at
/// <no location info>)` for an `UnhelpfulSpan`, or any other non-conforming
/// text).
fn parse_bound_at(entry_text: &str) -> Option<(&str, usize)> {
    let start = entry_text.rfind("(bound at ")?;
    let rest = entry_text[start + "(bound at ".len()..]
        .strip_suffix(')')?
        .trim_end();
    // "<file>:<line>:<col>" — file may itself contain ':' (e.g. a Windows
    // drive letter), so split from the RIGHT: col, then line, then file.
    let (file_and_line, col_s) = rest.rsplit_once(':')?;
    let (file, line_s) = file_and_line.rsplit_once(':')?;
    col_s.parse::<usize>().ok()?; // validated, not otherwise used
    Some((file, line_s.parse().ok()?))
}

/// Classify one "Relevant bindings include" entry (its text plus any wrapped
/// continuation lines, joined) as scaffold/droppable, by SPAN — mirrors
/// `span_in_range`/`is_in_anchor_file`'s diagnostic-level policy, applied to
/// the entry's own `(bound at ...)` location instead of the outer
/// diagnostic's `d.span`.
///
/// Fallback for an entry with no parseable span, or when `opts.user_lines` is
/// `None`: KEEP (never drop). Silently losing real user diagnostic info is
/// worse than occasional residual scaffold noise, which `maxRelevantBinds =
/// Just 0` already minimizes at the source.
fn entry_is_scaffold(entry_text: &str, opts: &RenderOpts<'_>) -> bool {
    let Some(ranges) = opts.user_lines else {
        return false;
    };
    let Some((file, line)) = parse_bound_at(entry_text) else {
        return false;
    };
    path_ends_with_anchor(file, opts.anchor)
        && !ranges
            .iter()
            .any(|&(start, end)| line >= start && line <= end)
}

/// Drop scaffold entries from a `Relevant bindings include` list INSIDE one
/// diagnostic's own message text, by comparing each entry's own `(bound at
/// file:line:col)` span against `opts.user_lines` — never by matching a
/// binder-name list against GHC's rendered wording. A diagnostic can
/// legitimately survive the whole-diagnostic span partition (its own span IS
/// on the user's own line) while GHC's own explanation for it still cites a
/// scaffold binder — e.g. an ambiguous type variable arising from `pure`
/// inside the `__b = pure (...)` wrapper binding, whose "Relevant bindings
/// include __b :: ..." entry is GHC's own text, not a separate droppable
/// diagnostic. If dropping scaffold entries empties the region, the header
/// and footer go too (an empty list is worse noise than no list). A
/// wrapped/multi-line entry's continuation lines are collected together with
/// its own `" :: "` line before classifying (GHC puts `(bound at ...)` on the
/// LAST physical line of a wrapped entry), so they're dropped or kept as one
/// group — never orphaned as leftover garbled text.
#[must_use]
fn drop_scaffold_relevant_binds(message: &str, opts: &RenderOpts<'_>) -> String {
    let lines: Vec<&str> = message.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if !trimmed.starts_with("Relevant bindings include") {
            out.push(line.to_string());
            i += 1;
            continue;
        }
        let header_indent = line.len() - trimmed.len();
        let mut region_out: Vec<String> = Vec::new();
        let mut all_dropped = true;
        let mut footer: Option<&str> = None;
        let mut j = i + 1;
        while j < lines.len() {
            let l = lines[j];
            let t = l.trim_start();
            if t.is_empty() {
                break;
            }
            let indent = l.len() - t.len();
            if indent <= header_indent {
                break;
            }
            if t.starts_with("(Some bindings suppressed") {
                footer = Some(l);
                j += 1;
                break;
            }
            if t.contains(" :: ") {
                // A genuine entry line — collect any following lines indented
                // deeper than it (wrapped-type continuations) before
                // classifying, so the trailing `(bound at ...)` is found no
                // matter which physical line it landed on.
                let entry_indent = indent;
                let mut entry_lines: Vec<&str> = vec![l];
                let mut k = j + 1;
                while k < lines.len() {
                    let cl = lines[k];
                    let ct = cl.trim_start();
                    if ct.is_empty() || ct.starts_with("(Some bindings suppressed") {
                        break;
                    }
                    let cindent = cl.len() - ct.len();
                    if cindent <= entry_indent {
                        break;
                    }
                    entry_lines.push(cl);
                    k += 1;
                }
                let joined_entry = entry_lines.join(" ");
                if entry_is_scaffold(&joined_entry, opts) {
                    // whole entry + its continuations dropped
                } else {
                    all_dropped = false;
                    region_out.extend(entry_lines.into_iter().map(str::to_string));
                }
                j = k;
                continue;
            }
            // Unexpected shape (no entry line seen yet in this region) — keep
            // verbatim rather than misclassify; GHC always emits "name :: ty"
            // as the first line of an entry in practice.
            all_dropped = false;
            region_out.push(l.to_string());
            j += 1;
        }
        if !all_dropped {
            out.push(line.to_string());
            out.extend(region_out);
            if let Some(f) = footer {
                out.push(f.to_string());
            }
        }
        i = j;
    }
    let mut joined = out.join("\n");
    if message.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Extract the 1-based inclusive `(start, end)` line range of the user's own
/// submitted code from a generated module's `-- [user-lines] <start>:<end>`
/// marker (emitted by `tidepool_mcp::eval_prep::template_haskell_impl` on the
/// `__user` binding's closing-bracket line). Absent for sources that don't
/// carry the marker (e.g. session-lib declaration compiles) — callers must not
/// fabricate a range when this returns `None`.
#[must_use]
pub fn extract_user_code_lines(source: &str) -> Option<(usize, usize)> {
    extract_one_marked_range(source, "-- [user-lines] ")
}

/// Every user-authored region marker a generated turn/eval module carries:
/// the primary code/expr block (`-- [user-lines] S:E`, from
/// [`extract_user_code_lines`]'s own needle) plus, when present, the
/// `helpers` param (`-- [user-helpers-lines] S:E`) and the `imports` param
/// (`-- [user-imports-lines] S:E`) — both emitted by
/// `tidepool_mcp::eval_prep::TurnTemplate::render` alongside the code marker.
/// These three regions are the WHOLE of what the model/user actually
/// authored; everything else in the module is generated wrapper scaffold. A
/// range is included only when its marker is actually present (an empty
/// `helpers`/`imports` param emits no marker at all — see `TurnTemplate`'s
/// doc) — `None` when NONE of the three markers are present (e.g. a
/// decl-candidate validation compile, which never wraps user code in
/// scaffold at all); callers must not fabricate ranges in that case.
#[must_use]
pub fn extract_user_code_ranges(source: &str) -> Option<Vec<(usize, usize)>> {
    const NEEDLES: [&str; 3] = [
        "-- [user-lines] ",
        "-- [user-helpers-lines] ",
        "-- [user-imports-lines] ",
    ];
    let ranges: Vec<(usize, usize)> = NEEDLES
        .iter()
        .filter_map(|needle| extract_one_marked_range(source, needle))
        .collect();
    (!ranges.is_empty()).then_some(ranges)
}

/// Parse a `<needle><start>:<end>` 1-based inclusive line range, where
/// `needle` is a `-- [...] ` marker comment. The real marker is emitted once,
/// AFTER the marked text (on its own trailing line) — `rfind` so user-
/// authored text that happens to contain this literal string can't be
/// mistaken for the real marker.
fn extract_one_marked_range(source: &str, needle: &str) -> Option<(usize, usize)> {
    let pos = source.rfind(needle)?;
    let rest = &source[pos + needle.len()..];
    let range: &str = rest.lines().next()?;
    let (start_s, end_s) = range.split_once(':')?;
    let start = start_s.trim().parse::<usize>().ok()?;
    let end = end_s.trim().parse::<usize>().ok()?;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(
        file: &str,
        sl: u32,
        sc: u32,
        el: u32,
        ec: u32,
        severity: DiagnosticSeverity,
        msg: &str,
    ) -> ExtractDiag {
        ExtractDiag {
            span: Some(DiagSpan {
                file: file.to_string(),
                start_line: sl,
                start_col: sc,
                end_line: el,
                end_col: ec,
            }),
            severity,
            message: msg.to_string(),
        }
    }

    // ---- worker report ----

    #[test]
    fn parse_clean_success_report_round_trip() {
        let stdout = br#"{"version":2,"outcome":"success","diagnostics":[]}"#;
        let report = parse_extract_report(stdout, b"").unwrap();
        assert_eq!(report.outcome, ExtractOutcome::Success);
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn parse_single_error_report() {
        let stdout = br#"{"version":2,"outcome":"source-failure","diagnostics":[{"span":{"file":"Bad.hs","startLine":3,"startCol":7,"endLine":3,"endCol":14},"severity":"error","message":"Variable not in scope: garbage"}]}"#;
        let report = parse_extract_report(stdout, b"").unwrap();
        assert_eq!(report.diagnostics.len(), 1);
        let d = &report.diagnostics[0];
        assert_eq!(d.severity, DiagnosticSeverity::Error);
        let span = d.span.as_ref().unwrap();
        assert_eq!(span.file, "Bad.hs");
        assert_eq!(span.start_line, 3);
    }

    #[test]
    fn parse_null_span_diagnostic() {
        let stdout =
            br#"{"version":2,"outcome":"worker-failure","diagnostics":[{"span":null,"severity":"error","message":"boom"}]}"#;
        let report = parse_extract_report(stdout, b"").unwrap();
        assert!(report.diagnostics[0].span.is_none());
    }

    #[test]
    fn malformed_stdout_produces_clear_error() {
        let err = parse_extract_report(b"not json", b"some stderr").unwrap_err();
        assert!(err.contains("did not parse"), "{err}");
        assert!(err.contains("stderr tail"), "{err}");
    }

    #[test]
    fn wrong_version_produces_clear_error() {
        let stdout = br#"{"version":99,"diagnostics":[]}"#;
        let err = parse_extract_report(stdout, b"").unwrap_err();
        assert!(err.contains("version 99"), "{err}");
        assert!(err.contains("expects 2"), "{err}");
    }

    #[test]
    fn process_status_and_typed_outcome_have_one_mapping() {
        let success = br#"{"version":2,"outcome":"success","diagnostics":[]}"#;
        assert!(decode_extract_result(true, success, b"").is_ok());
        assert!(matches!(
            decode_extract_result(false, success, b""),
            Err(CompileError::MalformedDiagnostics(_))
        ));

        let source = br#"{"version":2,"outcome":"source-failure","diagnostics":[]}"#;
        assert!(matches!(
            decode_extract_result(false, source, b""),
            Err(CompileError::Diagnostics(_))
        ));
        assert!(matches!(
            decode_extract_result(true, source, b""),
            Err(CompileError::MalformedDiagnostics(_))
        ));

        let worker = br#"{"version":2,"outcome":"worker-failure","diagnostics":[]}"#;
        assert!(matches!(
            decode_extract_result(false, worker, b""),
            Err(CompileError::WorkerFailure(_))
        ));
        assert!(matches!(
            decode_extract_result(true, worker, b""),
            Err(CompileError::MalformedDiagnostics(_))
        ));
    }

    #[test]
    fn worker_failure_retains_compiler_stderr() {
        let worker = br#"{"version":2,"outcome":"worker-failure","diagnostics":[]}"#;
        let Err(CompileError::WorkerFailure(diagnostics)) =
            decode_extract_result(false, worker, b"typecheckIface\nmodule X is not loaded\n")
        else {
            panic!("worker failure must stay typed");
        };
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert!(diagnostics[0]
            .message
            .contains("typecheckIface\nmodule X is not loaded"));
    }

    #[test]
    fn worker_failure_filters_machine_lines_before_bounding_stderr() {
        let worker = br#"{"version":2,"outcome":"worker-failure","diagnostics":[]}"#;
        let mut stderr = String::from("ghc: panic! (the 'impossible' happened)\n");
        for _ in 0..200 {
            stderr.push_str("tidepool-timing phase=cycle_modules_wall ms=14700\n");
            stderr.push_str("tidepool-checked module=Long.Dependency target=False\n");
        }
        stderr.push_str(&"tail noise\n".repeat(500));
        let Err(CompileError::WorkerFailure(diagnostics)) =
            decode_extract_result(false, worker, stderr.as_bytes())
        else {
            panic!("worker failure must stay typed");
        };
        let message = &diagnostics[0].message;
        assert!(message.contains("ghc: panic!"), "{message}");
        assert!(!message.contains("tidepool-timing"), "{message}");
        assert!(!message.contains("tidepool-checked"), "{message}");
        assert!(message.len() <= "compiler worker stderr:\n".len() + 4_000);
    }

    // ---- render_diagnostics ----

    #[test]
    fn single_in_range_error_renders_with_item_label_and_remapped_line() {
        let source = "line1\nline2\nageDays now c = _\nline4\n";
        let d = diag(
            "/tmp/x/Expr.hs",
            35,
            8,
            35,
            14,
            DiagnosticSeverity::Error,
            "No instance for HasField",
        );
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(33, 40)]),
                line_offset: 33,
                col_indent: 2,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.starts_with("<item>:2:6-12: error:"), "{got}");
        assert!(got.contains("No instance for HasField"), "{got}");
    }

    #[test]
    fn fallout_outside_range_collapses_to_footer_in_range_survives() {
        let source = "line1\n";
        let in_range = diag(
            "Expr.hs",
            2,
            10,
            2,
            10,
            DiagnosticSeverity::Error,
            "Ambiguous type variable",
        );
        let fallout = diag(
            "Expr.hs",
            9,
            5,
            9,
            5,
            DiagnosticSeverity::Error,
            "Overlapping instances for ToWire",
        );
        let got = render_diagnostics(
            &[in_range, fallout],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(2, 3)]),
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.contains("Ambiguous type variable"), "{got}");
        assert!(!got.contains("Overlapping instances"), "{got}");
        assert!(
            got.contains(
                "Further diagnostics on generated workbench wrapper lines: 1 error(s); locations suppressed"
            ),
            "{got}"
        );
    }

    /// A batch reported entirely on generated wrapper lines (dropping it all would
    /// leave zero survivors) renders the synthetic plain-language framing
    /// line PLUS the cleaned underlying diagnostic — never bare suppression,
    /// since the recipient must still get something actionable.
    #[test]
    fn all_wrapper_batch_renders_framing_and_underlying_diagnostic() {
        let source = "line1\n";
        let only_fallout = diag(
            "Expr.hs",
            9,
            5,
            9,
            5,
            DiagnosticSeverity::Error,
            "Overlapping instances for ToWire",
        );
        let got = render_diagnostics(
            &[only_fallout],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(2, 3)]),
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(
            got.contains("GHC reported 1 error(s) only on generated workbench wrapper lines"),
            "{got}"
        );
        assert!(
            got.contains("do not identify whether the root cause is in the authored input"),
            "{got}"
        );
        assert!(!got.contains("not in your block's own code"), "{got}");
        assert!(got.contains("1 error(s)"), "{got}");
        // The underlying diagnostic is now INCLUDED, raw (never remapped to
        // `<item>` coordinates — it isn't in the user's own code), never
        // suppressed.
        assert!(got.contains("Overlapping instances for ToWire"), "{got}");
        assert!(got.contains("Expr.hs:9:5"), "{got}");
        assert!(!got.contains("<item>:"), "{got}");
    }

    /// A multi-diagnostic ALL-wrapper batch counts every one of them in the
    /// synthetic framing line, and includes every one of them in the
    /// appended underlying-diagnostic text — not just the first.
    #[test]
    fn all_wrapper_batch_counts_and_includes_every_diagnostic() {
        let source = "line1\n";
        let a = diag(
            "Expr.hs",
            9,
            5,
            9,
            5,
            DiagnosticSeverity::Error,
            "Overlapping instances",
        );
        let b = diag(
            "Expr.hs",
            10,
            1,
            10,
            1,
            DiagnosticSeverity::Error,
            "No instance for ToJSON",
        );
        let got = render_diagnostics(
            &[a, b],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(2, 3)]),
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.contains("2 error(s)"), "{got}");
        assert!(got.contains("Overlapping instances"), "{got}");
        assert!(got.contains("No instance for ToJSON"), "{got}");
    }

    #[test]
    fn generated_warning_policy_preserves_exact_path_matching() {
        let own = "Tidepool/Session/Lib/G26.hs";
        let input = [
            diag(
                "Tidepool/Session/Lib/G25.hs",
                1,
                1,
                1,
                2,
                DiagnosticSeverity::Warning,
                "older",
            ),
            diag(own, 1, 1, 1, 2, DiagnosticSeverity::Warning, "own"),
            diag(
                "/tmp/Tidepool/Session/Lib/G26.hs",
                1,
                1,
                1,
                2,
                DiagnosticSeverity::Warning,
                "absolute",
            ),
            diag(own, 1, 1, 1, 2, DiagnosticSeverity::Error, "error"),
            diag(
                "Library.hs",
                1,
                1,
                1,
                2,
                DiagnosticSeverity::Warning,
                "ordinary",
            ),
            ExtractDiag {
                span: None,
                severity: DiagnosticSeverity::Warning,
                message: "unlocated".into(),
            },
        ];
        let original = input.clone();
        for (policy, expected) in [
            (
                GeneratedWarningPolicy::KeepAll,
                vec!["error", "older", "own", "absolute", "ordinary", "unlocated"],
            ),
            (
                GeneratedWarningPolicy::KeepOnly(own),
                vec!["error", "own", "ordinary", "unlocated"],
            ),
            (
                GeneratedWarningPolicy::DropAll,
                vec!["error", "ordinary", "unlocated"],
            ),
        ] {
            let rendered = render_diagnostics_structured(
                &input,
                &RenderOpts {
                    anchor: "Expr.hs",
                    label: "<decl>",
                    user_lines: None,
                    line_offset: 0,
                    col_indent: 0,
                    generated_warnings: policy,
                    source: "",
                },
            );
            let messages: Vec<_> = rendered
                .diagnostics
                .iter()
                .map(|d| d.message.as_str())
                .collect();
            assert_eq!(messages, expected, "{policy:?}");
            assert_eq!(rendered.text.split("\n\n").count(), expected.len());
        }
        assert_eq!(input, original);
    }

    #[test]
    fn embedded_suffix_anchor_is_not_matched() {
        let d = diag(
            "SomeExpr.hs",
            3,
            1,
            3,
            1,
            DiagnosticSeverity::Error,
            "whatever",
        );
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: None,
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source: "",
            },
        );
        // Not remapped to <item> — the raw file:line:col passes through.
        assert!(got.starts_with("SomeExpr.hs:3:1: error:"), "{got}");
    }

    /// An ambiguous `pure` diagnostic that legitimately survives the span
    /// partition (it's on the user's own line) can still carry GHC's own
    /// "Relevant bindings include __b :: ..." text, since `maxRelevantBinds =
    /// Just 0` only minimizes (not eliminates) the list — this is INSIDE a
    /// kept diagnostic's message, not a separate droppable diagnostic. `__b`'s
    /// OWN `(bound at ...)` here is on the scaffold-preamble line (1),
    /// distinct from the diagnostic's own span (line 2, the user's line) —
    /// span-based classification drops it because ITS span, not the
    /// diagnostic's, falls outside `user_lines`.
    #[test]
    fn scaffold_relevant_binds_scrubbed_from_a_surviving_diagnostic() {
        let source = "line1\n__b = pure (toWire x, x.files)\n";
        let d = diag(
            "Expr.hs",
            2,
            1,
            2,
            5,
            DiagnosticSeverity::Error,
            "* Ambiguous type variable `f0' arising from a use of `pure'\n\
             Relevant bindings include\n  __b :: f0 (Value, b0) (bound at Expr.hs:1:2)\n  \
             (Some bindings suppressed; use -fmax-relevant-binds=N or -fno-max-relevant-binds)\n\
             Probable fix: use a type annotation",
        );
        let rendered = render_diagnostics_structured(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(2, 2)]),
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        let got = &rendered.text;
        assert_eq!(
            rendered.diagnostics[0].message,
            "* Ambiguous type variable `f0' arising from a use of `pure'\nProbable fix: use a type annotation"
        );
        assert!(!got.contains("__b ::"), "{got}");
        assert!(!got.contains("Relevant bindings include"), "{got}");
        assert!(!got.contains("Some bindings suppressed"), "{got}");
        assert!(got.contains("Ambiguous type variable"), "{got}");
        assert!(got.contains("Probable fix"), "{got}");
    }

    #[test]
    fn scaffold_relevant_binds_keeps_user_named_binding() {
        // `c`'s own span (line 2) is inside `user_lines`; `__b`'s own span
        // (line 1, scaffold preamble) is outside — proving drop-vs-keep is
        // decided per-entry by span, not by name.
        let msg = "* Ambiguous\n  Relevant bindings include\n    c :: Commit (bound at Expr.hs:2:1)\n    __b :: f0 (bound at Expr.hs:1:2)\n";
        let opts = RenderOpts {
            anchor: "Expr.hs",
            label: "<item>",
            user_lines: Some(&[(2, 2)]),
            line_offset: 0,
            col_indent: 0,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source: "",
        };
        let got = drop_scaffold_relevant_binds(msg, &opts);
        assert!(!got.contains("__b ::"), "{got}");
        assert!(got.contains("c :: Commit"), "{got}");
        assert!(got.contains("Relevant bindings include"), "{got}");
    }

    /// A user's OWN binding named `result` must survive when its own
    /// `(bound at ...)` span is inside `user_lines` — classification is by
    /// span only, never by binder name, since a legitimate user name can
    /// collide with scaffold-sounding names.
    #[test]
    fn user_named_result_binding_with_own_span_in_range_is_kept() {
        let msg =
            "* Ambiguous\n  Relevant bindings include\n    result :: Int (bound at Expr.hs:2:1)\n";
        let opts = RenderOpts {
            anchor: "Expr.hs",
            label: "<item>",
            user_lines: Some(&[(2, 2)]),
            line_offset: 0,
            col_indent: 0,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source: "",
        };
        let got = drop_scaffold_relevant_binds(msg, &opts);
        assert!(got.contains("result :: Int"), "{got}");
    }

    /// A dropped entry's wrapped-type continuation line(s) — where GHC puts
    /// `(bound at ...)` on the LAST physical line, not the first — must be
    /// dropped together with the entry, never left behind as orphaned text.
    #[test]
    fn dropped_entry_continuation_lines_are_dropped_too() {
        let msg = "* Ambiguous\n  Relevant bindings include\n    __b :: SomeReally\n      WrappedType (bound at Expr.hs:1:2)\n    (Some bindings suppressed; use -fmax-relevant-binds=N or -fno-max-relevant-binds)\n";
        let opts = RenderOpts {
            anchor: "Expr.hs",
            label: "<item>",
            user_lines: Some(&[(2, 2)]),
            line_offset: 0,
            col_indent: 0,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source: "",
        };
        let got = drop_scaffold_relevant_binds(msg, &opts);
        assert!(!got.contains("__b"), "{got}");
        assert!(!got.contains("WrappedType"), "{got}");
        assert!(!got.contains("Relevant bindings"), "{got}"); // all-dropped region
    }

    /// An entry with no parseable `(bound at ...)` span is kept by default —
    /// losing real user diagnostic info silently is worse than occasional
    /// residual scaffold noise.
    #[test]
    fn unspannable_entry_is_kept_by_default() {
        let msg = "* Ambiguous\n  Relevant bindings include\n    __b :: f0 (bound at <no location info>)\n";
        let opts = RenderOpts {
            anchor: "Expr.hs",
            label: "<item>",
            user_lines: Some(&[(2, 2)]),
            line_offset: 0,
            col_indent: 0,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source: "",
        };
        let got = drop_scaffold_relevant_binds(msg, &opts);
        assert!(got.contains("__b ::"), "{got}");
    }

    #[test]
    fn extract_user_code_lines_parses_marker() {
        let source = "foo\nbar -- [user-lines] 5:9\nbaz\n";
        assert_eq!(extract_user_code_lines(source), Some((5, 9)));
    }

    #[test]
    fn extract_user_code_lines_absent_is_none() {
        assert_eq!(extract_user_code_lines("no marker here"), None);
    }

    #[test]
    fn extract_user_code_ranges_collects_helpers_imports_and_code() {
        let source = "-- [user-imports-lines] 3:3\n\
                       -- [user]\n\
                       helper = 1 -- [user-helpers-lines] 4:4\n\
                       __user = let {\n \
                       __b =\npure 1\n } in __b  -- [user-lines] 7:7\n";
        let mut ranges = extract_user_code_ranges(source).unwrap();
        ranges.sort_unstable();
        assert_eq!(ranges, vec![(3, 3), (4, 4), (7, 7)]);
    }

    #[test]
    fn extract_user_code_ranges_absent_is_none() {
        assert_eq!(extract_user_code_ranges("no markers here"), None);
    }

    /// The bug this closes: a `helpers`-param error (e.g. a deliberately
    /// disabled partial function used INSIDE `helpers`) used to be
    /// misclassified as wrapper-origin fallout — dropped outright, or (before
    /// this wave) folded into the all-wrapper synthetic suppression — because
    /// `helpers` sits OUTSIDE the single code-only `user_lines` range. With a
    /// SECOND range covering the helpers region, the same diagnostic is now
    /// KEPT (real position, raw anchor — helpers has no friendly `<item>`
    /// remap of its own, but it is real and actionable).
    #[test]
    fn helpers_region_error_is_kept_not_fallout() {
        let source = "line1\nhelperLine\nline3\n";
        // helpers occupies line 2; code occupies line 3.
        let helpers_err = diag(
            "Expr.hs",
            2,
            1,
            2,
            5,
            DiagnosticSeverity::Error,
            "(!!) is partial — use atMay xs i :: Maybe a",
        );
        let got = render_diagnostics(
            &[helpers_err],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(2, 2), (3, 3)]),
                line_offset: 2,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.contains("(!!) is partial"), "{got}");
        assert!(
            !got.contains("only on generated workbench wrapper lines"),
            "a genuinely kept diagnostic must never fall into the all-wrapper synthetic path: {got}"
        );
    }

    /// Mirror of the helpers case for the `imports` param: a bad qualified
    /// import (a real GHC import diagnostic) is a SECOND, disjoint region
    /// ahead of the code's own range — still kept, not classified as fallout.
    #[test]
    fn imports_region_error_is_kept_not_fallout() {
        let source = "import Data.Aeson as Aeson\nline2\ncode\n";
        let import_err = diag(
            "Expr.hs",
            1,
            1,
            1,
            10,
            DiagnosticSeverity::Error,
            "Could not find module `Data.Aeson'",
        );
        let got = render_diagnostics(
            &[import_err],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(1, 1), (3, 3)]),
                line_offset: 3,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.contains("Could not find module"), "{got}");
        assert!(
            !got.contains("only on generated workbench wrapper lines"),
            "{got}"
        );
    }

    /// The gutter/caret excerpt's printed line NUMBER must match the header's
    /// remapped line — this pins the reported defect (header said `<turn>:6`,
    /// the gutter printed the raw generated-file line, e.g. `42`).
    #[test]
    fn gutter_line_number_matches_remapped_header_line() {
        let source = "preamble0\npreamble1\nuserLine1\n";
        let d = diag(
            "Expr.hs",
            3,
            1,
            3,
            5,
            DiagnosticSeverity::Error,
            "Not in scope: `Foo'",
        );
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<turn>",
                user_lines: Some(&[(3, 3)]),
                line_offset: 2,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.contains("<turn>:1:"), "{got}");
        // The gutter must show the SAME "1", never the raw "3".
        assert!(got.contains("1 | userLine1"), "{got}");
        assert!(!got.contains("3 | userLine1"), "{got}");
    }

    /// A diagnostic that numerically falls inside the user-code range but
    /// whose GHC span actually lands on a WRAPPER-scaffold line (e.g. GHC
    /// attributing a binding's error to the `__user = let {`/`} in __b`
    /// bracket, or to the `__anchor` signature) must never show that scaffold
    /// text as if it were the user's own source — the snippet is omitted
    /// entirely rather than printed under a misleading position.
    #[test]
    fn scaffold_content_at_a_kept_position_omits_the_snippet() {
        let source = "userLine0\n__anchor :: P.Show a => a -> a\n";
        let d = diag(
            "Expr.hs",
            2,
            1,
            2,
            5,
            DiagnosticSeverity::Error,
            "Ambiguous type variable",
        );
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<turn>",
                user_lines: Some(&[(1, 2)]),
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(got.contains("Ambiguous type variable"), "{got}");
        assert!(!got.contains("__anchor"), "{got}");
        assert!(!got.contains(" | "), "no gutter/caret at all: {got}");
    }

    // ---- structured diagnostics alongside the rendered text ----

    /// Every rendered block's header is a coordinate the renderer already
    /// computed. Parsing it back out is exactly the work the structured form
    /// exists to remove, so this test does that parse ONCE, here, to hold the
    /// two forms together — and nowhere else in the system.
    fn header_of(block: &str) -> &str {
        block.lines().next().expect("a block has a header line")
    }

    #[test]
    fn structured_location_matches_rendered_header() {
        let source = "line1\nline2\nageDays now c = _\nline4\n";
        let opts = RenderOpts {
            anchor: "Expr.hs",
            label: "<item>",
            user_lines: Some(&[(1, 40)]),
            line_offset: 33,
            col_indent: 2,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source,
        };
        // One in the user's own region (remapped), one in the anchor's
        // generated preamble (raw, anchor-labelled), one in a foreign file.
        let authored = diag(
            "/tmp/x/Expr.hs",
            35,
            8,
            35,
            14,
            DiagnosticSeverity::Error,
            "No instance for HasField",
        );
        let preamble = diag(
            "/tmp/x/Expr.hs",
            4,
            1,
            4,
            9,
            DiagnosticSeverity::Warning,
            "Redundant import",
        );
        let foreign = diag(
            "compiler/GHC/Tc.hs",
            77,
            3,
            77,
            9,
            DiagnosticSeverity::Error,
            "panic",
        );
        let rendered = render_diagnostics_structured(&[authored, preamble, foreign], &opts);
        let blocks: Vec<&str> = rendered.text.split("\n\n").collect();
        assert_eq!(rendered.diagnostics.len(), 3, "{}", rendered.text);

        // The header the model reads and the structure it no longer has to
        // parse agree, field by field.
        assert_eq!(header_of(blocks[0]), "<item>:2:6-12: error:");
        assert_eq!(
            rendered.diagnostics[0].location,
            DiagnosticLocation::Authored {
                label: "<item>".into(),
                start_line: 2,
                start_col: 6,
                end_line: 2,
                end_col: 12,
            }
        );
        assert_eq!(rendered.diagnostics[0].severity, DiagnosticLevel::Error);
        assert_eq!(rendered.diagnostics[0].message, "No instance for HasField");

        assert_eq!(header_of(blocks[2]), "Expr.hs:4:1-9: warning:");
        assert_eq!(
            rendered.diagnostics[2].location,
            DiagnosticLocation::Foreign {
                file: "Expr.hs".into(),
                start_line: 4,
                start_col: 1,
                end_line: 4,
                end_col: 9,
            }
        );
        assert_eq!(rendered.diagnostics[2].severity, DiagnosticLevel::Warning);

        assert_eq!(header_of(blocks[1]), "compiler/GHC/Tc.hs:77:3: error:");
        assert_eq!(
            rendered.diagnostics[1].location,
            DiagnosticLocation::Foreign {
                file: "compiler/GHC/Tc.hs".into(),
                start_line: 77,
                start_col: 3,
                end_line: 77,
                end_col: 9,
            }
        );
    }

    /// GHC's `UnhelpfulSpan`, and any worker failure that never had a span,
    /// must stay representable rather than be dropped or forced into a
    /// location it does not have. The message is what must survive whole.
    #[test]
    fn unspanned_diagnostic_stays_unlocated_with_its_message_intact() {
        let unspanned = ExtractDiag {
            span: None,
            severity: DiagnosticSeverity::Error,
            message: "compiler worker stderr:\nghc: panic! (the 'impossible' happened)".into(),
        };
        let rendered = render_diagnostics_structured(
            &[unspanned],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<cell>",
                user_lines: None,
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source: "",
            },
        );
        assert_eq!(rendered.diagnostics.len(), 1);
        assert_eq!(
            rendered.diagnostics[0].location,
            DiagnosticLocation::Unlocated
        );
        assert_eq!(
            rendered.diagnostics[0].message,
            "compiler worker stderr:\nghc: panic! (the 'impossible' happened)"
        );
        assert_eq!(header_of(&rendered.text), "error:");
    }

    /// The plain and structured presenters share the same text. Held by construction (`render_diagnostics` IS
    /// `render_diagnostics_structured(..).text`) and by a literal source-first golden.
    #[test]
    fn rendered_text_is_byte_identical_beside_the_structure() {
        // 34 filler lines so the diagnostic's raw line 35 exists in the
        // source and the gutter/caret excerpt renders too.
        let mut source = String::new();
        for n in 1..=34 {
            source.push_str(&format!("filler{n}\n"));
        }
        source.push_str("ageDays now c = _\n");
        let d = diag(
            "/tmp/x/Expr.hs",
            35,
            8,
            35,
            14,
            DiagnosticSeverity::Error,
            "No instance for HasField",
        );
        let opts = || RenderOpts {
            anchor: "Expr.hs",
            label: "<item>",
            user_lines: Some(&[(33, 40)]),
            line_offset: 33,
            col_indent: 2,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source: &source,
        };
        let text = render_diagnostics(std::slice::from_ref(&d), &opts());
        let structured = render_diagnostics_structured(std::slice::from_ref(&d), &opts());
        assert_eq!(text, structured.text);
        assert_eq!(
            text,
            "<item>:2:6-12: error:\n  |\n2 | ageDays now c = _\n  |        ^^^^^^\n    No instance for HasField"
        );
        // ...and the same coordinates the text just showed, as data.
        assert_eq!(
            structured.diagnostics[0].location,
            DiagnosticLocation::Authored {
                label: "<item>".into(),
                start_line: 2,
                start_col: 6,
                end_line: 2,
                end_col: 12,
            }
        );
    }

    #[test]
    fn retained_pure_command_projection_shows_source_before_effect_row() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/pure_command_projection_diagnostics.json"
        ))
        .unwrap();
        let diagnostics: Vec<ExtractDiag> =
            serde_json::from_value(fixture["diagnostics"].clone()).unwrap();
        let source = fixture["cell"].as_str().unwrap();
        let rendered = render_diagnostics_structured(
            &diagnostics,
            &RenderOpts {
                anchor: "<cell>",
                label: "<cell>",
                user_lines: None,
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source,
            },
        );
        assert!(
            rendered
                .text
                .find("stdoutAssessment <- Cmd.stdout failedProbe")
                .unwrap()
                < rendered
                    .text
                    .find("Couldn't match expected type: Eff")
                    .unwrap()
        );
        assert_eq!(rendered.diagnostics.len(), 1);
        assert_eq!(rendered.diagnostics[0].message, diagnostics[0].message);
        assert_eq!(rendered.diagnostics[0].severity, DiagnosticLevel::Error);
    }

    proptest::proptest! {
        #[test]
        fn projected_coordinates_match_parsed_headers(
            kind in 0u8..4,
            start_line in proptest::prop_oneof![0u32..12, proptest::strategy::Just(u32::MAX)],
            line_delta in 0u32..3,
            start_col in 0u32..8,
            col_width in 0u32..8,
            line_offset in proptest::prop_oneof![0usize..16, proptest::strategy::Just(usize::MAX)],
            col_indent in proptest::prop_oneof![0usize..16, proptest::strategy::Just(usize::MAX)],
        ) {
            let mut input = diag(
                if kind == 1 { "C:\\λ\\SomeExpr.hs" } else { "C:\\λ\\Expr.hs" },
                start_line, start_col, start_line.saturating_add(line_delta), start_col + col_width,
                DiagnosticSeverity::Error, "source error λ",
            );
            if kind == 2 { input.span = None; }
            let original = input.clone();
            let rendered = render_diagnostics_structured(std::slice::from_ref(&input), &RenderOpts {
                anchor: "Expr.hs", label: "<cell>",
                user_lines: (kind == 3).then_some(&[][..]),
                line_offset, col_indent,
                generated_warnings: GeneratedWarningPolicy::KeepAll,
                source: "",
            });
            proptest::prop_assert_eq!(input, original);
            proptest::prop_assert_eq!(rendered.diagnostics.len(), 1);
            // The all-wrapper framing is a separate block; its underlying
            // header uses the same grammar as an ordinary diagnostic.
            let header = header_of(rendered.text.rsplit("\n\n").next().unwrap());
            let diagnostic = &rendered.diagnostics[0];
            proptest::prop_assert_eq!(&diagnostic.message, "source error λ");
            let coordinate = header.strip_suffix(": error:");
            match &diagnostic.location {
                DiagnosticLocation::Authored { label, start_line, start_col, end_col, .. }
                | DiagnosticLocation::Foreign { file: label, start_line, start_col, end_col, .. } => {
                    let mut parts = coordinate.expect("located header").rsplitn(3, ':');
                    let columns = parts.next().unwrap();
                    let line: u32 = parts.next().unwrap().parse().unwrap();
                    let file = parts.next().unwrap();
                    let (column, range_end) = match columns.split_once('-') {
                        Some((start, end)) => (start, Some(end.parse::<u32>().unwrap())),
                        None => (columns, None),
                    };
                    proptest::prop_assert_eq!(file, label);
                    proptest::prop_assert_eq!(line, *start_line);
                    proptest::prop_assert_eq!(column.parse::<u32>().unwrap(), *start_col);
                    if let Some(end) = range_end {
                        proptest::prop_assert_eq!(end, *end_col);
                    }
                }
                DiagnosticLocation::Unlocated => proptest::prop_assert_eq!(header, "error:"),
            }
        }

        #[test]
        fn presentation_preserves_errors_and_warning_occurrences(
            chars in proptest::collection::vec(proptest::char::any(), 0..32),
            copies in 2usize..8,
            reverse in proptest::bool::ANY,
        ) {
            let message: String = chars.into_iter().filter(|c| !matches!(c, '\n' | '\r')).collect();
            let authored = diag("Expr.hs", 1, 1, 1, 2, DiagnosticSeverity::Error, &format!("authored {message}"));
            let foreign = diag("Foreign.hs", 1, 1, 1, 2, DiagnosticSeverity::Error, &format!("foreign {message}"));
            let warning = diag("Scaffold.hs", 1, 1, 1, 2, DiagnosticSeverity::Warning, "scaffold warning λ");
            let mut input = vec![warning.clone(); copies];
            input.extend([foreign, authored.clone(), authored]);
            if reverse { input.reverse(); }
            let original = input.clone();
            let rendered = render_diagnostics_structured(&input, &RenderOpts {
                anchor: "Expr.hs", label: "<cell>", user_lines: None,
                line_offset: 0, col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::KeepAll, source: "λ = \"é\"",
            });
            proptest::prop_assert_eq!(input, original);
            proptest::prop_assert_eq!(rendered.diagnostics.len(), copies + 3);
            proptest::prop_assert_eq!(rendered.diagnostics.iter().filter(|d| d.severity == DiagnosticLevel::Error).count(), 3);
            proptest::prop_assert!(matches!(rendered.diagnostics[0].location, DiagnosticLocation::Authored { .. }), "authored error must lead");
            proptest::prop_assert_eq!(rendered.text.matches("scaffold warning λ").count(), 1);
            proptest::prop_assert!(rendered.text.contains(&format!("repeated {copies} times")), "warning count must survive");
            proptest::prop_assert!(rendered.diagnostics.windows(2).all(|pair|
                pair[0].severity == DiagnosticLevel::Error || pair[1].severity == DiagnosticLevel::Warning));
            proptest::prop_assert_eq!(rendered.diagnostics.iter().filter(|d| d.message == format!("authored {message}")).count(), 2);
            proptest::prop_assert_eq!(rendered.text.matches("\n1 | λ = \"é\"").count(), 2);
        }
    }

    #[test]
    fn generated_warning_counts_do_not_claim_errors() {
        let warning = diag(
            "Expr.hs",
            1,
            1,
            1,
            2,
            DiagnosticSeverity::Warning,
            "warning λ",
        );
        let opts = RenderOpts {
            anchor: "Expr.hs",
            label: "<cell>",
            user_lines: Some(&[(2, 2)]),
            line_offset: 0,
            col_indent: 0,
            generated_warnings: GeneratedWarningPolicy::KeepAll,
            source: "generated\nauthored",
        };
        let rendered = render_diagnostics_structured(&[warning.clone(), warning.clone()], &opts);
        assert!(rendered.text.starts_with("GHC reported 2 warning(s) only"));
        assert_eq!(rendered.text.matches("warning λ").count(), 1);
        assert_eq!(rendered.diagnostics.len(), 2);
        let error = diag(
            "Expr.hs",
            1,
            1,
            1,
            2,
            DiagnosticSeverity::Error,
            "authored cause",
        );
        let mixed = render_diagnostics_structured(&[warning, error], &opts);
        assert!(mixed
            .text
            .starts_with("GHC reported 1 error(s), 1 warning(s) only"));
        assert_eq!(mixed.diagnostics[0].severity, DiagnosticLevel::Error);
    }

    /// The structured list mirrors the blocks actually rendered: a dropped
    /// foreign gen warning is absent from both, and the wrapper-fallout
    /// footer — a count, not a diagnostic — adds no entry.
    #[test]
    fn structure_mirrors_only_the_blocks_that_were_rendered() {
        let source = "userLine\n";
        let kept = diag(
            "Expr.hs",
            1,
            1,
            1,
            5,
            DiagnosticSeverity::Error,
            "kept in range",
        );
        let fell_out = diag(
            "Expr.hs",
            9,
            1,
            9,
            5,
            DiagnosticSeverity::Error,
            "wrapper fallout",
        );
        let gen_warning = diag(
            "Tidepool/Session/Lib/G4.hs",
            1,
            1,
            1,
            2,
            DiagnosticSeverity::Warning,
            "dependency noise",
        );
        let rendered = render_diagnostics_structured(
            &[kept, fell_out, gen_warning],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<cell>",
                user_lines: Some(&[(1, 1)]),
                line_offset: 0,
                col_indent: 0,
                generated_warnings: GeneratedWarningPolicy::DropAll,
                source,
            },
        );
        assert!(rendered.text.contains("kept in range"), "{}", rendered.text);
        assert!(
            rendered.text.contains("wrapper lines: 1 error(s)"),
            "{}",
            rendered.text
        );
        assert!(
            !rendered.text.contains("dependency noise"),
            "{}",
            rendered.text
        );
        assert_eq!(rendered.diagnostics.len(), 1, "{:?}", rendered.diagnostics);
        assert_eq!(rendered.diagnostics[0].message, "kept in range");
    }
}
