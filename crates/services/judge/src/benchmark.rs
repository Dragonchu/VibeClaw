use std::path::Path;

use loopy_ipc::messages::DimensionScore;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct DimensionSpec {
    pub name: String,
    pub weight: f64,
    pub min_threshold: f64,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct BenchmarksConfig {
    pub version: String,
    pub dimensions: Vec<DimensionSpec>,
    pub overall_min: f64,
    pub regression_tolerance: f64,
}

impl BenchmarksConfig {
    pub fn load(constitution_dir: &Path) -> Result<Self, String> {
        let path = constitution_dir.join("benchmarks.json");
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
    }
}

pub struct BenchmarkScorer {
    config: BenchmarksConfig,
}

#[derive(Debug)]
pub struct ScoringResult {
    pub dimension_scores: Vec<DimensionScore>,
    pub overall_score: f64,
    pub any_below_threshold: bool,
    pub below_overall_min: bool,
}

impl BenchmarkScorer {
    pub fn new(config: BenchmarksConfig) -> Self {
        Self { config }
    }

    pub fn score(&self, raw_scores: &[(String, f64)]) -> ScoringResult {
        let mut dimension_scores = Vec::new();
        let mut weighted_sum = 0.0;
        let mut total_weight = 0.0;
        let mut any_below_threshold = false;

        for spec in &self.config.dimensions {
            let score = raw_scores
                .iter()
                .find(|(name, _)| name == &spec.name)
                .map(|(_, s)| *s)
                .unwrap_or(0.0);

            if score < spec.min_threshold {
                any_below_threshold = true;
            }

            weighted_sum += score * spec.weight;
            total_weight += spec.weight;

            dimension_scores.push(DimensionScore {
                name: spec.name.clone(),
                score,
                min_threshold: spec.min_threshold,
            });
        }

        let overall_score = if total_weight > 0.0 {
            weighted_sum / total_weight
        } else {
            0.0
        };

        let below_overall_min = overall_score < self.config.overall_min;

        ScoringResult {
            dimension_scores,
            overall_score,
            any_below_threshold,
            below_overall_min,
        }
    }

    pub fn is_regression(&self, new_overall: f64, old_overall: f64) -> bool {
        old_overall - new_overall > self.config.regression_tolerance
    }

    pub fn overall_min(&self) -> f64 {
        self.config.overall_min
    }

    pub fn regression_tolerance(&self) -> f64 {
        self.config.regression_tolerance
    }
}
