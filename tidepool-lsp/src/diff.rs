//! Minimal line-level unified-diff renderer.
//!
//! Produces standard `--- a/… / +++ b/… / @@ -l,s +l,s @@` output with 3 lines
//! of context, suitable for tidepool's `applyDiff`. Uses an LCS table; for very
//! large files it falls back to a single whole-file-replacement hunk rather
//! than allocating a quadratic table.

const CONTEXT: usize = 3;
const LCS_CELL_BUDGET: usize = 25_000_000; // ~100MB of i32; above this, fall back

/// Render a unified diff for one file. `rel` is the path shown in the headers.
/// Returns an empty string when `old == new`.
pub fn unified(rel: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let old_lines: Vec<&str> = split_lines(old);
    let new_lines: Vec<&str> = split_lines(new);

    let ops = if old_lines.len().saturating_mul(new_lines.len()) > LCS_CELL_BUDGET {
        // Fall back: replace the whole file in one hunk.
        let mut ops = Vec::with_capacity(old_lines.len() + new_lines.len());
        ops.extend(old_lines.iter().map(|l| Op::Del(l.to_string())));
        ops.extend(new_lines.iter().map(|l| Op::Ins(l.to_string())));
        ops
    } else {
        diff_ops(&old_lines, &new_lines)
    };

    let hunks = group_hunks(&ops);
    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n+++ b/{}\n", rel, rel));
    for h in hunks {
        out.push_str(&h.render());
    }
    out
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    // strip a single trailing newline so we don't get a phantom empty line
    let body = s.strip_suffix('\n').unwrap_or(s);
    body.split('\n').collect()
}

#[derive(Clone)]
enum Op {
    Eq(String),
    Del(String),
    Ins(String),
}

/// LCS-based line diff producing an edit script.
fn diff_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let n = a.len();
    let m = b.len();
    // dp[i][j] = LCS length of a[i..] and b[j..]
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Eq(a[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Del(a[i].to_string()));
            i += 1;
        } else {
            ops.push(Op::Ins(b[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del(a[i].to_string()));
        i += 1;
    }
    while j < m {
        ops.push(Op::Ins(b[j].to_string()));
        j += 1;
    }
    ops
}

struct Hunk {
    old_start: usize, // 1-based
    new_start: usize,
    lines: Vec<String>, // each already prefixed with ' ', '-', or '+'
    old_count: usize,
    new_count: usize,
}

impl Hunk {
    fn render(&self) -> String {
        let mut s = format!(
            "@@ -{},{} +{},{} @@\n",
            self.old_start, self.old_count, self.new_start, self.new_count
        );
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

/// Walk the edit script and emit hunks, coalescing changes within `2*CONTEXT`
/// of each other and trimming surrounding equal lines to `CONTEXT`.
fn group_hunks(ops: &[Op]) -> Vec<Hunk> {
    // Index changed positions.
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Eq(_)))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    // Group changed indices into clusters separated by > 2*CONTEXT equal lines.
    let mut clusters: Vec<(usize, usize)> = Vec::new();
    let mut start = changed[0];
    let mut end = changed[0];
    for &c in &changed[1..] {
        if c - end <= 2 * CONTEXT + 1 {
            end = c;
        } else {
            clusters.push((start, end));
            start = c;
            end = c;
        }
    }
    clusters.push((start, end));

    let mut hunks = Vec::new();
    for (cs, ce) in clusters {
        let lo = cs.saturating_sub(CONTEXT);
        let hi = (ce + CONTEXT + 1).min(ops.len());

        let old_start = 1 + ops[..lo]
            .iter()
            .filter(|o| !matches!(o, Op::Ins(_)))
            .count();
        let new_start = 1 + ops[..lo]
            .iter()
            .filter(|o| !matches!(o, Op::Del(_)))
            .count();

        let mut lines = Vec::new();
        let mut old_count = 0;
        let mut new_count = 0;
        for op in &ops[lo..hi] {
            match op {
                Op::Eq(t) => {
                    lines.push(format!(" {}", t));
                    old_count += 1;
                    new_count += 1;
                }
                Op::Del(t) => {
                    lines.push(format!("-{}", t));
                    old_count += 1;
                }
                Op::Ins(t) => {
                    lines.push(format!("+{}", t));
                    new_count += 1;
                }
            }
        }
        hunks.push(Hunk {
            old_start,
            new_start,
            lines,
            old_count,
            new_count,
        });
    }
    hunks
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
}
