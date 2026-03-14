use loopy_ipc::messages::{DimensionScore, InvariantResult, TestResult, TestVerdict};

use crate::benchmark::ScoringResult;

pub fn build_test_result(
    version: &str,
    invariant_results: &[InvariantResult],
    scoring: &ScoringResult,
    old_overall: Option<f64>,
    regression_tolerance: f64,
) -> TestResult {
    let any_invariant_failed = invariant_results.iter().any(|r| !r.passed);

    let verdict = if any_invariant_failed {
        TestVerdict::HardFail
    } else if scoring.any_below_threshold || scoring.below_overall_min {
        TestVerdict::HardFail
    } else if let Some(old) = old_overall {
        if old - scoring.overall_score > regression_tolerance {
            TestVerdict::SoftFail
        } else {
            TestVerdict::Pass
        }
    } else {
        TestVerdict::Pass
    };

    let suggestion = match verdict {
        TestVerdict::HardFail => build_hard_fail_suggestion(invariant_results, scoring),
        TestVerdict::SoftFail => Some(format!(
            "Score regression detected ({:.2} → {:.2}, tolerance {:.2}). Version eligible for probation.",
            old_overall.unwrap_or(0.0),
            scoring.overall_score,
            regression_tolerance
        )),
        TestVerdict::Pass => None,
    };

    TestResult {
        version: version.to_string(),
        verdict,
        invariant_results: invariant_results.to_vec(),
        dimension_scores: scoring.dimension_scores.clone(),
        overall_score: scoring.overall_score,
        suggestion,
    }
}

fn build_hard_fail_suggestion(
    invariant_results: &[InvariantResult],
    scoring: &ScoringResult,
) -> Option<String> {
    let mut parts = Vec::new();

    let failed_tests: Vec<&InvariantResult> =
        invariant_results.iter().filter(|r| !r.passed).collect();
    if !failed_tests.is_empty() {
        let names: Vec<&str> = failed_tests.iter().map(|r| r.test_id.as_str()).collect();
        parts.push(format!("Failed invariant tests: [{}]", names.join(", ")));

        for test in &failed_tests {
            if let Some(detail) = &test.detail {
                parts.push(format!("  {}: {}", test.test_id, detail));
            }
        }
    }

    let below: Vec<&DimensionScore> = scoring
        .dimension_scores
        .iter()
        .filter(|d| d.score < d.min_threshold)
        .collect();
    if !below.is_empty() {
        for dim in &below {
            parts.push(format!(
                "{} score {:.2} below min threshold {:.2}",
                dim.name, dim.score, dim.min_threshold
            ));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}
