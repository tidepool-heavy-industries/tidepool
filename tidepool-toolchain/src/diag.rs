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
                    message: format!("compiler worker stderr:\n{}", truncate_tail(stderr, 4_000)),
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
    /// When `Some(keep_path)`, a WARNING diagnostic whose file matches the
    /// generated-lib-module pattern (`Tidepool/Session/Lib/G` substring) and
    /// is NOT `keep_path` is dropped — session-lib generation warnings are
    /// dependency noise that would otherwise ride every later item's errors.
    /// Pass `Some("")` to drop every foreign gen warning unconditionally.
    pub drop_foreign_gen_warnings_except: Option<&'a str>,
    /// The assembled module source text (or candidate source), for building
    /// a gutter/caret excerpt by indexing into it by line number.
    pub source: &'a str,
}

/// Render surviving diagnostics (after the fallout partition + foreign-gen
/// warning drop) into human-facing text: one block per diagnostic (a
/// `{label}:{line}:{col}[-{end_col}]: {severity}:` header, the message body,
/// and a gutter source excerpt when the span is single-line, the line exists
/// in `opts.source`, and that line isn't itself wrapper-template scaffold),
/// joined by a blank line, with a wrapper-fallout footer appended when the
/// `user_lines` partition dropped anything — UNLESS every surviving
/// diagnostic was reported only on generated wrapper lines, in which case
/// this renders one synthetic, plain-language framing line PLUS the cleaned underlying
/// diagnostic(s) (raw `file:line:col` header, no snippet — the model never
/// wrote those lines, so a coordinate remapped against user code would be
/// misleading) instead of the ordinary per-diagnostic blocks. The recipient
/// always gets SOMETHING actionable: never bare suppression.
#[must_use]
pub fn render_diagnostics(diags: &[ExtractDiag], opts: &RenderOpts<'_>) -> String {
    // Foreign-gen-warning drop: no "never empty" guard — dropping a warning
    // can legitimately leave zero total diagnostics.
    let after_gen_drop: Vec<&ExtractDiag> = diags
        .iter()
        .filter(|d| !is_dropped_foreign_gen_warning(d, opts.drop_foreign_gen_warnings_except))
        .collect();

    // User-lines fallout partition.
    let (kept, fallout): (Vec<&ExtractDiag>, Vec<&ExtractDiag>) = match opts.user_lines {
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

    // Every surviving diagnostic points at generated wrapper text. That span
    // does not identify the root cause, so describe only the known location
    // and retain the underlying diagnostics for diagnosis.
    if kept.is_empty() && !fallout.is_empty() {
        let mut out = format!(
            "GHC reported {} error(s) only on generated workbench wrapper lines. Those \
             locations do not identify whether the root cause is in the authored input or \
             generated framing. Underlying GHC diagnostic(s):",
            fallout.len()
        );
        for d in &fallout {
            out.push_str("\n\n");
            out.push_str(&render_one_raw(d, opts));
        }
        return out;
    }

    let mut blocks: Vec<String> = kept.iter().map(|d| render_one(d, opts)).collect();
    if !fallout.is_empty() {
        blocks.push(format!(
            "({} further error(s) reported on generated workbench wrapper lines; locations \
             suppressed)",
            fallout.len()
        ));
    }
    blocks.join("\n\n")
}

fn is_dropped_foreign_gen_warning(d: &ExtractDiag, keep_path: Option<&str>) -> bool {
    let Some(keep) = keep_path else {
        return false;
    };
    let Some(span) = &d.span else {
        return false;
    };
    d.severity == DiagnosticSeverity::Warning
        && span.file.contains("Tidepool/Session/Lib/G")
        && span.file != keep
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

/// The display coordinate space one diagnostic renders under: the label to
/// show (`opts.label`/`<expr>`-style when the span is in the user-code
/// region, `opts.anchor`/raw otherwise), the 1-based display line, and
/// whether that line is inside the user-code region (which alone decides
/// whether the column also gets `opts.col_indent` stripped). Computed ONCE
/// and shared by [`render_one`]'s header AND its gutter excerpt below — the
/// two must never disagree about which line number they're reporting (a
/// header claiming `<turn>:6` while the gutter printed the raw generated-file
/// line, e.g. `42`, was exactly the reported defect this fixes).
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

fn render_one(d: &ExtractDiag, opts: &RenderOpts<'_>) -> String {
    let coord = d.span.as_ref().and_then(|span| display_coord(span, opts));
    let header = match (&d.span, &coord) {
        (Some(span), Some(coord)) => {
            // Only strip the wrapper's column indent inside the user-code
            // region — a preamble-region diagnostic already shows a raw,
            // unshifted line (via `coord.line` above), so its column must
            // stay raw too, or the two disagree about which coordinate space
            // they're in.
            let strip_col = |c: u32| {
                let c = c as usize;
                if coord.in_user_region && c > opts.col_indent {
                    c - opts.col_indent
                } else {
                    c
                }
            };
            let start_col = strip_col(span.start_col);
            if span.start_line == span.end_line && span.end_col != span.start_col {
                format!(
                    "{}:{}:{start_col}-{}: {}:",
                    coord.label,
                    coord.line,
                    strip_col(span.end_col),
                    d.severity
                )
            } else {
                format!(
                    "{}:{}:{start_col}: {}:",
                    coord.label, coord.line, d.severity
                )
            }
        }
        (Some(span), None) => format!(
            "{}:{}:{}: {}:",
            span.file, span.start_line, span.start_col, d.severity
        ),
        (None, _) => format!("{}:", d.severity),
    };

    let scrubbed_message = drop_scaffold_relevant_binds(&d.message, opts);

    let mut out = header;
    out.push('\n');
    for line in scrubbed_message.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.pop(); // drop trailing newline (blocks are joined with "\n\n")

    // Gutter/caret excerpt: only for a single-line span, indexed by the
    // SOURCE's own (raw, un-remapped) line number — this excerpt reads
    // straight out of `opts.source`, which is the assembled/candidate
    // module — but DISPLAYED under the same `coord.line` the header used, and
    // omitted entirely when the indexed line is itself wrapper scaffold (a
    // GHC span that lands, numerically, inside the user-code range but whose
    // real content is generated template text — never shown as if it were
    // the user's own code).
    if let Some(span) = &d.span {
        if span.start_line == span.end_line {
            if let Some(src_line) = opts.source.lines().nth(span.start_line as usize - 1) {
                if !looks_like_wrapper_scaffold(src_line) {
                    let disp_line = coord.as_ref().map_or(span.start_line as usize, |c| c.line);
                    let gutter = format!("{disp_line}");
                    out.push_str(&format!("\n{:>width$} |\n", "", width = gutter.len()));
                    out.push_str(&format!("{gutter} | {src_line}\n"));
                    let start = span.start_col.saturating_sub(1) as usize;
                    let width = (span.end_col.saturating_sub(span.start_col)).max(1) as usize;
                    out.push_str(&format!(
                        "{:>gwidth$} | {:>start$}{}",
                        "",
                        "",
                        "^".repeat(width),
                        gwidth = gutter.len(),
                        start = start
                    ));
                }
            }
        }
    }
    out
}

/// Render one diagnostic RAW, never remapped to `opts.label`/`<expr>`
/// coordinates and never with a gutter/caret excerpt — used ONLY for the
/// all-wrapper batch's "underlying diagnostic(s)" appendix
/// ([`render_diagnostics`]): these diagnostics are, by construction, NOT in
/// the user's own code, so a remapped position would misattribute them and a
/// source excerpt would show generated wrapper text. The real
/// `file:line:col` plus the scrubbed message is still fully actionable —
/// unlike outright suppression.
fn render_one_raw(d: &ExtractDiag, opts: &RenderOpts<'_>) -> String {
    let header = match &d.span {
        Some(span) => format!(
            "{}:{}:{}: {}:",
            span.file, span.start_line, span.start_col, d.severity
        ),
        None => format!("{}:", d.severity),
    };
    let scrubbed_message = drop_scaffold_relevant_binds(&d.message, opts);
    let mut out = header;
    out.push('\n');
    for line in scrubbed_message.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.pop();
    out
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
                drop_foreign_gen_warnings_except: None,
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
                drop_foreign_gen_warnings_except: None,
                source,
            },
        );
        assert!(got.contains("Ambiguous type variable"), "{got}");
        assert!(!got.contains("Overlapping instances"), "{got}");
        assert!(
            got.contains(
                "1 further error(s) reported on generated workbench wrapper lines; locations suppressed"
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
                drop_foreign_gen_warnings_except: None,
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
                drop_foreign_gen_warnings_except: None,
                source,
            },
        );
        assert!(got.contains("2 error(s)"), "{got}");
        assert!(got.contains("Overlapping instances"), "{got}");
        assert!(got.contains("No instance for ToJSON"), "{got}");
    }

    #[test]
    fn foreign_gen_warning_dropped_own_gen_kept() {
        let source = "";
        let foreign = ExtractDiag {
            span: Some(DiagSpan {
                file: "Tidepool/Session/Lib/G25.hs".into(),
                start_line: 33,
                start_col: 35,
                end_line: 33,
                end_col: 40,
            }),
            severity: DiagnosticSeverity::Warning,
            message: "partial head".into(),
        };
        let own = ExtractDiag {
            span: Some(DiagSpan {
                file: "Tidepool/Session/Lib/G26.hs".into(),
                start_line: 3,
                start_col: 1,
                end_line: 3,
                end_col: 5,
            }),
            severity: DiagnosticSeverity::Warning,
            message: "user warning".into(),
        };
        let got = render_diagnostics(
            &[foreign.clone(), own.clone()],
            &RenderOpts {
                anchor: "Tidepool/Session/Lib/G26.hs",
                label: "<decl>",
                user_lines: None,
                line_offset: 0,
                col_indent: 0,
                drop_foreign_gen_warnings_except: Some("Tidepool/Session/Lib/G26.hs"),
                source,
            },
        );
        assert!(!got.contains("partial head"), "{got}");
        assert!(got.contains("user warning"), "{got}");

        // keep_path = "" drops all gen warnings, including "own".
        let got2 = render_diagnostics(
            &[foreign, own],
            &RenderOpts {
                anchor: "Tidepool/Session/Lib/G26.hs",
                label: "<decl>",
                user_lines: None,
                line_offset: 0,
                col_indent: 0,
                drop_foreign_gen_warnings_except: Some(""),
                source,
            },
        );
        assert!(!got2.contains("partial head"), "{got2}");
        assert!(!got2.contains("user warning"), "{got2}");
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
                drop_foreign_gen_warnings_except: None,
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
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some(&[(2, 2)]),
                line_offset: 0,
                col_indent: 0,
                drop_foreign_gen_warnings_except: None,
                source,
            },
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
            drop_foreign_gen_warnings_except: None,
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
            drop_foreign_gen_warnings_except: None,
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
            drop_foreign_gen_warnings_except: None,
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
            drop_foreign_gen_warnings_except: None,
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
                drop_foreign_gen_warnings_except: None,
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
                drop_foreign_gen_warnings_except: None,
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
                drop_foreign_gen_warnings_except: None,
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
                drop_foreign_gen_warnings_except: None,
                source,
            },
        );
        assert!(got.contains("Ambiguous type variable"), "{got}");
        assert!(!got.contains("__anchor"), "{got}");
        assert!(!got.contains(" | "), "no gutter/caret at all: {got}");
    }
}
