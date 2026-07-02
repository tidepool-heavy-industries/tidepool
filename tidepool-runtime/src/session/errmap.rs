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
    // True between a header rewritten into user coordinates and the next blank
    // line: gutter line numbers in that region get the same offset.
    let mut in_user_block = false;
    for line in err.lines() {
        if let Some((rewritten, user_region)) =
            rewrite_header(line, anchor, label, line_offset, col_indent)
        {
            in_user_block = user_region;
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
        assert_eq!(
            remap_generated_coords(err, "Expr.hs", "<item>", 33, 2),
            err
        );
    }

    #[test]
    fn embedded_suffix_is_not_ours() {
        let err = "SomeExpr.hs:3:1: error: whatever\n";
        assert_eq!(
            remap_generated_coords(err, "Expr.hs", "<item>", 33, 2),
            err
        );
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
        let got = remap_generated_coords(
            err,
            "Tidepool/Session/Lib/G2.hs",
            "<decl>",
            27,
            0,
        );
        assert_eq!(got, "<decl>:2:17: error: [GHC-88464]\n");
    }

    #[test]
    fn gutter_resets_at_blank_line() {
        let err = "/tmp/.tmpX/Expr.hs:35:8: error: a\n35 | x\n\n40 | unrelated\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        assert!(got.contains("\n 2 | x\n"), "{got}");
        assert!(got.contains("\n40 | unrelated"), "{got}");
    }

    #[test]
    fn column_inside_indent_is_clamped() {
        let err = "/tmp/.tmpX/Expr.hs:35:1: error: a\n";
        let got = remap_generated_coords(err, "Expr.hs", "<item>", 33, 2);
        // col 1 <= indent 2: kept raw rather than going to 0/negative
        assert_eq!(got, "<item>:2:1: error: a\n");
    }
}
