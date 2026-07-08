//! The structured diagnostics contract with `tidepool-extract-bin`: parse the
//! extractor's fixed-shape stdout JSON report, and render surviving
//! diagnostics into human-facing text.
//!
//! `tidepool-extract-bin` prints exactly one JSON value to stdout on EVERY
//! invocation (success or failure): `{"version":1,"diagnostics":[...]}`, empty
//! `diagnostics` on success. Every diagnostic carries a real `(file, line,
//! col)` span (or `null` when GHC has none) plus a severity and message —
//! Rust answers "is this in the user's own code, or wrapper scaffolding?" by
//! comparing spans against known line ranges, never by pattern-matching GHC's
//! rendered wording.

/// A source span on the extractor's stdout report: `(file, startLine,
/// startCol, endLine, endCol)`.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct DiagSpan {
    pub file: String,
    #[serde(rename = "startLine")]
    pub start_line: u32,
    #[serde(rename = "startCol")]
    pub start_col: u32,
    #[serde(rename = "endLine")]
    pub end_line: u32,
    #[serde(rename = "endCol")]
    pub end_col: u32,
}

/// One diagnostic from the extractor's stdout report.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ExtractDiag {
    /// `None` when GHC has no real span for the diagnostic (`UnhelpfulSpan`).
    pub span: Option<DiagSpan>,
    /// `"error"` or `"warning"`.
    pub severity: String,
    pub message: String,
}

/// The fixed-shape stdout report.
#[derive(serde::Deserialize, Debug)]
pub struct DiagReport {
    pub version: u32,
    pub diagnostics: Vec<ExtractDiag>,
}

/// The wire version this server's reader understands. A mismatch is a
/// version skew between the deployed `tidepool-extract-bin` and this server,
/// not a user Haskell error.
const SUPPORTED_VERSION: u32 = 1;

/// Parse the extract binary's stdout as the fixed-shape diagnostics report.
/// Fails LOUD (never falls back to reading stderr) on malformed JSON or an
/// unexpected `version` — the error names the likely cause (a stale deployed
/// `tidepool-extract-bin` vs. this server's expectations) and includes a short
/// stderr tail for debugging.
pub fn parse_diag_report(stdout: &[u8], stderr: &[u8]) -> Result<DiagReport, String> {
    let text = String::from_utf8_lossy(stdout);
    let report: DiagReport = serde_json::from_str(&text).map_err(|e| {
        format!(
            "extract stdout did not parse as the diagnostics report ({e}) — likely a stale \
             deployed `tidepool-extract-bin` predating the structured-diagnostics contract; \
             rebuild/redeploy it. stdout: {}\nstderr tail: {}",
            truncate_tail(&text, 500),
            truncate_tail(&String::from_utf8_lossy(stderr), 500)
        )
    })?;
    if report.version != SUPPORTED_VERSION {
        return Err(format!(
            "extract emitted diagnostics wire version {}, this server expects {} — \
             rebuild/redeploy tidepool-extract-bin so both sides agree.",
            report.version, SUPPORTED_VERSION
        ));
    }
    Ok(report)
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
    /// `Some((start, end))` — 1-based inclusive line range of the user's own
    /// code within the anchor file. A diagnostic anchored in-file but OUTSIDE
    /// this range is wrapper-scaffold fallout: dropped, counted in one footer
    /// line, UNLESS dropping would leave zero survivors (never render "no
    /// diagnostics" when there is at least one) — in which case keep
    /// everything. `None` — no such partition (e.g. decl-candidate
    /// validation, which never wraps user code in scaffold).
    pub user_lines: Option<(usize, usize)>,
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
/// and a gutter source excerpt when the span is single-line and the line
/// exists in `opts.source`), joined by a blank line, with a wrapper-fallout
/// footer appended when the `user_lines` partition dropped anything.
#[must_use]
pub fn render_diagnostics(diags: &[ExtractDiag], opts: &RenderOpts<'_>) -> String {
    // Foreign-gen-warning drop: no "never empty" guard — dropping a warning
    // can legitimately leave zero total diagnostics.
    let after_gen_drop: Vec<&ExtractDiag> = diags
        .iter()
        .filter(|d| !is_dropped_foreign_gen_warning(d, opts.drop_foreign_gen_warnings_except))
        .collect();

    // User-lines fallout partition: dropping is guarded against emptying the
    // whole surviving set.
    let (kept, fallout_count) = match opts.user_lines {
        Some((start, end)) => {
            let mut kept = Vec::new();
            let mut fallout = 0usize;
            for d in &after_gen_drop {
                if is_in_anchor_file(d, opts.anchor) && !span_in_range(d, start, end) {
                    fallout += 1;
                } else {
                    kept.push(*d);
                }
            }
            if kept.is_empty() && fallout > 0 {
                // Guard: never drop everything. Keep the original (post-gen-drop) set.
                (after_gen_drop.clone(), 0)
            } else {
                (kept, fallout)
            }
        }
        None => (after_gen_drop.clone(), 0),
    };

    let mut blocks: Vec<String> = kept.iter().map(|d| render_one(d, opts)).collect();
    if fallout_count > 0 {
        blocks.push(format!(
            "({fallout_count} further error(s) suppressed: fallout in the result-display \
             wrapper from the error(s) above)"
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
    d.severity == "warning" && span.file.contains("Tidepool/Session/Lib/G") && span.file != keep
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

fn span_in_range(d: &ExtractDiag, start: usize, end: usize) -> bool {
    match &d.span {
        Some(span) => {
            let l = span.start_line as usize;
            l >= start && l <= end
        }
        None => false,
    }
}

fn render_one(d: &ExtractDiag, opts: &RenderOpts<'_>) -> String {
    let header = match &d.span {
        Some(span) if path_ends_with_anchor(&span.file, opts.anchor) => {
            let line = span.start_line as usize;
            let (disp_label, disp_line) = if line > opts.line_offset {
                (opts.label, line - opts.line_offset)
            } else {
                (opts.anchor, line)
            };
            let strip_col = |c: u32| {
                let c = c as usize;
                if c > opts.col_indent {
                    c - opts.col_indent
                } else {
                    c
                }
            };
            let start_col = strip_col(span.start_col);
            if span.start_line == span.end_line && span.end_col != span.start_col {
                format!(
                    "{disp_label}:{disp_line}:{start_col}-{}: {}:",
                    strip_col(span.end_col),
                    d.severity
                )
            } else {
                format!("{disp_label}:{disp_line}:{start_col}: {}:", d.severity)
            }
        }
        Some(span) => format!(
            "{}:{}:{}: {}:",
            span.file, span.start_line, span.start_col, d.severity
        ),
        None => format!("{}:", d.severity),
    };

    let scrubbed_message = drop_scaffold_relevant_binds(&d.message);

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
    // straight out of `opts.source`, which is the assembled/candidate module.
    if let Some(span) = &d.span {
        if span.start_line == span.end_line {
            if let Some(src_line) = opts.source.lines().nth(span.start_line as usize - 1) {
                let gutter = format!("{}", span.start_line);
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
    out
}

/// Binder names generated by the repl/eval scaffold wrapper (`__user = let {
/// __b = <user expr> } in __b`, `result = do { _r <- __user; paginateResult N
/// (toJSON _r) }`, plus the `:t`/probe helpers' `__t`/`__probe`, and the
/// budget-tracking `_scV`/`_sayC`). A "Relevant bindings include" entry naming
/// one of these is wrapper plumbing, not the user's own code.
const SCAFFOLD_BINDERS: &[&str] = &[
    "__user", "__b", "it", "__t", "__probe", "result", "_r", "_scV", "_sayC",
];

/// Drop scaffold-named entries from a `Relevant bindings include` list INSIDE
/// one diagnostic's own message text. This is narrower than the whole-
/// diagnostic span partition `render_diagnostics` does above: a diagnostic can
/// legitimately survive that partition (its span IS on the user's own line)
/// while GHC's own explanation for it still cites a scaffold binder — e.g. an
/// ambiguous type variable arising from `pure` inside the `__b = pure (...)`
/// wrapper binding, whose "Relevant bindings include __b :: ..." entry is
/// GHC's own text, not a separate droppable diagnostic. `GhcPipeline`'s
/// `maxRelevantBinds = Just 0` suppresses most of these at the source but GHC
/// can still print a residual single entry alongside a "(Some bindings
/// suppressed …)" footer — this mops that up. If dropping scaffold entries
/// empties the region, the header and footer go too (an empty list is worse
/// noise than no list). User-named bindings in the same region are untouched.
#[must_use]
fn drop_scaffold_relevant_binds(message: &str) -> String {
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
            let binder = t.split_once(" :: ").map(|(name, _)| name.trim());
            if binder.is_some_and(|n| SCAFFOLD_BINDERS.contains(&n)) {
                // dropped
            } else {
                all_dropped = false;
                region_out.push(l.to_string());
            }
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
    const NEEDLE: &str = "-- [user-lines] ";
    let pos = source.find(NEEDLE)?;
    let rest = &source[pos + NEEDLE.len()..];
    let range: &str = rest.lines().next()?;
    let (start_s, end_s) = range.split_once(':')?;
    let start = start_s.trim().parse::<usize>().ok()?;
    let end = end_s.trim().parse::<usize>().ok()?;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(file: &str, sl: u32, sc: u32, el: u32, ec: u32, sev: &str, msg: &str) -> ExtractDiag {
        ExtractDiag {
            span: Some(DiagSpan {
                file: file.to_string(),
                start_line: sl,
                start_col: sc,
                end_line: el,
                end_col: ec,
            }),
            severity: sev.to_string(),
            message: msg.to_string(),
        }
    }

    // ---- parse_diag_report ----

    #[test]
    fn parse_clean_success_report_round_trip() {
        let stdout = br#"{"version":1,"diagnostics":[]}"#;
        let report = parse_diag_report(stdout, b"").unwrap();
        assert_eq!(report.version, 1);
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn parse_single_error_report() {
        let stdout = br#"{"version":1,"diagnostics":[{"span":{"file":"Bad.hs","startLine":3,"startCol":7,"endLine":3,"endCol":14},"severity":"error","message":"Variable not in scope: garbage"}]}"#;
        let report = parse_diag_report(stdout, b"").unwrap();
        assert_eq!(report.diagnostics.len(), 1);
        let d = &report.diagnostics[0];
        assert_eq!(d.severity, "error");
        let span = d.span.as_ref().unwrap();
        assert_eq!(span.file, "Bad.hs");
        assert_eq!(span.start_line, 3);
    }

    #[test]
    fn parse_null_span_diagnostic() {
        let stdout =
            br#"{"version":1,"diagnostics":[{"span":null,"severity":"error","message":"boom"}]}"#;
        let report = parse_diag_report(stdout, b"").unwrap();
        assert!(report.diagnostics[0].span.is_none());
    }

    #[test]
    fn malformed_stdout_produces_clear_error() {
        let err = parse_diag_report(b"not json", b"some stderr").unwrap_err();
        assert!(err.contains("did not parse"), "{err}");
        assert!(err.contains("stderr tail"), "{err}");
    }

    #[test]
    fn wrong_version_produces_clear_error() {
        let stdout = br#"{"version":99,"diagnostics":[]}"#;
        let err = parse_diag_report(stdout, b"").unwrap_err();
        assert!(err.contains("version 99"), "{err}");
        assert!(err.contains("expects 1"), "{err}");
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
            "error",
            "No instance for HasField",
        );
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some((33, 40)),
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
        let in_range = diag("Expr.hs", 2, 10, 2, 10, "error", "Ambiguous type variable");
        let fallout = diag(
            "Expr.hs",
            9,
            5,
            9,
            5,
            "error",
            "Overlapping instances for ToWire",
        );
        let got = render_diagnostics(
            &[in_range, fallout],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some((2, 3)),
                line_offset: 0,
                col_indent: 0,
                drop_foreign_gen_warnings_except: None,
                source,
            },
        );
        assert!(got.contains("Ambiguous type variable"), "{got}");
        assert!(!got.contains("Overlapping instances"), "{got}");
        assert!(
            got.contains("1 further error(s) suppressed: fallout in the result-display wrapper"),
            "{got}"
        );
    }

    #[test]
    fn would_drop_everything_keeps_everything() {
        let source = "line1\n";
        let only_fallout = diag(
            "Expr.hs",
            9,
            5,
            9,
            5,
            "error",
            "Overlapping instances for ToWire",
        );
        let got = render_diagnostics(
            &[only_fallout],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some((2, 3)),
                line_offset: 0,
                col_indent: 0,
                drop_foreign_gen_warnings_except: None,
                source,
            },
        );
        assert!(got.contains("Overlapping instances"), "{got}");
        assert!(!got.contains("suppressed"), "{got}");
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
            severity: "warning".into(),
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
            severity: "warning".into(),
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
        let d = diag("SomeExpr.hs", 3, 1, 3, 1, "error", "whatever");
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

    /// Found live 2026-07-08 through the real repl bind path: an ambiguous
    /// `pure` diagnostic that legitimately survives the span partition (it's
    /// on the user's own line) still carries GHC's own "Relevant bindings
    /// include __b :: ..." text, since `maxRelevantBinds = Just 0` only
    /// minimizes (not eliminates) the list — this is INSIDE a kept
    /// diagnostic's message, not a separate droppable diagnostic.
    #[test]
    fn scaffold_relevant_binds_scrubbed_from_a_surviving_diagnostic() {
        let source = "line1\n__b = pure (toWire x, x.files)\n";
        let d = diag(
            "Expr.hs",
            2,
            1,
            2,
            5,
            "error",
            "* Ambiguous type variable `f0' arising from a use of `pure'\n\
             Relevant bindings include\n  __b :: f0 (Value, b0) (bound at Expr.hs:2:2)\n  \
             (Some bindings suppressed; use -fmax-relevant-binds=N or -fno-max-relevant-binds)\n\
             Probable fix: use a type annotation",
        );
        let got = render_diagnostics(
            &[d],
            &RenderOpts {
                anchor: "Expr.hs",
                label: "<item>",
                user_lines: Some((2, 2)),
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
        let msg = "* Ambiguous\n  Relevant bindings include\n    c :: Commit (bound at Expr.hs:2:1)\n    __b :: f0 (bound at Expr.hs:2:2)\n";
        let got = drop_scaffold_relevant_binds(msg);
        assert!(!got.contains("__b ::"), "{got}");
        assert!(got.contains("c :: Commit"), "{got}");
        assert!(got.contains("Relevant bindings include"), "{got}");
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
}
