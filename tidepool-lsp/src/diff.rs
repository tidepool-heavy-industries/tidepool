//! Minimal line-level unified-diff renderer.
//!
//! Produces standard `--- a/… / +++ b/… / @@ -l,s +l,s @@` output with 3 lines
//! of context, suitable for tidepool's `applyDiff`. Backed by the `similar`
//! crate's Myers diff — linear in the edit distance rather than a hand-rolled
//! O(N*M) LCS table, so there is no separate large-file fallback to maintain.

use similar::TextDiff;

const CONTEXT: usize = 3;

/// Render a unified diff for one file. `rel` is the path shown in the headers.
/// Returns an empty string when `old == new`.
pub fn unified(rel: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(CONTEXT)
        .header(&format!("a/{rel}"), &format!("b/{rel}"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_change() {
        let d = unified("f.rs", "a\nb\nc\n", "a\nB\nc\n");
        assert!(d.contains("--- a/f.rs"));
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
        assert!(d.contains("@@ -1,3 +1,3 @@"));
    }

    #[test]
    fn no_change_is_empty() {
        assert_eq!(unified("f.rs", "x\n", "x\n"), "");
    }

    #[test]
    fn insertion() {
        let d = unified("f.rs", "a\nc\n", "a\nb\nc\n");
        assert!(d.contains("+b"));
        assert!(d.contains(" a"));
        assert!(d.contains(" c"));
    }

    /// Matches the old hand-rolled LCS renderer's output on every ordinary
    /// fixture (single change, insertion, deletion, multi-hunk,
    /// adjacent-hunk coalescing, unchanged content, pure append). See
    /// `divergent_fixtures_are_the_old_bugs_fixed` for the two fixtures that
    /// differ, and why.
    #[test]
    fn golden_matches_hand_rolled_lcs_on_ordinary_fixtures() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "single_change",
                "a\nb\nc\n",
                "a\nB\nc\n",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n",
            ),
            (
                "insertion",
                "a\nc\n",
                "a\nb\nc\n",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +1,3 @@\n a\n+b\n c\n",
            ),
            (
                "deletion",
                "a\nb\nc\n",
                "a\nc\n",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,2 @@\n a\n-b\n c\n",
            ),
            (
                "unchanged",
                "same\ncontent\nhere\n",
                "same\ncontent\nhere\n",
                "",
            ),
            (
                "pure_append",
                "line1\nline2\nline3\n",
                "line1\nline2changed\nline3\nline4\n",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,4 @@\n line1\n-line2\n+line2changed\n line3\n+line4\n",
            ),
            (
                "two_hunks_far_apart",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n",
                "1\n2\n3\nCHANGED\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\nCHANGED2\n20\n",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1,7 +1,7 @@\n 1\n 2\n 3\n-4\n+CHANGED\n 5\n 6\n 7\n@@ -16,5 +16,5 @@\n 16\n 17\n 18\n-19\n+CHANGED2\n 20\n",
            ),
            (
                "adjacent_changes_coalesce",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n",
                "1\n2\nX\n4\n5\nY\n7\n8\n9\n10\n",
                "--- a/f.rs\n+++ b/f.rs\n@@ -1,9 +1,9 @@\n 1\n 2\n-3\n+X\n 4\n 5\n-6\n+Y\n 7\n 8\n 9\n",
            ),
        ];
        for (name, old, new, expected) in cases {
            assert_eq!(unified("f.rs", old, new), *expected, "case: {name}");
        }
    }

    /// Two fixtures pinned as the correct, standards-conformant behavior
    /// (rather than the naive 1-based-everywhere rendering a hand-rolled
    /// differ would produce):
    ///
    /// - File creation (empty `old`): hunk header is `@@ -0,0 +1,2 @@`, the
    ///   GNU diff/patch convention for an empty original file.
    /// - No trailing newline: emits the POSIX `\ No newline at end of file`
    ///   marker rather than silently treating the last line as
    ///   newline-terminated (which would round-trip a fabricated newline).
    #[test]
    fn divergent_fixtures_are_the_old_bugs_fixed() {
        assert_eq!(
            unified("f.rs", "", "hello\nworld\n"),
            "--- a/f.rs\n+++ b/f.rs\n@@ -0,0 +1,2 @@\n+hello\n+world\n",
            "empty-old-file hunk header should be the standard -0,0, not the old -1,0"
        );
        assert_eq!(
            unified("f.rs", "hello\nworld\n", ""),
            "--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +0,0 @@\n-hello\n-world\n"
        );
        assert_eq!(
            unified("f.rs", "no newline at end", "no newline at end changed"),
            "--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-no newline at end\n\\ No newline at end of file\n+no newline at end changed\n\\ No newline at end of file\n",
            "a missing trailing newline must be marked, not silently dropped"
        );
    }
}
