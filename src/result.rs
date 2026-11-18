//! Deliver (spec §2.5): merge, dedupe and gate the findings into `result.json` (spec §5, after
//! block/review `Result`, `dedupe` and `Gate` in https://github.com/block/review, commit
//! 0c13293, Apache-2.0, Copyright Block, Inc.; modified: pass durations in milliseconds and the
//! pass's agent instead of a Goose provider), and the human summary.

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
        min_confidence: 0.8,
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
    /// `p<=2,conf>=0.8`; a part left out keeps its default.
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

/// Merges findings at the same place with the same title (case and spacing ignored), keeping
/// the most urgent priority and the first one's other fields. Order is kept.
pub fn dedupe(findings: Vec<Finding>) -> Vec<Finding> {
    let mut out: Vec<Finding> = vec![];
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
    for f in findings {
        match out.iter_mut().find(|o| key(o) == key(&f)) {
            Some(o) => o.priority = o.priority.min(f.priority),
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
        }
    }

    #[test]
    fn gate_parses_and_filters() {
        assert_eq!("p<=2,conf>=0.8".parse::<Gate>().unwrap(), Gate::DEFAULT);
        let g: Gate = "p<=1".parse().unwrap();
        assert_eq!((g.max_priority, g.min_confidence), (1, 0.8));
        let g: Gate = " conf>=0.5 ".parse().unwrap();
        assert_eq!((g.max_priority, g.min_confidence), (2, 0.5));
        for bad in ["p<=4", "conf>=1.1", "p<2", "", "p<=-1", "conf>=x"] {
            assert!(bad.parse::<Gate>().is_err(), "{bad}");
        }
        assert!(Gate::DEFAULT.keep(&f("t", 1, 2, 0.8)));
        assert!(!Gate::DEFAULT.keep(&f("t", 1, 3, 0.9)));
        assert!(!Gate::DEFAULT.keep(&f("t", 1, 0, 0.79)));
        assert!(Gate::NONE.keep(&f("t", 1, 3, 0.0)));
    }

    #[test]
    fn dedupe_merges_same_place_and_title_keeping_the_most_urgent() {
        let out = dedupe(vec![
            f("Fix  It", 1, 2, 0.9),
            f("other", 1, 3, 0.9),
            f("fix it", 1, 1, 0.5),
            f("fix it", 2, 3, 0.9),
        ]);
        let got: Vec<_> = out
            .iter()
            .map(|f| (f.title.as_str(), f.start_line, f.priority))
            .collect();
        assert_eq!(got, [("Fix  It", 1, 1), ("other", 1, 3), ("fix it", 2, 3)]);
    }
}
