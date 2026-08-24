//! The fold: raw evidence rows in, a ranked desire-path report out.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::classify::{classify_error, DesireBucket};
use super::read::RunEvidence;

/// How many identifiers to keep per bucket in the report (most-frequent
/// first) — a bucket can name many distinct identifiers; only the head of
/// that ranking is reported, and the drop is stated, not silent (see
/// [`CompileFailureReport::to_human`]).
const TOP_IDENTIFIERS_PER_BUCKET: usize = 10;

#[derive(Debug, Clone)]
pub struct BucketReportRow {
    pub bucket: DesireBucket,
    /// Number of compile-failure rows classified into this bucket (one row
    /// = one failed round or one eval-failure log line — not one identifier
    /// occurrence, which can be higher when a round names the same missing
    /// thing more than once).
    pub count: usize,
    /// `(identifier, occurrence count)`, ranked highest-first, truncated to
    /// [`TOP_IDENTIFIERS_PER_BUCKET`].
    pub top_identifiers: Vec<(String, usize)>,
    /// How many distinct identifiers this bucket named in total, before
    /// truncation — so a report can say "and N more" honestly.
    pub distinct_identifiers: usize,
}

#[derive(Debug, Clone)]
pub struct RunTrendRow {
    pub run_label: String,
    /// Distinct holes (`site`s) serviced in this run.
    pub sites: usize,
    /// Holes whose round 1 compiled (`error == None`).
    pub first_try_successes: usize,
    /// `first_try_successes / sites`, `0.0` when `sites == 0`.
    pub rate: f64,
}

#[derive(Debug, Clone)]
pub struct CompileFailureReport {
    pub buckets: Vec<BucketReportRow>,
    pub trend: Vec<RunTrendRow>,
    pub total_rounds_seen: usize,
    pub total_failed_rounds: usize,
    pub total_eval_failures: usize,
}

/// Fold every run's evidence into one ranked report. Buckets are always
/// reported in [`DesireBucket::all`] order (zero-count buckets included, at
/// `count: 0`) — a ranked report should say "nothing reached for this" as
/// loudly as it says "reached for 40 times", so a reader doesn't mistake an
/// absent row for a bucket that was never checked.
pub fn build_report(runs: &[RunEvidence]) -> CompileFailureReport {
    let mut bucket_counts: HashMap<DesireBucket, usize> = HashMap::new();
    let mut bucket_idents: HashMap<DesireBucket, HashMap<String, usize>> = HashMap::new();
    let mut total_rounds_seen = 0usize;
    let mut total_failed_rounds = 0usize;
    let mut total_eval_failures = 0usize;
    let mut trend = Vec::with_capacity(runs.len());

    for run in runs {
        total_rounds_seen += run.answerer_rounds.len();

        // First-try-compile rate: group by site, look at round 1's error.
        let mut round1_by_site: HashMap<u32, bool> = HashMap::new();
        for row in &run.answerer_rounds {
            if row.round == 1 {
                round1_by_site.insert(row.site, row.error.is_none());
            }
            if let Some(err) = &row.error {
                total_failed_rounds += 1;
                let c = classify_error(err);
                *bucket_counts.entry(c.bucket).or_insert(0) += 1;
                let idents = bucket_idents.entry(c.bucket).or_default();
                for ident in c.identifiers {
                    *idents.entry(ident).or_insert(0) += 1;
                }
            }
        }
        let sites = round1_by_site.len();
        let first_try_successes = round1_by_site.values().filter(|ok| **ok).count();
        // A run with no hole-servicing rounds (a `log-*.jsonl`/journal file
        // matched by a broad glob, say) has no first-try-compile rate to
        // report — an omitted row, not a misleading "0/0 = 0.0%" data point.
        if sites > 0 {
            trend.push(RunTrendRow {
                run_label: run.run_label.clone(),
                sites,
                first_try_successes,
                rate: first_try_successes as f64 / sites as f64,
            });
        }

        for row in &run.eval_failures {
            total_eval_failures += 1;
            // Only a compile-phase failure carries GHC-shaped text worth
            // classifying into a desire-path bucket; a run-phase failure
            // (a Haskell `error`, resource exhaustion, …) is real evidence
            // but not a "construct reached for that doesn't exist yet".
            if row.phase != "compile" {
                continue;
            }
            let c = classify_error(&row.detail);
            *bucket_counts.entry(c.bucket).or_insert(0) += 1;
            let idents = bucket_idents.entry(c.bucket).or_default();
            for ident in c.identifiers {
                *idents.entry(ident).or_insert(0) += 1;
            }
        }
    }

    let buckets = DesireBucket::all()
        .into_iter()
        .map(|bucket| {
            let count = bucket_counts.get(&bucket).copied().unwrap_or(0);
            let mut idents: Vec<(String, usize)> = bucket_idents
                .get(&bucket)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect())
                .unwrap_or_default();
            idents.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let distinct_identifiers = idents.len();
            idents.truncate(TOP_IDENTIFIERS_PER_BUCKET);
            BucketReportRow {
                bucket,
                count,
                top_identifiers: idents,
                distinct_identifiers,
            }
        })
        .collect::<Vec<_>>();

    CompileFailureReport {
        buckets,
        trend,
        total_rounds_seen,
        total_failed_rounds,
        total_eval_failures,
    }
}

impl CompileFailureReport {
    /// Ranked-highest-first human table, plus the per-run trend.
    pub fn to_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "compile-failure report: {} rounds seen, {} failed, {} eval-surface failures\n\n",
            self.total_rounds_seen, self.total_failed_rounds, self.total_eval_failures
        ));

        let mut ranked = self.buckets.clone();
        ranked.sort_by(|a, b| b.count.cmp(&a.count));

        out.push_str(&format!("{:<32} {:>6}  identifiers\n", "bucket", "count"));
        out.push_str(&"-".repeat(70));
        out.push('\n');
        for row in &ranked {
            let idents = if row.top_identifiers.is_empty() {
                String::new()
            } else {
                let shown = row
                    .top_identifiers
                    .iter()
                    .map(|(name, n)| format!("{name} ({n})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let remaining = row
                    .distinct_identifiers
                    .saturating_sub(row.top_identifiers.len());
                if remaining > 0 {
                    format!("{shown}, and {remaining} more")
                } else {
                    shown
                }
            };
            out.push_str(&format!(
                "{:<32} {:>6}  {}\n",
                row.bucket.tag(),
                row.count,
                idents
            ));
        }

        if !self.trend.is_empty() {
            out.push_str("\nper-run first-try-compile rate\n");
            out.push_str(&"-".repeat(70));
            out.push('\n');
            for row in &self.trend {
                out.push_str(&format!(
                    "{:<40} {:>3}/{:<3}  {:>6.1}%\n",
                    row.run_label,
                    row.first_try_successes,
                    row.sites,
                    row.rate * 100.0
                ));
            }
        }

        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "total_rounds_seen": self.total_rounds_seen,
            "total_failed_rounds": self.total_failed_rounds,
            "total_eval_failures": self.total_eval_failures,
            "buckets": self.buckets.iter().map(|row| json!({
                "bucket": row.bucket.tag(),
                "count": row.count,
                "top_identifiers": row.top_identifiers.iter().map(|(name, n)| json!({
                    "identifier": name,
                    "count": n,
                })).collect::<Vec<_>>(),
                "distinct_identifiers": row.distinct_identifiers,
            })).collect::<Vec<_>>(),
            "trend": self.trend.iter().map(|row| json!({
                "run": row.run_label,
                "sites": row.sites,
                "first_try_successes": row.first_try_successes,
                "rate": row.rate,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::read::{AnswererRoundRow, EvalFailureRow};
    use super::*;

    fn run(label: &str, rounds: Vec<AnswererRoundRow>) -> RunEvidence {
        RunEvidence {
            run_label: label.to_string(),
            answerer_rounds: rounds,
            eval_failures: Vec::new(),
        }
    }

    #[test]
    fn buckets_report_zero_counts_for_untouched_buckets() {
        let report = build_report(&[]);
        assert_eq!(report.buckets.len(), 8);
        assert!(report.buckets.iter().all(|b| b.count == 0));
    }

    #[test]
    fn first_try_rate_counts_round_one_success_per_site() {
        let runs = vec![run(
            "r1",
            vec![
                AnswererRoundRow {
                    site: 0,
                    round: 1,
                    error: None,
                },
                AnswererRoundRow {
                    site: 1,
                    round: 1,
                    error: Some("Variable not in scope: fork".into()),
                },
                AnswererRoundRow {
                    site: 1,
                    round: 2,
                    error: None,
                },
            ],
        )];
        let report = build_report(&runs);
        assert_eq!(report.trend.len(), 1);
        assert_eq!(report.trend[0].sites, 2);
        assert_eq!(report.trend[0].first_try_successes, 1);
        assert!((report.trend[0].rate - 0.5).abs() < 1e-9);
        assert_eq!(report.total_failed_rounds, 1);
    }

    #[test]
    fn run_with_no_answerer_rounds_is_omitted_from_trend() {
        let runs = vec![run("log-only", Vec::new())];
        let report = build_report(&runs);
        assert!(
            report.trend.is_empty(),
            "a run with no hole-servicing rounds must not appear as a misleading 0/0 trend row"
        );
    }

    #[test]
    fn failed_rounds_classify_and_rank_identifiers() {
        let runs = vec![run(
            "r1",
            vec![
                AnswererRoundRow {
                    site: 0,
                    round: 1,
                    error: Some("Variable not in scope: fork".into()),
                },
                AnswererRoundRow {
                    site: 0,
                    round: 2,
                    error: Some("Variable not in scope: fork".into()),
                },
                AnswererRoundRow {
                    site: 1,
                    round: 1,
                    error: Some("Variable not in scope: forkAll".into()),
                },
            ],
        )];
        let report = build_report(&runs);
        let var_bucket = report
            .buckets
            .iter()
            .find(|b| b.bucket == DesireBucket::VariableNotInScope)
            .unwrap();
        assert_eq!(var_bucket.count, 3);
        assert_eq!(var_bucket.top_identifiers[0], ("fork".to_string(), 2));
        assert_eq!(var_bucket.top_identifiers[1], ("forkAll".to_string(), 1));
    }

    #[test]
    fn eval_failures_only_classify_compile_phase() {
        let runs = vec![RunEvidence {
            run_label: "eval".to_string(),
            answerer_rounds: Vec::new(),
            eval_failures: vec![
                EvalFailureRow {
                    op: "eval".into(),
                    class: "user-haskell".into(),
                    phase: "compile".into(),
                    detail: "Variable not in scope: bar".into(),
                },
                EvalFailureRow {
                    op: "eval".into(),
                    class: "runtime".into(),
                    phase: "run".into(),
                    detail: "Prelude.undefined".into(),
                },
            ],
        }];
        let report = build_report(&runs);
        assert_eq!(report.total_eval_failures, 2);
        let var_bucket = report
            .buckets
            .iter()
            .find(|b| b.bucket == DesireBucket::VariableNotInScope)
            .unwrap();
        assert_eq!(var_bucket.count, 1);
        let total_classified: usize = report.buckets.iter().map(|b| b.count).sum();
        assert_eq!(
            total_classified, 1,
            "the run-phase row must not be classified"
        );
    }

    #[test]
    fn human_render_notes_dropped_identifiers_beyond_top_n() {
        let mut rounds = Vec::new();
        for i in 0..15u32 {
            rounds.push(AnswererRoundRow {
                site: i,
                round: 1,
                error: Some(format!("Variable not in scope: v{i}")),
            });
        }
        let runs = vec![run("r1", rounds)];
        let report = build_report(&runs);
        let text = report.to_human();
        assert!(text.contains("more"), "expected a truncation note:\n{text}");
    }
}
