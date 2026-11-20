//! Deliver (spec §2.5): merge, dedupe and gate the findings into `result.json` (spec §5, after
//! block/review `Result`, `dedupe` and `Gate` in https://github.com/block/review, commit
//! 0c13293, Apache-2.0, Copyright Block, Inc.; modified: pass durations in milliseconds and the
//! pass's agent instead of a Goose provider), and the human summary.

use std::cmp::Reverse;
use std::fmt::Write;

use serde::{Deserialize, Serialize};

use crate::coordinator::{Dropped, Finding};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResult {
    /// The findings that passed the gate; `ungated_findings` is everything the review returned.
    pub findings: Vec<Finding>,
    pub ungated_findings: Vec<Finding>,
    pub dropped_candidates: Vec<Dropped>,
    pub overall_correctness: String,
    pub overall_explanation: String,
    pub review_process: String,
    pub base: String,
    pub head: String,
    pub passes: Vec<PassReport>,
    /// A panelist or the judge did not complete: the review ran without one of its
    /// independent passes.
    pub degraded: bool,
    /// `os`, or `external` when the reviewers ran without rreview's own sandbox (spec §3);
    /// a result written before this field existed was sandboxed.
    #[serde(default = "os")]
    pub sandbox: String,
}

fn os() -> String {
    "os".into()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassReport {
    pub role: String,
    pub agent: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// `completed` or `failed`.
    pub status: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Reports a finding at or above priority `max_priority` (0 is the most urgent) with at least
/// `min_confidence`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    pub max_priority: u8,
    pub min_confidence: f64,
}

impl Gate {
    pub const DEFAULT: Gate = Gate {
        max_priority: 2,
        // Calibration (docs/calibration.md): the coordinator scores even defects it confirmed
        // 0.5-0.72, so a 0.8 bar let through only judge restorations.
        min_confidence: 0.5,
    };
    pub const NONE: Gate = Gate {
        max_priority: 3,
        min_confidence: 0.0,
    };

    pub fn keep(&self, f: &Finding) -> bool {
        f.priority <= self.max_priority && f.confidence_score >= self.min_confidence
    }
}

impl std::str::FromStr for Gate {
    type Err = String;
    /// `p<=2,conf>=0.5`; a part left out keeps its default.
    fn from_str(s: &str) -> Result<Gate, String> {
        let mut g = Gate::DEFAULT;
        for part in s.split(',').map(str::trim) {
            if let Some(p) = part.strip_prefix("p<=") {
                g.max_priority = p
                    .parse()
                    .ok()
                    .filter(|p| *p <= 3)
                    .ok_or(format!("{part:?}: the priority is 0-3"))?;
            } else if let Some(c) = part.strip_prefix("conf>=") {
                g.min_confidence = c
                    .parse()
                    .ok()
                    .filter(|c| (0.0..=1.0).contains(c))
                    .ok_or(format!("{part:?}: the confidence is 0-1"))?;
            } else {
                return Err(format!("{part:?}: expected p<=N or conf>=X"));
            }
        }
        Ok(g)
    }
}

/// Merges findings at the same place with the same title (case and spacing ignored). The
/// whole finding that ranks highest wins: one that clears `gate` over one that does not, then
/// the most urgent priority, then the highest confidence, then the first. The merged finding
/// covers every duplicate's candidates. Order is kept.
pub fn dedupe(findings: Vec<Finding>, gate: Gate) -> Vec<Finding> {
    let key = |f: &Finding| {
        (
            f.file.clone(),
            f.start_line,
            f.title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase(),
        )
    };
    let rank = |f: &Finding| (gate.keep(f), Reverse(f.priority), f.confidence_score);
    let mut out: Vec<Finding> = vec![];
    for f in findings {
        match out.iter_mut().find(|o| key(o) == key(&f)) {
            Some(o) => {
                let mut ids = std::mem::take(&mut o.candidate_ids);
                if rank(&f) > rank(o) {
                    *o = f.clone();
                }
                ids.extend(f.candidate_ids);
                ids.retain({
                    let mut seen = std::collections::BTreeSet::new();
                    move |id| seen.insert(id.clone())
                });
                o.candidate_ids = ids;
            }
            None => out.push(f),
        }
    }
    out
}

/// The human summary of a result.
pub fn summary(r: &ReviewResult) -> String {
    let mut s = String::new();
    for f in &r.findings {
        let _ = writeln!(
            s,
            "[P{}] {} ({}:{}-{}, confidence {:.2}, {})\n  {}\n",
            f.priority,
            f.title,
            f.file,
            f.start_line,
            f.end_line,
            f.confidence_score,
            f.stage,
            f.body.replace('\n', "\n  ")
        );
    }
    let _ = writeln!(
        s,
        "{} gated findings ({} before the gate, {} dropped candidates). Verdict: {}. {}",
        r.findings.len(),
        r.ungated_findings.len(),
        r.dropped_candidates.len(),
        r.overall_correctness,
        r.overall_explanation.trim()
    );
    if r.degraded {
        let failed: Vec<_> = r
            .passes
            .iter()
            .filter(|p| p.status != "completed")
            .map(|p| p.role.as_str())
            .collect();
        let _ = writeln!(
            s,
            "DEGRADED: not every independent pass completed (failed: {}).",
            failed.join(", ")
        );
    }
    if r.sandbox != "os" {
        let _ = writeln!(
            s,
            "SANDBOX {}: the reviewers ran without rreview's own sandbox; only the container they ran in confined them.",
            r.sandbox.to_uppercase()
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(title: &str, line: u32, priority: u8, confidence: f64) -> Finding {
        Finding {
            title: title.into(),
            body: "b".into(),
            file: "a.rs".into(),
            start_line: line,
            end_line: line,
            priority,
            confidence_score: confidence,
            stage: "coordinator".into(),
            candidate_ids: vec![format!("c-{line}-{priority}")],
        }
    }

    #[test]
    fn gate_parses_and_filters() {
        assert_eq!("p<=2,conf>=0.5".parse::<Gate>().unwrap(), Gate::DEFAULT);
        let g: Gate = "p<=1".parse().unwrap();
        assert_eq!((g.max_priority, g.min_confidence), (1, 0.5));
        let g: Gate = " conf>=0.8 ".parse().unwrap();
        assert_eq!((g.max_priority, g.min_confidence), (2, 0.8));
        for bad in ["p<=4", "conf>=1.1", "p<2", "", "p<=-1", "conf>=x"] {
            assert!(bad.parse::<Gate>().is_err(), "{bad}");
        }
        assert!(Gate::DEFAULT.keep(&f("t", 1, 2, 0.5)));
        assert!(!Gate::DEFAULT.keep(&f("t", 1, 3, 0.9)));
        assert!(!Gate::DEFAULT.keep(&f("t", 1, 0, 0.49)));
        assert!(Gate::NONE.keep(&f("t", 1, 3, 0.0)));
    }

    #[test]
    fn dedupe_merges_same_place_and_title() {
        let out = dedupe(
            vec![
                f("Fix  It", 1, 2, 0.9),
                f("other", 1, 3, 0.9),
                f("fix it", 1, 1, 0.9),
                f("fix it", 2, 3, 0.9),
            ],
            Gate::DEFAULT,
        );
        let got: Vec<_> = out
            .iter()
            .map(|f| {
                (
                    f.title.as_str(),
                    f.start_line,
                    f.priority,
                    f.candidate_ids.join(","),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("fix it", 1, 1, "c-1-2,c-1-1".to_string()),
                ("other", 1, 3, "c-1-3".into()),
                ("fix it", 2, 3, "c-2-3".into())
            ]
        );
    }

    /// Only one duplicate clears the gate: that whole finding wins, so the merge never mixes
    /// one's priority with the other's confidence.
    #[test]
    fn the_duplicate_that_clears_the_gate_wins() {
        let gate: Gate = "p<=2,conf>=0.8".parse().unwrap();
        for findings in [
            vec![f("t", 1, 2, 0.70), f("t", 1, 1, 0.95)],
            vec![f("t", 1, 1, 0.95), f("t", 1, 2, 0.70)],
            // The more urgent one misses the gate; the other clears it.
            vec![f("t", 1, 1, 0.70), f("t", 1, 2, 0.95)],
        ] {
            let want = findings.iter().find(|f| gate.keep(f)).unwrap().clone();
            let out = dedupe(findings, gate);
            assert_eq!(out.len(), 1);
            assert!(gate.keep(&out[0]), "{out:?}");
            assert_eq!(
                (out[0].priority, out[0].confidence_score),
                (want.priority, want.confidence_score)
            );
            assert_eq!(out[0].candidate_ids.len(), 2);
        }
        // Neither clears it: the most urgent, then the most confident.
        let out = dedupe(
            vec![f("t", 1, 3, 0.9), f("t", 1, 2, 0.5), f("t", 1, 2, 0.6)],
            gate,
        );
        assert_eq!((out[0].priority, out[0].confidence_score), (2, 0.6));
    }
}
