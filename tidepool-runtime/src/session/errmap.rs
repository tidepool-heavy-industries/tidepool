//! Remap GHC diagnostic coordinates that point into a GENERATED module so
//! errors speak the user's item-relative coordinates instead of elaboration
//! truth (tmp paths, preamble-shifted line numbers).
//!
//! The rewriter is anchored to the generated file's path suffix ONLY: a token
//! is rewritten iff its path component ends in `anchor` preceded by `/`,
//! whitespace, an opening delimiter, or line start. Foreign `.hs:L:C` tokens —
//! e.g. GHC panic backtraces quoting `compiler/GHC/Utils/Panic.hs` — pass
//! through untouched (the failure mode that got the first version of this
//! idea reverted; see notes/friction-ledger.md "Reverted").

/// Rewrite `<path ending in anchor>:<line>:<col>` diagnostic headers so lines
/// count from the user's first code line and the pseudo-path `label` replaces
/// the generated path. Gutter lines (`35 | code`) between a rewritten header
/// and the next blank line are renumbered with the same offset, width-padded
/// so the caret lines below stay aligned. `col_indent` is subtracted from
/// columns (the wrapper indents each user line by that much). Headers pointing
/// into the generated preamble (`line <= line_offset`) keep raw coordinates
/// but still lose the path prefix.
#[must_use]
pub fn remap_generated_coords(
    err: &str,
    anchor: &str,
    label: &str,
    line_offset: usize,
    col_indent: usize,
) -> String {
    let mut out: Vec<String> = Vec::new();
    // True between a DIAGNOSTIC-HEADER line (`…: error:` / `…: warning:`)
    // rewritten into user coordinates and the next blank line: gutter line
    // numbers in that region get the same offset. Inline coordinate mentions
    // (`(bound at <decl>:2:1)`, hole-fit provenance) are rewritten too but do
    // NOT touch the gutter state — a preamble-region provenance ref inside a
    // hole-fits list must not disarm renumbering of the snippet below it.
    let mut in_user_block = false;
    for line in err.lines() {
        let is_diag_header = line.contains(": error") || line.contains(": warning");
        if let Some((rewritten, user_region)) =
            rewrite_header(line, anchor, label, line_offset, col_indent)
        {
            if is_diag_header {
                in_user_block = user_region;
            }
            out.push(rewritten);
        } else if line.trim().is_empty() {
            in_user_block = false;
            out.push(line.to_string());
        } else if in_user_block {
            out.push(rewrite_gutter(line, line_offset));
        } else {
            out.push(line.to_string());
        }
    }
    let mut joined = out.join("\n");
    if err.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Rewrite the first `<path ending in anchor>:<coords>` token in `line`.
/// Returns `None` when the line carries no such token (foreign paths, code
/// echo, carets). The `bool` is true when the token landed in the user region
/// (line > offset) — gutter renumbering applies only then.
fn rewrite_header(
    line: &str,
    anchor: &str,
    label: &str,
    line_offset: usize,
    col_indent: usize,
) -> Option<(String, bool)> {
    let mut search = 0;
    while let Some(rel) = line[search..].find(anchor) {
        let start = search + rel;
        search = start + anchor.len();

        // Boundary check: the char before the anchor must be a path separator
        // (absorb the directory prefix), whitespace/delimiter, or line start —
        // otherwise the anchor is embedded in a longer name (`SomeExpr.hs`).
        let before = &line[..start];
        let prefix_start = match before.chars().next_back() {
            None => 0,
            Some('/') => before
                .rfind(|c: char| c.is_whitespace() || matches!(c, '(' | '"' | '\'' | '`'))
                .map_or(0, |i| i + 1),
            Some(c) if c.is_whitespace() || matches!(c, '(' | '"' | '\'' | '`') => start,
            Some(_) => continue,
        };

        let after = &line[start + anchor.len()..];
        let Some((consumed, token, user_region)) =
            rewrite_coords(after, anchor, label, line_offset, col_indent)
        else {
            continue;
        };
        let mut s = String::with_capacity(line.len());
        s.push_str(&line[..prefix_start]);
        s.push_str(&token);
        s.push_str(&line[start + anchor.len() + consumed..]);
        return Some((s, user_region));
    }
    None
}

/// Parse `:<line>:<col>[-<col2>]` right after the anchor and render the
/// remapped token. Returns `(chars consumed, token, in user region)`.
fn rewrite_coords(
    after: &str,
    anchor: &str,
    label: &str,
    line_offset: usize,
    col_indent: usize,
) -> Option<(usize, String, bool)> {
    let rest = after.strip_prefix(':')?;
    let l_digits = leading_digits(rest);
    if l_digits.is_empty() {
        return None;
    }
    let rest2 = rest[l_digits.len()..].strip_prefix(':')?;
    let c_digits = leading_digits(rest2);
    if c_digits.is_empty() {
        return None;
    }
    let mut consumed = 1 + l_digits.len() + 1 + c_digits.len();
    let span_end = rest2[c_digits.len()..].strip_prefix('-').and_then(|s| {
        let d = leading_digits(s);
        (!d.is_empty()).then(|| {
            consumed += 1 + d.len();
            d.parse::<usize>().unwrap_or(0)
        })
    });

    let l: usize = l_digits.parse().ok()?;
    let c: usize = c_digits.parse().ok()?;
    let strip_col = |c: usize| if c > col_indent { c - col_indent } else { c };

    let (token, user_region) = if l > line_offset {
        let mut t = format!("{label}:{}:{}", l - line_offset, strip_col(c));
        if let Some(c2) = span_end {
            t.push_str(&format!("-{}", strip_col(c2)));
        }
        (t, true)
    } else {
        // Generated-preamble region: keep raw coordinates (they point at
        // infrastructure, not user text) but drop the directory prefix.
        let mut t = format!("{anchor}:{l}:{c}");
        if let Some(c2) = span_end {
            t.push_str(&format!("-{c2}"));
        }
        (t, false)
    };
    Some((consumed, token, user_region))
}

/// Renumber a GHC source-snippet gutter line (`  35 | code`). The new number
/// is right-aligned to the old gutter width so the `   |` / caret lines
/// around it stay aligned. Non-gutter lines pass through.
fn rewrite_gutter(line: &str, line_offset: usize) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let rest = &line[indent_len..];
    let digits = leading_digits(rest);
    if digits.is_empty() || !rest[digits.len()..].starts_with(" |") {
        return line.to_string();
    }
    match digits.parse::<usize>() {
        Ok(n) if n > line_offset => {
            let width = indent_len + digits.len();
            format!("{:>width$}{}", n - line_offset, &rest[digits.len()..])
        }
        _ => line.to_string(),
    }
}

/// Collapse duplicate GHC diagnostic blocks in an extract stderr. The extract
/// emits most diagnostics TWICE: once via GHC's logger during `load` (unicode
/// quotes) and once via `show se` after the catch (ASCII quotes) — but parse
/// errors arrive ONLY via `show se`, so the extract must keep printing it and
/// the dedup lives here. Blocks are keyed on their alphanumeric content only,
/// so quote-style and whitespace differences between the two copies vanish.
#[must_use]
pub fn dedupe_diagnostics(err: &str) -> String {
    let blocks = split_diag_blocks(err);
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for block in blocks {
        let text = block.join("\n");
        // Key on header + message lines only: the logger copy carries source
        // gutter/caret snippet lines the `show se` copy lacks — including them
        // would make the two copies key differently and defeat the dedup.
        let key: String = block
            .iter()
            .filter(|l| {
                let t = l.trim_start();
                let digits = leading_digits(t);
                !(t.starts_with('|') || (!digits.is_empty() && t[digits.len()..].starts_with(" |")))
            })
            .flat_map(|l| l.chars())
            .filter(char::is_ascii_alphanumeric)
            .collect();
        // Containment, not equality: the `show se` copy can be a strict
        // SUBSET of the logger copy (no gutter, no "Suggested fix" lines —
        // found live on a not-in-scope error with a similar-name hint).
        if key.is_empty() || !seen.iter().any(|s| s.contains(key.as_str())) {
            if !key.is_empty() {
                seen.push(key);
            }
            out.push(text);
        }
    }
    let mut joined = out.join("\n");
    if err.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Split an extract stderr into diagnostic blocks (a block starts at a
/// `path.hs:L:C: error/warning` header or the `Compilation failed.` marker).
fn split_diag_blocks(err: &str) -> Vec<Vec<&str>> {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in err.lines() {
        let is_header = (line.contains(".hs:")
            && (line.contains(": error") || line.contains(": warning")))
            || line.trim() == "Compilation failed."
            || blocks.is_empty();
        if is_header {
            blocks.push(vec![line]);
        } else if let Some(last) = blocks.last_mut() {
            last.push(line);
        }
    }
    blocks
}

/// Drop WARNING blocks that cite a `Tidepool/Session/Lib/G<k>.hs` generation
/// module other than `keep_rel` (None ⇒ drop them all). GHC re-emits
/// dependency-module warnings on every downstream recompile, so a gen-25
/// `-Wx-partial` warning otherwise rides EVERY later item's error output
/// (noise-leak, friction #26). Errors are never dropped.
#[must_use]
pub fn drop_foreign_gen_warnings(err: &str, keep_rel: Option<&str>) -> String {
    let kept: Vec<String> = split_diag_blocks(err)
        .into_iter()
        .filter(|block| {
            let header = block.first().copied().unwrap_or("");
            let is_gen_warning = header.contains(": warning")
                && header.contains("Tidepool/Session/Lib/G")
                && keep_rel.is_none_or(|k| !header.contains(k));
            !is_gen_warning
        })
        .map(|b| {
            b.join(
                "
",
            )
        })
        .collect();
    let mut joined = kept.join("\n");
    if err.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn leading_digits(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(i, _)| i);
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_plane_header_and_gutter() {
        let err = "/tmp/.tmpIZMs8T/Expr.hs:35:8: error: [GHC-39999]\n    \
                   * No instance for HasField\n   |\n35 | ageDays now c = _\n   |        ^\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        assert!(got.starts_with("<item>:2:6: error: [GHC-39999]"), "{got}");
        assert!(got.contains("\n 2 | ageDays now c = _"), "{got}");
        // caret line untouched, alignment preserved by width padding
        assert!(got.contains("\n   |        ^"), "{got}");
    }

    #[test]
    fn foreign_paths_pass_through() {
        let err = "panic! at compiler/GHC/Utils/Panic.hs:23:1 in ghc:GHC.Utils.Panic\n";
        assert_eq!(remap_generated_coords(err, "Expr.hs", "<item>", 33, 2), err);
    }

    #[test]
    fn embedded_suffix_is_not_ours() {
        let err = "SomeExpr.hs:3:1: error: whatever\n";
        assert_eq!(remap_generated_coords(err, "Expr.hs", "<item>", 33, 2), err);
    }

    #[test]
    fn preamble_region_keeps_raw_coords_but_strips_dir() {
        let err = "/tmp/.tmpX/Expr.hs:10:5: error: bad import\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        assert_eq!(got, "Expr.hs:10:5: error: bad import\n");
    }

    #[test]
    fn column_span_is_remapped() {
        let err = "/tmp/.tmpX/Expr.hs:35:8-12: error: thing\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        assert_eq!(got, "<item>:2:6-10: error: thing\n");
    }

    #[test]
    fn decl_plane_absolute_path() {
        let err = "/tmp/tidepool-repl-4178942/session-1/Tidepool/Session/Lib/G2.hs:29:17: error: [GHC-88464]\n";
        let got = remap_generated_coords(err, "Tidepool/Session/Lib/G2.hs", "<decl>", 27, 0);
        assert_eq!(got, "<decl>:2:17: error: [GHC-88464]\n");
    }

    #[test]
    fn inline_preamble_ref_does_not_disarm_gutter() {
        // Hole-fits provenance cites a preamble-region location mid-block;
        // the snippet gutter after it must still renumber (found live: the
        // `pick = _` fits list left `28 |` raw).
        let err = "/tmp/x/Expr.hs:28:8: error: [GHC-88464]\n    Found hole\n      Valid hole fits include\n        (imported at Expr.hs:21:1-29)\n   |\n28 | pick = _\n   |        ^\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 26, 0);
        assert!(got.contains("<item>:2:8: error"), "{got}");
        assert!(got.contains("\n 2 | pick = _"), "{got}");
    }

    #[test]
    fn gutter_resets_at_blank_line() {
        let err = "/tmp/.tmpX/Expr.hs:35:8: error: a\n35 | x\n\n40 | unrelated\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        assert!(got.contains("\n 2 | x\n"), "{got}");
        assert!(got.contains("\n40 | unrelated"), "{got}");
    }

    #[test]
    fn foreign_gen_warnings_dropped_errors_kept() {
        let err = "Tidepool/Session/Lib/G25.hs:33:35: warning: [GHC-63394]\n    partial head\n\nTidepool/Session/Lib/G26.hs:2:1: error: [GHC-1]\n    real error\n\nCompilation failed.\n";
        let got = drop_foreign_gen_warnings(err, Some("Tidepool/Session/Lib/G26.hs"));
        assert!(!got.contains("partial head"), "{got}");
        assert!(got.contains("real error"), "{got}");
        // keep_rel=None drops all gen warnings
        let got2 = drop_foreign_gen_warnings(err, None);
        assert!(!got2.contains("partial head"), "{got2}");
        // current-gen WARNINGS survive when keep_rel matches
        let own = "Tidepool/Session/Lib/G26.hs:3:1: warning: [GHC-2]\n    user warning\n";
        assert!(
            drop_foreign_gen_warnings(own, Some("Tidepool/Session/Lib/G26.hs"))
                .contains("user warning")
        );
    }

    #[test]
    fn dedupe_collapses_logger_and_show_se_copies() {
        // Logger copy (unicode quotes) then the `show se` copy (ASCII quotes)
        // behind the marker — the second block must vanish, the marker stays.
        let err = "Expr.hs:3:1: error: [GHC-39999]\n    \u{2022} No instance for \u{2018}Foo\u{2019}\n\nCompilation failed.\nExpr.hs:3:1: error: [GHC-39999]\n    * No instance for `Foo'\n";
        let got = dedupe_diagnostics(err);
        assert_eq!(got.matches("No instance").count(), 1, "{got}");
        assert!(got.contains("Compilation failed."), "{got}");
    }

    #[test]
    fn dedupe_ignores_snippet_lines_in_key() {
        // The logger copy carries gutter/caret lines; `show se` doesn't. They
        // must still collapse (found live 2026-07-01: both copies survived).
        let err = "Expr.hs:1:52: error: [GHC-83865]\n    \u{2022} Couldn't match \u{2018}[Char]\u{2019} with \u{2018}Text\u{2019}\n   |\n 1 | pure (read x)\n   |       ^^^^\n\nExpr.hs:1:52: error: [GHC-83865]\n    * Couldn't match `[Char]' with `Text'\n";
        let got = dedupe_diagnostics(err);
        assert_eq!(got.matches("Couldn't match").count(), 1, "{got}");
        // the richer (gutter-bearing) first copy is the one kept
        assert!(got.contains("^^^^"), "{got}");
    }

    #[test]
    fn dedupe_drops_subset_copy_without_fix_lines() {
        // The show-se copy lacks the "Suggested fix" paragraph the logger
        // copy carries — containment keying still collapses it.
        let err = "Expr.hs:4:11: error: [GHC-88464]\n    Variable not in scope: (!!)\n    Suggested fix: use P.!!\n   |\n 4 | s !! n\n   |   ^^\n\nCompilation failed.\nExpr.hs:4:11: error: [GHC-88464]\n    Variable not in scope: (!!)\n";
        let got = dedupe_diagnostics(err);
        assert_eq!(got.matches("not in scope").count(), 1, "{got}");
        assert!(got.contains("Suggested fix"), "{got}");
    }

    #[test]
    fn dedupe_keeps_parse_error_only_copy() {
        // Parse errors are printed ONLY by `show se` — nothing to collapse.
        let err = "Compilation failed.\nExpr.hs:41:5: error: [GHC-58481]\n    parse error on input `let'\n";
        assert_eq!(dedupe_diagnostics(err), err);
    }

    #[test]
    fn column_inside_indent_is_clamped() {
        let err = "/tmp/.tmpX/Expr.hs:35:1: error: a\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        // col 1 <= indent 2: kept raw rather than going to 0/negative
        assert_eq!(got, "<item>:2:1: error: a\n");
    }
}
